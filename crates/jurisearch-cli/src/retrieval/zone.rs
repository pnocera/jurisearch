//! zone command: official-zone (Cassation) parallel retrieval index path.

use crate::*;

/// Dedicated zone readiness gate (NOT the chunk-corpus `ensure_query_readiness`): the zone subsystem
/// has its own coverage. Requires materialized `zone_units`; for dense/hybrid also requires the
/// zone-unit embeddings to be complete (no pending units) AND finalized under the SAME fingerprint the
/// query embedder uses — otherwise the dense arm (which filters by fingerprint) would silently match
/// nothing and report a false `no_results` instead of an actionable readiness error.
pub(crate) fn ensure_zone_retrieval_readiness(
    postgres: &ManagedPostgres,
    needs_dense: bool,
    expected_fingerprint: Option<&str>,
) -> Result<(), ErrorObject> {
    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_json(postgres).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    if coverage["zone_units"]["total"].as_u64().unwrap_or(0) == 0 {
        return Err(index_unavailable(
            "no official zone units are indexed; run `ingest enrich-zones` then `ingest build-zone-units` \
             (and `ingest embed-zone-units` for hybrid/dense) before `search --zone`",
        ));
    }
    if needs_dense {
        let pending = coverage["embeddings"]["units_pending"]
            .as_u64()
            .unwrap_or(u64::MAX);
        let embedded = coverage["embeddings"]["total"].as_u64().unwrap_or(0);
        if embedded == 0 || pending != 0 {
            return Err(index_unavailable(format!(
                "the zone-unit dense index is incomplete ({pending} units pending); run \
                 `ingest embed-zone-units`, or use `--mode bm25` for lexical zone search"
            )));
        }
        if let Some(expected) = expected_fingerprint {
            let indexed = coverage["embedding_manifest"]["embedding_fingerprint"].as_str();
            if indexed != Some(expected) {
                return Err(index_unavailable(format!(
                    "the zone-unit dense index was finalized under fingerprint `{}` but the query \
                     embedder uses `{expected}`; re-run `ingest embed-zone-units` with the matching \
                     embedding config, or use `--mode bm25` for lexical zone search",
                    indexed.unwrap_or("<none>")
                )));
            }
        }
    }
    Ok(())
}

