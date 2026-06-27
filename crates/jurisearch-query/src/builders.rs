//! Side-effect-free per-operation response builders (work/09 P3B). Each takes a validated input struct
//! plus a [`ReadSnapshot`] (and a [`QueryEmbedder`] for dense retrieval) and returns the response body.
//! A builder never resolves an `index_dir`, never starts Postgres, and never writes: every database read
//! goes through the snapshot. The CLI adapters (and P4's site service) call these after opening one
//! snapshot per request.
//!
//! Scope (3B): the cleanly-separable exposed operations — `fetch`, `context`, `related`, `compare`.
//! `search` and `cite` are snapshot-bound in the CLI adapter for 3B (their CLI-entangled response
//! shaping — intent routing, authority rerank, citation classification, online enrichment — moves here
//! with the service in P4). Online enrichment (`fetch --part`, `cite --online`) is adapter-side by
//! design (it is not a snapshot read).

use std::collections::{HashMap, HashSet};

use jurisearch_core::error::ErrorObject;
use jurisearch_storage::query::ReadSnapshot;
use jurisearch_storage::retrieval::{
    ContextDocumentsQuery, DecisionFilters, FetchDocumentsQuery, GroupBy, HybridCandidateQuery,
    RelatedQuery, RelatedRelation, RetrievalMode, RetrievalOptions, context_documents_in_snapshot,
    fetch_documents_in_snapshot, hybrid_candidates_in_snapshot, related_neighbours_in_snapshot,
};
use serde_json::{Value, json};

use crate::embedder::QueryEmbedder;
use crate::errors::{no_results, parse_storage_json, storage_error_object};

/// `fetch` input: the exact, version-pinned stable IDs to return.
pub struct FetchInput {
    pub document_ids: Vec<String>,
}

/// Build the `fetch` response body: exact source text for the requested IDs. The decision-part overlay
/// (`--part`, online) is layered on by the CLI adapter, not here.
pub fn build_fetch(
    input: &FetchInput,
    snapshot: &mut dyn ReadSnapshot,
) -> Result<Value, ErrorObject> {
    let ids = input
        .document_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let response =
        fetch_documents_in_snapshot(snapshot, &FetchDocumentsQuery { document_ids: &ids })
            .map_err(storage_error_object)?;
    let response = parse_storage_json(&response)?;
    if response["documents"]
        .as_array()
        .is_some_and(|documents| documents.is_empty())
    {
        return Err(no_results(
            "fetch returned no documents for the requested IDs",
        ));
    }
    Ok(response)
}

/// `context` input: a stable ID, optional as-of pin, and whether to include structural siblings.
pub struct ContextInput {
    pub document_id: String,
    pub as_of: Option<String>,
    pub include_siblings: bool,
}

/// Build the `context` response body: structural ancestry/siblings for one document.
pub fn build_context(
    input: &ContextInput,
    snapshot: &mut dyn ReadSnapshot,
) -> Result<Value, ErrorObject> {
    let response = context_documents_in_snapshot(
        snapshot,
        &ContextDocumentsQuery {
            document_id: input.document_id.as_str(),
            as_of: input.as_of.as_deref(),
            include_siblings: input.include_siblings,
        },
    )
    .map_err(storage_error_object)?;
    let response = parse_storage_json(&response)?;
    if response["target"].is_null() {
        Err(no_results(
            "context returned no valid document for the requested ID and --as-of date",
        ))
    } else {
        Ok(response)
    }
}

/// `related` input: a document id, the (already-validated) graph relation, and a result cap.
pub struct RelatedInput {
    pub document_id: String,
    pub rel: RelatedRelation,
    pub limit: u32,
}

/// Build the `related` response body: depth-1 graph neighbours with authority signals.
pub fn build_related(
    input: &RelatedInput,
    snapshot: &mut dyn ReadSnapshot,
) -> Result<Value, ErrorObject> {
    let response = related_neighbours_in_snapshot(
        snapshot,
        &RelatedQuery {
            document_id: input.document_id.as_str(),
            rel: input.rel,
            limit: input.limit,
        },
    )
    .map_err(storage_error_object)?;
    parse_storage_json(&response)
}

/// `compare` input: the raw query (echoed) + its tokenized lexical text, the per-mode top-k, the kind
/// filter, and the resolved as-of date. The adapter pre-tokenizes (boundary validation) so the builder
/// stays free of the CLI's query-text helper.
pub struct CompareInput {
    pub query: String,
    pub query_text: String,
    pub top_k: u32,
    pub kind_filter: Option<&'static str>,
    pub kind_label: &'static str,
    pub as_of: String,
}

