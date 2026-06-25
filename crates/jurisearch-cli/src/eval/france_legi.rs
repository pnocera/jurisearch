//! France-LEGI official benchmark (structured-citation + temporal floors, advisory semantic).

use crate::*;

/// Run the France-LEGI official-evidence benchmark over the production pipeline and assemble a
/// `phase1_france_legi_benchmark` artifact. Opens the index ONCE and runs every qrel through
/// `search_with_postgres` (single Postgres lifecycle). Gold comes from `france_legi_gold_json`
/// (no archive re-parse, no human/LLM).
pub(crate) fn eval_france_legi_payload(
    args: EvalFranceLegiArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    // Verify query readiness ONCE for the whole sweep (the index is static during the run), so the
    // per-query searches can skip the expensive coverage re-count. The runner uses hybrid search,
    // which needs the dense `Search` readiness gate.
    ensure_query_readiness(&postgres, QueryReadinessGate::Search)?;

    let limits = FranceLegiGoldLimits {
        known_item: args.known_item,
        temporal: args.temporal,
        cross_reference: args.cross_reference,
    };
    let gold_json = france_legi_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    // Fixed at top-10 (document-level): the gate validates @10, so the runner must measure @10.
    let top_k = FRANCE_LEGI_GATE_TOP_K as usize;
    let overfetch = FRANCE_LEGI_GATE_TOP_K.saturating_mul(4);

    // Build the query embedder once for the whole sweep (the runner always uses hybrid/dense).
    let embedder = PreparedQueryEmbedder::from_env()?;

    // Each category runs its gold qrels through the production search pipeline and records which
    // routing backend served each query (the gate audit). known-item -> structured_citation_resolution
    // and temporal -> temporal_version_pinning resolve structurally; cross-reference is the advisory
    // semantic stress test (full body -> cited article, via hybrid).
    let mut known_hits = 0usize;
    let mut known_done = 0usize;
    let mut known_backends = std::collections::BTreeMap::<String, usize>::new();
    for qrel in gold["known_item"].as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id), Some(as_of)) = (
            qrel["query"].as_str(),
            qrel["gold_document_id"].as_str(),
            qrel["as_of"].as_str(),
        ) else {
            continue;
        };
        let (docs, backend) =
            france_legi_search_documents(&postgres, &embedder, query, as_of, overfetch)?;
        *known_backends.entry(backend).or_default() += 1;
        known_done += 1;
        if docs.iter().take(top_k).any(|doc| doc == gold_id) {
            known_hits += 1;
        }
    }

    let mut temporal_hits = 0usize;
    let mut temporal_done = 0usize;
    let mut temporal_backends = std::collections::BTreeMap::<String, usize>::new();
    for qrel in gold["temporal"].as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id), Some(as_of)) = (
            qrel["query"].as_str(),
            qrel["gold_document_id"].as_str(),
            qrel["as_of"].as_str(),
        ) else {
            continue;
        };
        let (docs, backend) =
            france_legi_search_documents(&postgres, &embedder, query, as_of, overfetch)?;
        *temporal_backends.entry(backend).or_default() += 1;
        temporal_done += 1;
        if docs.iter().take(top_k).any(|doc| doc == gold_id) {
            temporal_hits += 1;
        }
    }

    // cross-reference (advisory semantic): production search applies a temporal prefilter, so match
    // the cited ARTICLE (any version, by source_uid) rather than the exact cited version; as_of =
    // the citing article's own date.
    let mut cross_recall_sum = 0.0f64;
    let mut cross_done = 0usize;
    let mut cross_backends = std::collections::BTreeMap::<String, usize>::new();
    for qrel in gold["cross_reference"].as_array().into_iter().flatten() {
        let (Some(query), Some(query_doc), Some(gold_ids)) = (
            qrel["query"].as_str(),
            qrel["query_document_id"].as_str(),
            qrel["gold_document_ids"].as_array(),
        ) else {
            continue;
        };
        let gold_uids: Vec<String> = gold_ids
            .iter()
            .filter_map(|value| value.as_str().and_then(legi_source_uid_of).map(str::to_owned))
            .collect();
        if gold_uids.is_empty() {
            continue;
        }
        let as_of = legi_document_as_of(query_doc)
            .map(str::to_owned)
            .unwrap_or_else(today_utc);
        let (docs, backend) =
            france_legi_search_documents(&postgres, &embedder, query, &as_of, overfetch)?;
        *cross_backends.entry(backend).or_default() += 1;
        let top_uids: std::collections::HashSet<&str> = docs
            .iter()
            .take(top_k)
            .filter_map(|doc| legi_source_uid_of(doc))
            .collect();
        let matched = gold_uids
            .iter()
            .filter(|uid| top_uids.contains(uid.as_str()))
            .count();
        cross_recall_sum += matched as f64 / gold_uids.len() as f64;
        cross_done += 1;
    }

    let structured = FranceLegiCategoryResult {
        metric: mean(known_hits, known_done),
        queries: known_done,
        backends: json!(known_backends),
    };
    let temporal = FranceLegiCategoryResult {
        metric: mean(temporal_hits, temporal_done),
        queries: temporal_done,
        backends: json!(temporal_backends),
    };
    let semantic = FranceLegiCategoryResult {
        metric: if cross_done > 0 {
            cross_recall_sum / cross_done as f64
        } else {
            0.0
        },
        queries: cross_done,
        backends: json!(cross_backends),
    };

    let index_revision = index_dir
        .as_path()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(france_legi_artifact(
        structured,
        temporal,
        semantic,
        limits,
        &index_revision,
        &source_revision,
    ))
}