/// Run a zone-scoped search against the official-zone subsystem. Explicit opt-in only (`--zone`); the
/// result is self-labeling (a `scope` block stating the Cassation-only coverage and `zone_accurate`).
pub(crate) fn zone_search_payload(req: SearchRequest, zone: CliZone) -> Result<Value, ErrorObject> {
    // Zone scope is Cour de cassation case law; an explicit statute kind is a contradiction.
    if matches!(req.kind, CliKind::Code) {
        return Err(ErrorObject::bad_input(
            "--zone is Cour de cassation case-law scope and cannot be combined with --kind code",
        ));
    }
    // Authority on the zone path: --zone already implies decisions (so --kind all is fine), so only the
    // first-page-only cursor restriction applies. `0.0`/unset is inert; numeric validation already ran
    // in `search_payload` before the zone dispatch.
    if effective_authority_weight(&req.retrieval_options()).is_some() && req.cursor.is_some() {
        return Err(ErrorObject::bad_input(
            "--authority-weight is first-page-only and cannot be combined with --cursor; omit the cursor to get the authority-ranked first page",
        ));
    }
    let retrieval_mode: RetrievalMode = req.mode.into();
    let output_format: OutputFormat = req.format.into();
    // Zone retrieval always groups by decision; a chunk cursor from the main path is rejected.
    let after_cursor = req
        .cursor
        .as_deref()
        .map(|cursor| parse_search_cursor(cursor, CliGroupBy::Document))
        .transpose()?;
    let normalized_query_text = parade_query_text(&req.query);
    let query_text = if retrieval_mode.uses_lexical() {
        normalized_query_text.ok_or_else(|| {
            ErrorObject::bad_input("search query must contain at least one searchable token")
        })?
    } else if normalized_query_text.is_none() {
        return Err(ErrorObject::bad_input(
            "search query must contain at least one searchable token",
        ));
    } else {
        req.query.trim().to_owned()
    };
    let index_dir = require_existing_index_dir(req.index_dir.as_deref())?;
    let postgres = open_index(index_dir.as_path())?;

    let needs_dense = retrieval_mode.uses_dense();
    // Compute the expected storage fingerprint (cheap, no network) so readiness can reject a zone dense
    // index finalized under a different embedder before we run a query that would match nothing.
    let expected_fingerprint =
        needs_dense.then(|| embedding_config_from_env().storage_embedding_fingerprint());
    ensure_zone_retrieval_readiness(&postgres, needs_dense, expected_fingerprint.as_deref())?;

    let as_of = req.as_of.clone().unwrap_or_else(today_utc);
    let (query_embedding, embedding_fingerprint) = if needs_dense {
        let (literal, fingerprint) =
            PreparedQueryEmbedder::from_env()?.embed(req.query.as_str())?;
        (Some(literal), Some(fingerprint))
    } else {
        (None, None)
    };

    // Group by decision -> overfetch a deeper pool to still yield up to top_k UNIQUE decisions.
    let lexical_limit = req.top_k.saturating_mul(20);
    let dense_limit = req.top_k.saturating_mul(20);
    // Authority (A4): mirror the main path. ON widens the window to `top_k * W_eff` (W_eff clamped to
    // the zone pool multiplier 20) and projects `publication`; OFF keeps today's exact `top_k + 1`.
    let authority_weight = effective_authority_weight(&req.retrieval_options());
    let window_factor = if authority_weight.is_some() {
        AUTHORITY_RERANK_WINDOW.min(20)
    } else {
        1
    };
    let query_limit = if authority_weight.is_some() {
        req.top_k.saturating_mul(window_factor).saturating_add(1)
    } else {
        req.top_k.saturating_add(1)
    };

    let response = zone_candidates_json(
        &postgres,
        &ZoneCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: query_embedding.as_deref(),
            embedding_fingerprint: embedding_fingerprint.as_deref(),
            retrieval_mode,
            options: req.retrieval_options(),
            after_cursor: after_cursor
                .as_ref()
                .map(ParsedSearchCursor::as_retrieval_cursor),
            zone: zone.as_str(),
            as_of: as_of.as_str(),
            project_authority: authority_weight.is_some(),
            decision_filters: req.decision_filters(),
            lexical_limit,
            dense_limit,
            limit: query_limit,
        },
    )
    .map_err(storage_error_object)?;
    let mut response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    // Shared search decoration (expansion, format, limit) so the zone surface matches ordinary search.
    let expansion = expand_query(&req.query);
    response["format"] = json!(output_format.as_str());
    response["limit"] = json!(req.top_k);
    response["expansion_seed_version"] = json!(expansion.seed_version);
    response["expanded_terms"] = json!(expansion.expanded_terms);
    response["scope"] = json!({
        "mode": "official_zone_retrieval",
        "zone": zone.as_str(),
        "coverage": "cour_de_cassation (cass+inca)",
        "zone_accurate": true,
        "indexed_decisions": coverage["zone_units"]["decisions"].clone(),
        "note": "Coverage-bounded: searches ONLY resolver-reachable Cour de cassation decisions that have official Judilibre zones — not a corpus-wide search. Other courts/administrative decisions are not covered."
    });

    // Authority re-rank (A4): reorder the widened window by within-order publication authority BEFORE
    // truncation, same helper as the main path (zone results are all decisions, so no kind gate).
    let mut authority_applied = false;
    if let Some(weight) = authority_weight
        && let Some(candidates) = response["candidates"].as_array_mut()
    {
        authority_rerank(candidates, weight, AUTHORITY_DEFAULT_BAND);
        authority_applied = true;
    }
    let mut next_cursor = None;
    let top_k = req.top_k as usize;
    if let Some(candidates) = response["candidates"].as_array_mut()
        && candidates.len() > top_k
    {
        candidates.truncate(top_k);
        next_cursor = candidates
            .last()
            .and_then(|candidate| candidate["cursor"].as_str().map(str::to_owned));
    }
    let returned = response["candidates"].as_array().map_or(0, Vec::len);
    // Zone candidates normally carry a ranking cursor (paging supported). Authority re-rank is
    // first-page-only in v1: it reorders rows away from SQL keyset order, so disable paging for it.
    if authority_applied {
        next_cursor = None;
    }
    response["pagination"] = search_pagination_value(
        req.top_k,
        req.cursor.as_deref(),
        returned,
        !authority_applied,
        next_cursor.as_deref(),
    );
    if authority_applied {
        response["pagination"]["cursor_note"] = json!(
            "Authority re-rank is first-page-only in v1: cursor paging is disabled for this \
             response. Increase --top-k (or top_k in session JSON) to inspect a wider \
             authority-ranked window."
        );
        response["authority"] = json!({
            "enabled": true,
            "weight": authority_weight,
            "window_factor": window_factor,
            "paging": "first_page_only",
        });
    }
    response["routing"] = json!({
        "query_type": "zone",
        "chosen_backend": "official_zone_retrieval",
        "zone": zone.as_str(),
        "candidate_count": returned,
        "fallback_path": "none",
    });
    if matches!(output_format, OutputFormat::Detailed) {
        response["diagnostics"] = json!({
            "query_input": req.query.clone(),
            "lexical_query_text": if retrieval_mode.uses_lexical() {
                Some(query_text.as_str())
            } else {
                None
            },
            "retrieval": {
                "mode": retrieval_mode.as_str(),
                "uses_lexical": retrieval_mode.uses_lexical(),
                "uses_dense": needs_dense,
                "lexical_limit": lexical_limit,
                "dense_limit": dense_limit,
                "query_limit": query_limit,
                "zone": zone.as_str(),
                "as_of": as_of.as_str(),
                "embedding_fingerprint": expected_fingerprint.as_deref(),
                "after_cursor": req.cursor.as_deref(),
            }
        });
    }
    if response["candidates"]
        .as_array()
        .is_some_and(|candidates| candidates.is_empty())
    {
        Err(no_results("zone search returned no candidates"))
    } else {
        Ok(response)
    }
}