/// Build the `compare` response body: run the SAME query through bm25/dense/hybrid (document grouping)
/// over ONE snapshot, then align per-mode top-k plus the pooled union with per-mode ranks and pairwise
/// overlap. Single-page (cross-mode pagination has no honest shared meaning).
pub fn build_compare(
    input: &CompareInput,
    snapshot: &mut dyn ReadSnapshot,
    embedder: &dyn QueryEmbedder,
) -> Result<Value, ErrorObject> {
    let pool_limit = input.top_k.saturating_mul(20);
    // Embed once (only the dense/hybrid arms use it); a bm25-only compare never reaches dense.
    let embedding = embedder.embed(input.query.as_str())?;

    let mut modes_out = serde_json::Map::new();
    let mut pool: Vec<Value> = Vec::new();
    let mut pool_index: HashMap<String, usize> = HashMap::new();
    let mut docs_by_mode: HashMap<&str, HashSet<String>> = HashMap::new();

    for mode in [
        RetrievalMode::Bm25,
        RetrievalMode::Dense,
        RetrievalMode::Hybrid,
    ] {
        let (query_embedding, fingerprint) = if mode.uses_dense() {
            (
                Some(embedding.literal.as_str()),
                Some(embedding.fingerprint.as_str()),
            )
        } else {
            (None, None)
        };
        let response = hybrid_candidates_in_snapshot(
            snapshot,
            &HybridCandidateQuery {
                query_text: input.query_text.as_str(),
                query_embedding,
                embedding_fingerprint: fingerprint,
                retrieval_mode: mode,
                group_by: GroupBy::Document,
                options: RetrievalOptions::default(),
                after_cursor: None,
                as_of: input.as_of.as_str(),
                kind_filter: input.kind_filter,
                project_authority: false,
                decision_filters: DecisionFilters::default(),
                lexical_limit: pool_limit,
                dense_limit: pool_limit,
                limit: input.top_k,
            },
        )
        .map_err(storage_error_object)?;
        let response = parse_storage_json(&response)?;
        let candidates = response["candidates"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mode_name = mode.as_str();
        let mut mode_docs = HashSet::new();
        for (rank, candidate) in candidates.iter().enumerate() {
            let Some(document_id) = candidate["document_id"].as_str() else {
                continue;
            };
            let document_id = document_id.to_owned();
            mode_docs.insert(document_id.clone());
            let index = *pool_index.entry(document_id.clone()).or_insert_with(|| {
                pool.push(json!({
                    "document_id": document_id,
                    "best_chunk_id": candidate.get("best_chunk_id").cloned().unwrap_or(Value::Null),
                    "citation": candidate.get("citation").cloned().unwrap_or(Value::Null),
                    "title": candidate.get("title").cloned().unwrap_or(Value::Null),
                    "by_mode": { "bm25": Value::Null, "dense": Value::Null, "hybrid": Value::Null }
                }));
                pool.len() - 1
            });
            pool[index]["by_mode"][mode_name] =
                json!({ "rank": rank + 1, "score": candidate["scores"]["rrf"].clone() });
        }
        docs_by_mode.insert(mode_name, mode_docs);
        modes_out.insert(mode_name.to_owned(), json!({ "candidates": candidates }));
    }

    // Pool ordered by best (minimum) rank across modes, then document_id — most relevant first.
    let best_rank = |entry: &Value| -> u64 {
        ["bm25", "dense", "hybrid"]
            .iter()
            .filter_map(|mode| entry["by_mode"][*mode]["rank"].as_u64())
            .min()
            .unwrap_or(u64::MAX)
    };
    pool.sort_by(|a, b| {
        best_rank(a)
            .cmp(&best_rank(b))
            .then_with(|| a["document_id"].as_str().cmp(&b["document_id"].as_str()))
    });

    let overlap = |left: &str, right: &str| -> usize {
        match (docs_by_mode.get(left), docs_by_mode.get(right)) {
            (Some(a), Some(b)) => a.intersection(b).count(),
            _ => 0,
        }
    };

    Ok(json!({
        "query": input.query,
        "as_of": input.as_of,
        "kind": input.kind_label,
        "group_by": "document",
        "top_k": input.top_k,
        "modes": Value::Object(modes_out),
        "pool": pool,
        "overlap": {
            "bm25_dense": overlap("bm25", "dense"),
            "bm25_hybrid": overlap("bm25", "hybrid"),
            "dense_hybrid": overlap("dense", "hybrid"),
        },
        "pagination": { "cursor_supported": false }
    }))
}