/// One France-LEGI gate category: the @10 metric over its qrels, the query count, and the per-query
/// routing-backend audit (proving structured categories were resolved structurally, input-driven —
/// not because the evaluator knew the answer).
pub(crate) struct FranceLegiCategoryResult {
    pub(crate) metric: f64,
    pub(crate) queries: usize,
    pub(crate) backends: Value,
}

/// Assemble the `phase1_france_legi_benchmark` artifact from the three split-gate category results.
/// The two structured categories (citation resolution, temporal version pinning) GATE the claim at
/// high floors; `semantic_retrieval` is ADVISORY (recorded, never gating). `state` is `passed` only
/// when BOTH gating categories clear their floor + minimum query count; the status gate re-derives
/// pass from the recorded metrics either way.
pub(crate) fn france_legi_artifact(
    structured: FranceLegiCategoryResult,
    temporal: FranceLegiCategoryResult,
    semantic: FranceLegiCategoryResult,
    limits: FranceLegiGoldLimits,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    let passed = structured.metric >= PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10
        && structured.queries as u64 >= PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_QUERIES
        && temporal.metric >= PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10
        && temporal.queries as u64 >= PHASE1_FRANCE_LEGI_MIN_TEMPORAL_QUERIES;

    json!({
        "schema_version": 1,
        "kind": "phase1_france_legi_benchmark",
        "state": if passed { "passed" } else { "failed" },
        "jurisdiction": "france",
        "claim_scope": "France-LEGI official-evidence retrieval with intent routing: structured citation resolution and temporal version pinning (gating), plus advisory full-body semantic retrieval, through the production pipeline",
        "source": "DILA LEGI (Licence Ouverte) official fields, extracted from the built index",
        "retriever": "jurisearch search (intent-routed: structured citation resolver + BM25/dense/RRF hybrid)",
        "embedding": {
            "fingerprint_model": PHASE0_EMBEDDING_MODEL,
            "dimension": PHASE0_EMBEDDING_DIMENSION,
            "normalize": true
        },
        "thresholds": {
            "structured_citation_recall_at_10_min": PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10,
            "temporal_version_exactness_at_10_min": PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10,
            "semantic_retrieval_recall_at_10_advisory": PHASE1_FRANCE_LEGI_ADVISORY_SEMANTIC_RECALL_AT_10
        },
        "categories": {
            "structured_citation_resolution": {
                "metric_value": floor_metric(structured.metric),
                "queries": structured.queries,
                "gating": true,
                "routing_backends": structured.backends
            },
            "temporal_version_pinning": {
                "metric_value": floor_metric(temporal.metric),
                "queries": temporal.queries,
                "gating": true,
                "routing_backends": temporal.backends
            },
            "semantic_retrieval": {
                "metric_value": floor_metric(semantic.metric),
                "queries": semantic.queries,
                "gating": false,
                "advisory": true,
                "routing_backends": semantic.backends
            }
        },
        "provenance": {
            "official_source": "DILA LEGI (Licence Ouverte)",
            "source_revision": source_revision,
            "pipeline": "jurisearch search (intent-routed structured + hybrid)",
            // Record the exact fusion weights so the gate evidence is honest about the retrieval
            // configuration it measured (dense is down-weighted as a recall-expander).
            "fusion": {
                "rrf_lexical_weight": rrf_weights().0,
                "rrf_dense_weight": rrf_weights().1
            },
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            // The qrel set is a deterministic, reproducible ORDER BY + LIMIT bound (not random or
            // cherry-picked), so `sampled` is false; the per-category caps are recorded for audit.
            "qrel_selection": "deterministic_bounded_by_document_id",
            "qrel_limits": {
                "structured_citation_resolution": limits.known_item,
                "temporal_version_pinning": limits.temporal,
                "semantic_retrieval": limits.cross_reference
            },
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false
        },
        "evidence": [
            format!(
                "France-LEGI intent-routed runner over index `{index_revision}`: {} structured-citation, {} temporal, {} semantic (advisory) qrels through the production search pipeline",
                structured.queries, temporal.queries, semantic.queries
            )
        ]
    })
}

