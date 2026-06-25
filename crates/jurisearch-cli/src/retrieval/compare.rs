//! compare command: aligned bm25/dense/hybrid retriever comparison.

use crate::*;

/// Run the same query through bm25/dense/hybrid (document grouping) and return aligned per-mode
/// top-k plus the pooled union with per-mode ranks and pairwise overlap. Single-page (no cursor):
/// cross-mode pagination has no honest shared meaning.
pub(crate) fn compare_payload(req: CompareRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("compare requires a query"));
    }
    if req.top_k == 0 {
        return Err(ErrorObject::bad_input("compare --top-k must be at least 1"));
    }
    let kind: LegalKind = req.kind.into();
    let query_text = parade_query_text(&req.query).ok_or_else(|| {
        ErrorObject::bad_input("compare query must contain at least one searchable token")
    })?;
    let as_of = req.as_of.clone().unwrap_or_else(today_utc);
    let kind_filter = match kind {
        LegalKind::Code => Some("article"),
        LegalKind::Decision => Some("decision"),
        LegalKind::All => None,
    };
    let pool_limit = req.top_k.saturating_mul(20);

    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Search)?;
    let embedder = PreparedQueryEmbedder::from_env()?;
    let (embedding_literal, embedding_fingerprint) = embedder.embed(req.query.as_str())?;

    let mut modes_out = serde_json::Map::new();
    let mut pool: Vec<Value> = Vec::new();
    let mut pool_index: HashMap<String, usize> = HashMap::new();
    let mut docs_by_mode: HashMap<&str, HashSet<String>> = HashMap::new();

    for mode in [RetrievalMode::Bm25, RetrievalMode::Dense, RetrievalMode::Hybrid] {
        let (embedding, fingerprint) = if mode.uses_dense() {
            (
                Some(embedding_literal.as_str()),
                Some(embedding_fingerprint.as_str()),
            )
        } else {
            (None, None)
        };
        let response = hybrid_candidates_json(
            &postgres,
            &HybridCandidateQuery {
                query_text: &query_text,
                query_embedding: embedding,
                embedding_fingerprint: fingerprint,
                retrieval_mode: mode,
                group_by: GroupBy::Document,
                options: RetrievalOptions::default(),
                after_cursor: None,
                as_of: as_of.as_str(),
                kind_filter,
                decision_filters: DecisionFilters::default(),
                lexical_limit: pool_limit,
                dense_limit: pool_limit,
                limit: req.top_k,
            },
        )
        .map_err(storage_error_object)?;
        let response: Value = parse_storage_json(&response)?;
        let candidates = response["candidates"].as_array().cloned().unwrap_or_default();
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
        "query": req.query,
        "as_of": as_of,
        "kind": match kind {
            LegalKind::Code => "code",
            LegalKind::Decision => "decision",
            LegalKind::All => "all",
        },
        "group_by": "document",
        "top_k": req.top_k,
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

pub(crate) fn emit_compare(req: CompareRequest) -> anyhow::Result<()> {
    match compare_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}
