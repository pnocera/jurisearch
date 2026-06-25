//! France-juris benchmark (judicial/administrative retrieval + decision-citation verification).

use crate::*;

/// One Phase-2 jurisprudence benchmark category: the @10 / accuracy metric over its qrels and the
/// query count.
pub(crate) struct FranceJurisCategoryResult {
    pub(crate) metric: f64,
    pub(crate) queries: usize,
}

/// Run the France-jurisprudence benchmark and emit the `phase2_france_juris_benchmark` artifact.
/// Opens the index ONCE; runs retrieval qrels through `search_with_postgres` (Hybrid, kind=decision)
/// and citation qrels through the same `citation_lookup_json` path as CLI `cite`. Gold comes from
/// `france_juris_gold_json` (official indexed fields; NO archive re-parse, NO human/LLM).
pub(crate) fn eval_france_juris_payload(
    args: EvalFranceJurisArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Search)?;

    let limits = FranceJurisGoldLimits {
        judicial_retrieval: args.judicial_retrieval,
        administrative_retrieval: args.administrative_retrieval,
        ecli: args.ecli,
        pourvoi: args.pourvoi,
        cetatext: args.cetatext,
    };
    let gold_json = france_juris_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    // Fixed at top-10 (document-level): the gate validates recall@10, so the runner must measure @10.
    let top_k = 10u32;
    let overfetch = top_k.saturating_mul(4);
    let embedder = PreparedQueryEmbedder::from_env()?;

    let judicial = france_juris_retrieval_category(
        &postgres,
        &embedder,
        &gold["judicial_retrieval"],
        top_k,
        overfetch,
    )?;
    let administrative = france_juris_retrieval_category(
        &postgres,
        &embedder,
        &gold["administrative_retrieval"],
        top_k,
        overfetch,
    )?;
    let ecli = france_juris_citation_category(&postgres, &gold["decision_citation"]["ecli"])?;
    let pourvoi = france_juris_citation_category(&postgres, &gold["decision_citation"]["pourvoi"])?;
    let cetatext =
        france_juris_citation_category(&postgres, &gold["decision_citation"]["cetatext"])?;

    let index_revision = france_juris_index_revision(&postgres).map_err(storage_error_object)?;
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(france_juris_artifact(
        judicial,
        administrative,
        ecli,
        pourvoi,
        cetatext,
        limits,
        &index_revision,
        &source_revision,
    ))
}

/// Retrieval category: recall@10 over known-item qrels through the production hybrid search,
/// restricted to `kind=decision`.
pub(crate) fn france_juris_retrieval_category(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    qrels: &Value,
    top_k: u32,
    overfetch: u32,
) -> Result<FranceJurisCategoryResult, ErrorObject> {
    let score = score_known_item_qrels(
        qrels,
        |qrel| {
            let Some(query) = qrel["query"].as_str() else {
                return Ok(None);
            };
            Ok(Some((
                france_juris_search_documents(postgres, embedder, query, overfetch)?,
                None,
            )))
        },
        |docs, gold_id| docs.iter().take(top_k as usize).any(|doc| doc == gold_id),
    )?;
    Ok(FranceJurisCategoryResult {
        metric: score.metric,
        queries: score.queries,
    })
}

