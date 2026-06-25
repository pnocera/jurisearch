//! Advisory france-juris-zones zone-retrieval benchmark.

use crate::*;

/// Run the SEPARATE official-zone retrieval benchmark and emit the `phase2_zone_benchmark` artifact
/// (Z5/T5.2). Measures recall@10 of `search --zone <zone>` over the parallel `zone_units` subsystem;
/// gold = an identifier-stripped excerpt of a decision's OFFICIAL zone text → the source decision.
/// MEASURED-ONLY: it is NOT a Phase 2 gate input and its artifact (distinct `kind`, distinct `--out`)
/// never inflates the full-juridic corpus claim. Opens the index ONCE; gates on `zone` readiness (not
/// the chunk corpus), so it works independently of whether the bulk chunk index is query-ready.
pub(crate) fn eval_france_juris_zones_payload(
    args: EvalFranceJurisZonesArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;

    let retrieval_mode: RetrievalMode = args.mode.into();
    let needs_dense = retrieval_mode.uses_dense();
    // Reject a zone dense index finalized under a different embedder before running queries that would
    // match nothing — and gate on the ZONE subsystem only (independent of chunk readiness).
    let expected_fingerprint =
        needs_dense.then(|| embedding_config_from_env().storage_embedding_fingerprint());
    ensure_zone_retrieval_readiness(&postgres, needs_dense, expected_fingerprint.as_deref())?;

    let limits = FranceJurisZoneGoldLimits {
        motivations: args.motivations,
        moyens: args.moyens,
        dispositif: args.dispositif,
    };
    let gold_json =
        france_juris_zone_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    let top_k = 10u32;
    let embedder = needs_dense
        .then(PreparedQueryEmbedder::from_env)
        .transpose()?;

    let mut categories = serde_json::Map::new();
    for zone in [CliZone::Motivations, CliZone::Moyens, CliZone::Dispositif] {
        let result = france_juris_zone_retrieval_category(
            &postgres,
            embedder.as_ref(),
            retrieval_mode,
            zone,
            &gold[zone.as_str()],
            top_k,
        )?;
        categories.insert(
            zone.as_str().to_owned(),
            zone_benchmark_category(&result, args.floor),
        );
    }

    let index_revision = france_juris_index_revision(&postgres).map_err(storage_error_object)?;
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(zone_benchmark_artifact(
        Value::Object(categories),
        retrieval_mode,
        needs_dense,
        expected_fingerprint.as_deref(),
        args.floor,
        limits,
        &index_revision,
        &source_revision,
    ))
}