/// Run one France-LEGI query through the production search pipeline and return the ranked unique
/// document IDs plus the routing backend that served it (`structured_citation`/`hybrid`/`none`), for
/// the gate's routing audit. A `no_results` outcome is an empty list (a miss), not an error.
pub(crate) fn france_legi_search_documents(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    query: &str,
    as_of: &str,
    top_k: u32,
) -> Result<(Vec<String>, String), ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok((Vec::new(), "none".to_owned()));
    };
    let request = SearchRequest {
        query: query.to_owned(),
        kind: CliKind::Code,
        mode: CliSearchMode::Hybrid,
        format: CliOutputFormat::Concise,
        group_by: CliGroupBy::Chunk,
        top_k,
        cursor: None,
        as_of: Some(as_of.to_owned()),
        rrf_lexical_weight: None,
        rrf_dense_weight: None,
        probes: None,
        court: None,
        formation: None,
        publication: None,
        decided_from: None,
        decided_to: None,
        zone: None,
        index_dir: None,
    };
    let response = match search_with_postgres(
        postgres,
        &request,
        RetrievalMode::Hybrid,
        OutputFormat::Concise,
        None,
        &query_text,
        LegalKind::Code,
        // The runner verifies query readiness once before the loop, so skip the per-query check.
        false,
        // Reuse the embedder built once by the runner instead of rebuilding it per query.
        Some(embedder),
    ) {
        Ok(response) => response,
        Err(error) if error.code == ErrorCode::NoResults => {
            return Ok((Vec::new(), "none".to_owned()));
        }
        Err(error) => return Err(error),
    };
    let backend = response["routing"]["chosen_backend"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if let Some(document_id) = candidate["document_id"].as_str()
                && !documents.iter().any(|existing| existing == document_id)
            {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok((documents, backend))
}

/// `legi:LEGIARTI...@YYYY-MM-DD` -> `LEGIARTI...`
pub(crate) fn legi_source_uid_of(document_id: &str) -> Option<&str> {
    document_id.strip_prefix("legi:")?.split('@').next()
}

/// `legi:LEGIARTI...@YYYY-MM-DD` -> `YYYY-MM-DD`
pub(crate) fn legi_document_as_of(document_id: &str) -> Option<&str> {
    document_id.rsplit_once('@').map(|(_, date)| date)
}