/// Run one decision query through the production search pipeline (Hybrid, kind=decision) and return
/// the ranked UNIQUE decision document ids. Errors if a non-decision candidate is returned: the
/// `kind=decision` filter must hold for the benchmark to be an honest judicial/administrative measure.
pub(crate) fn france_juris_search_documents(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    query: &str,
    top_k: u32,
) -> Result<Vec<String>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let request = benchmark_search_request(
        query,
        CliKind::Decision,
        CliGroupBy::Document,
        None,
        top_k,
    );
    let response = match search_with_postgres(
        postgres,
        &request,
        RetrievalMode::Hybrid,
        OutputFormat::Concise,
        None,
        &query_text,
        LegalKind::Decision,
        false,
        Some(embedder),
    ) {
        Ok(response) => response,
        Err(error) if error.code == ErrorCode::NoResults => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if candidate["kind"].as_str() != Some("decision") {
                return Err(dependency_unavailable(
                    "france-juris retrieval returned a non-decision candidate; the kind=decision filter is not holding".to_owned(),
                ));
            }
            if let Some(document_id) = candidate["document_id"].as_str()
                && !documents.iter().any(|existing| existing == document_id)
            {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok(documents)
}

/// Citation category: decision_citation_accuracy over identifier qrels, resolved through the SAME
/// production citation path as CLI `cite` (`citation_lookup_json`). A qrel is a hit when the gold
/// document is among the resolved matches.
pub(crate) fn france_juris_citation_category(
    postgres: &ManagedPostgres,
    qrels: &Value,
) -> Result<FranceJurisCategoryResult, ErrorObject> {
    let score = score_known_item_qrels(
        qrels,
        |qrel| {
            let Some(query) = qrel["query"].as_str() else {
                return Ok(None);
            };
            Ok(Some((france_juris_cite_documents(postgres, query)?, None)))
        },
        |docs, gold_id| docs.iter().any(|doc| doc == gold_id),
    )?;
    Ok(FranceJurisCategoryResult {
        metric: score.metric,
        queries: score.queries,
    })
}

/// Resolve one citation identifier through the production `citation_lookup_json` path and return the
/// matched document ids.
pub(crate) fn france_juris_cite_documents(
    postgres: &ManagedPostgres,
    query: &str,
) -> Result<Vec<String>, ErrorObject> {
    let parsed = parse_citation_target(query);
    let Some(lookup) = parsed.lookup() else {
        return Ok(Vec::new());
    };
    let response = citation_lookup_json(postgres, &CitationLookupQuery { lookup, limit: 25 })
        .map_err(storage_error_object)?;
    let parsed_response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    let mut documents = Vec::new();
    if let Some(matches) = parsed_response["matches"].as_array() {
        for entry in matches {
            if let Some(document_id) = entry["document_id"].as_str() {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok(documents)
}

/// Assemble the `phase2_france_juris_benchmark` artifact in the exact shape the Phase 2 gate
/// re-derives (`phase2_benchmark_artifact_errors`): category `metric`/`value`/`queries`,
/// `decision_citation.by_identifier`, and production provenance. Metrics are floored to 3 decimals so
/// the RECORDED value can never exceed the measured one; the gate re-derives pass/fail from the fields.
pub(crate) fn france_juris_artifact(
    judicial: FranceJurisCategoryResult,
    administrative: FranceJurisCategoryResult,
    ecli: FranceJurisCategoryResult,
    pourvoi: FranceJurisCategoryResult,
    cetatext: FranceJurisCategoryResult,
    limits: FranceJurisGoldLimits,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    let citation_pass = |category: &FranceJurisCategoryResult| {
        floor_metric(category.metric) >= PHASE2_MIN_DECISION_CITATION_ACCURACY
            && category.queries as u64 >= PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER
    };
    let passed = floor_metric(judicial.metric) >= PHASE2_MIN_RETRIEVAL_RECALL_AT_10
        && judicial.queries as u64 >= PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES
        && floor_metric(administrative.metric) >= PHASE2_MIN_RETRIEVAL_RECALL_AT_10
        && administrative.queries as u64 >= PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES
        && citation_pass(&ecli)
        && citation_pass(&pourvoi)
        && citation_pass(&cetatext);

    let citation_category = |category: &FranceJurisCategoryResult| {
        json!({
            "metric": "decision_citation_accuracy",
            "value": floor_metric(category.metric),
            "queries": category.queries
        })
    };

    json!({
        "schema_version": 1,
        "kind": "phase2_france_juris_benchmark",
        "state": if passed { "passed" } else { "failed" },
        "jurisdiction": "france",
        "fingerprint": "bge-m3:1024:normalize:true",
        "claim_scope": "full French juridic search (statutes + jurisprudence): judicial (Cassation/appeal) AND administrative retrieval AND ECLI/pourvoi/CETATEXT decision-citation verification, through the production pipeline",
        "source": "DILA CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte) official fields, extracted from the built index",
        "retriever": "jurisearch search (hybrid BM25/dense/RRF, kind=decision) + citation resolver",
        "categories": {
            "judicial_retrieval": {
                "metric": "recall_at_10",
                "value": floor_metric(judicial.metric),
                "queries": judicial.queries
            },
            "administrative_retrieval": {
                "metric": "recall_at_10",
                "value": floor_metric(administrative.metric),
                "queries": administrative.queries
            },
            "decision_citation": {
                "metric": "decision_citation_accuracy",
                "by_identifier": {
                    "ecli": citation_category(&ecli),
                    "pourvoi": citation_category(&pourvoi),
                    "cetatext": citation_category(&cetatext)
                }
            }
        },
        "provenance": {
            "official_source": "DILA CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte), extracted from the built index",
            "pipeline": PHASE2_PRODUCTION_PIPELINE,
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            "source_revision": source_revision,
            "qrel_selection": "deterministic_bounded_by_document_id_from_official_index_fields",
            "qrel_limits": {
                "judicial_retrieval": limits.judicial_retrieval,
                "administrative_retrieval": limits.administrative_retrieval,
                "ecli": limits.ecli,
                "pourvoi": limits.pourvoi,
                "cetatext": limits.cetatext
            },
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false,
            "pseudonymisation": "preserved: gold and identifiers come from the pseudonymised official bulk fields; no re-identification, no cross-source linking"
        },
        "evidence": [
            format!(
                "France-jurisprudence runner over index `{index_revision}`: {} judicial + {} administrative retrieval recall@10, {} ECLI / {} pourvoi / {} CETATEXT citation-accuracy qrels through the production search/cite pipeline",
                judicial.queries, administrative.queries, ecli.queries, pourvoi.queries, cetatext.queries
            )
        ],
        "reason": if passed {
            "all Phase 2 categories cleared their floors through the production pipeline"
        } else {
            "one or more Phase 2 categories did not clear the floor or minimum query count"
        }
    })
}