/// One zone-retrieval category: recall@10 over the zone's known-item qrels through the official-zone
/// search path (`zone_candidates_json`), restricted to that zone.
pub(crate) fn france_juris_zone_retrieval_category(
    postgres: &ManagedPostgres,
    embedder: Option<&PreparedQueryEmbedder>,
    retrieval_mode: RetrievalMode,
    zone: CliZone,
    qrels: &Value,
    top_k: u32,
) -> Result<FranceJurisCategoryResult, ErrorObject> {
    let score = score_known_item_qrels(
        qrels,
        |qrel| {
            let Some(query) = qrel["query"].as_str() else {
                return Ok(None);
            };
            Ok(Some((
                france_juris_zone_search_documents(
                    postgres,
                    embedder,
                    retrieval_mode,
                    zone,
                    query,
                    top_k,
                )?,
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

/// Run one zone query through the official-zone retrieval path (`zone_candidates_json`, grouped by
/// decision) and return the ranked UNIQUE decision document ids. Mirrors
/// [`france_juris_search_documents`] but on the zone subsystem; reuses the already-open index (no second
/// `open_index`). Errors if a candidate is not zone-accurate or is in the wrong zone — the zone scope
/// must hold for the benchmark to be honest.
pub(crate) fn france_juris_zone_search_documents(
    postgres: &ManagedPostgres,
    embedder: Option<&PreparedQueryEmbedder>,
    retrieval_mode: RetrievalMode,
    zone: CliZone,
    query: &str,
    top_k: u32,
) -> Result<Vec<String>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let (query_embedding, embedding_fingerprint) = match embedder {
        Some(embedder) => {
            let (literal, fingerprint) = embedder.embed(query)?;
            (Some(literal), Some(fingerprint))
        }
        None => (None, None),
    };
    let response = zone_candidates_json(
        postgres,
        &ZoneCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: query_embedding.as_deref(),
            embedding_fingerprint: embedding_fingerprint.as_deref(),
            retrieval_mode,
            options: RetrievalOptions::default(),
            after_cursor: None,
            zone: zone.as_str(),
            as_of: &today_utc(),
            decision_filters: DecisionFilters::default(),
            lexical_limit: top_k.saturating_mul(20),
            dense_limit: top_k.saturating_mul(20),
            limit: top_k,
        },
    )
    .map_err(storage_error_object)?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if candidate["zone"].as_str() != Some(zone.as_str())
                || candidate["zone_accurate"].as_bool() != Some(true)
            {
                return Err(dependency_unavailable(
                    "zone retrieval returned an off-zone or non-zone-accurate candidate; the zone scope is not holding".to_owned(),
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

/// One `phase2_zone_benchmark` category: measured recall@10 + whether it meets the PROPOSED floor.
/// A zone with no qrels reports `value:null, queries:0` (skipped/empty) and is excluded from the floor
/// verdict — never a misleading 0.0.
pub(crate) fn zone_benchmark_category(result: &FranceJurisCategoryResult, floor: f64) -> Value {
    if result.queries == 0 {
        return json!({
            "metric": "recall_at_10",
            "value": null,
            "queries": 0,
            "meets_proposed_floor": null
        });
    }
    let value = floor_metric(result.metric);
    json!({
        "metric": "recall_at_10",
        "value": value,
        "queries": result.queries,
        "meets_proposed_floor": value >= floor
    })
}

/// Assemble the `phase2_zone_benchmark` artifact. MEASURED-ONLY: `state:"measured"` (never a
/// pass/fail gate), records each zone's measured recall@10 against the PROPOSED floor, and is scoped to
/// the Cassation-only zone overlay so it can never inflate the full-juridic corpus claim. The recorded
/// `fingerprint` is the ACTUAL dense fingerprint used (`None` → `null` for a lexical-only BM25 run), so
/// the artifact's provenance never claims an embedder it did not use.
#[allow(clippy::too_many_arguments)]
pub(crate) fn zone_benchmark_artifact(
    categories: Value,
    retrieval_mode: RetrievalMode,
    uses_dense: bool,
    fingerprint: Option<&str>,
    proposed_floor: f64,
    limits: FranceJurisZoneGoldLimits,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    // Advisory only: do all the zones that actually had qrels meet the proposed floor?
    let measured: Vec<&Value> = categories
        .as_object()
        .into_iter()
        .flat_map(|map| map.values())
        .filter(|category| category["queries"].as_u64().unwrap_or(0) > 0)
        .collect();
    let all_meet_proposed_floor = !measured.is_empty()
        && measured
            .iter()
            .all(|category| category["meets_proposed_floor"].as_bool() == Some(true));

    json!({
        "schema_version": 1,
        "kind": "phase2_zone_benchmark",
        "state": "measured",
        "gate_input": false,
        "jurisdiction": "france",
        "uses_dense": uses_dense,
        "fingerprint": fingerprint,
        "claim_scope": "official Cour de cassation zone retrieval (cass+inca) ONLY — a coverage-bounded overlay, NOT corpus-wide French juridic search; this benchmark is measured-only and is NOT an input to the Phase 2 full-juridic gate",
        "source": "official Judilibre decision zones (motivations/moyens/dispositif) materialized as zone_units, extracted from the built index",
        "retriever": format!("jurisearch search --zone (zone_units {} retrieval)", retrieval_mode.as_str()),
        "retrieval_mode": retrieval_mode.as_str(),
        "proposed_floor": proposed_floor,
        "all_meet_proposed_floor": all_meet_proposed_floor,
        "categories": categories,
        "provenance": {
            "official_source": "Judilibre official decision zones (Cour de cassation), materialized as zone_units from the built index",
            "pipeline": "jurisearch search --zone (official_zone_retrieval) over zone_units / zone_unit_embeddings / zone_units_bm25_idx",
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            "source_revision": source_revision,
            "qrel_selection": "deterministic_first_fragment_per_decision_by_document_id_from_official_zone_units",
            "qrel_limits": {
                "motivations": limits.motivations,
                "moyens": limits.moyens,
                "dispositif": limits.dispositif
            },
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false,
            "pseudonymisation": "preserved: gold comes from the pseudonymised official Judilibre zone fields; no re-identification, no cross-source linking"
        },
        "reason": "measured-only official-zone retrieval recall@10; the proposed floor is advisory (calibrate on the first clone run), never asserted as a gate"
    })
}
