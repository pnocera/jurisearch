//! zone command: official-zone (Cassation) parallel retrieval index path.

use jurisearch_storage::query::{QueryStore, ReadSnapshot};
use jurisearch_storage::zone_retrieval::zone_candidates_in_snapshot;
use jurisearch_storage::zone_units::zone_retrieval_coverage_in_snapshot;

use crate::*;

/// Dedicated zone readiness gate (NOT the chunk-corpus `ensure_query_readiness`): the zone subsystem
/// has its own coverage. Requires materialized `zone_units`; for dense/hybrid also requires the
/// zone-unit embeddings to be complete (no pending units) AND finalized under the SAME fingerprint the
/// query embedder uses — otherwise the dense arm (which filters by fingerprint) would silently match
/// nothing and report a false `no_results` instead of an actionable readiness error. Reads coverage
/// THROUGH the request snapshot (work/09 P3B) so the gate, the candidates, and the response's `scope`
/// coverage all observe ONE generation.
pub(crate) fn ensure_zone_retrieval_readiness(
    snapshot: &mut dyn ReadSnapshot,
    needs_dense: bool,
    expected_fingerprint: Option<&str>,
) -> Result<(), ErrorObject> {
    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_in_snapshot(snapshot).map_err(storage_error_object)?,
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

/// work/09 P3C: multi-corpus `--zone` fan-out is deferred — `zone_candidates_in_snapshot` would
/// otherwise read the `jurisearch_server` union views (no per-generation zone index). Fail closed for
/// more than one active corpus; single-corpus zone search is unchanged. (`--zone` is Cour de cassation
/// scope, single-corpus in practice.)
pub(crate) fn reject_multi_corpus_zone(snapshot: &dyn ReadSnapshot) -> Result<(), ErrorObject> {
    if snapshot.active_corpora().len() > 1 {
        return Err(index_unavailable(
            "multi-corpus `--zone` fan-out is not supported yet (work/09 3C deferral); zone search \
             requires a single active corpus",
        ));
    }
    Ok(())
}

/// A multi-corpus `mc:` cursor must never be replayed on the zone path (work/09 P3C): zone search is
/// single-corpus, and `zone_candidates_in_snapshot` would otherwise SILENTLY drop the cursor (the
/// single-corpus SQL has no multi-corpus keyset) and replay the first page. Reject it explicitly, like
/// the main hybrid path does.
pub(crate) fn reject_multi_corpus_zone_cursor(
    cursor: &Option<ParsedSearchCursor>,
) -> Result<(), ErrorObject> {
    if matches!(cursor, Some(ParsedSearchCursor::MultiCorpus { .. })) {
        return Err(ErrorObject::bad_input(
            "multi-corpus cursors cannot be used with --zone; restart the zone search without a cursor",
        ));
    }
    Ok(())
}

/// The snapshot-bound core of `search --zone`'s reads: zone candidate retrieval AND the response's
/// `scope` coverage, run through the SAME [`ReadSnapshot`] so they cannot straddle a generation swap
/// (work/09 P3B). Returns `(candidates_json, coverage_json)`. The readiness gate
/// ([`ensure_zone_retrieval_readiness`]) is run on this same snapshot just before this, in the adapter.
pub(crate) fn zone_candidates_and_coverage_in_snapshot(
    snapshot: &mut dyn ReadSnapshot,
    query: &ZoneCandidateQuery<'_>,
) -> Result<(Value, Value), ErrorObject> {
    let candidates: Value = serde_json::from_str(
        &zone_candidates_in_snapshot(snapshot, query).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_in_snapshot(snapshot).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok((candidates, coverage))
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
    reject_multi_corpus_zone_cursor(&after_cursor)?;
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

    // work/09 P3B: ONE read snapshot spans the whole request — the zone readiness gate, the candidate
    // retrieval, AND the response's `scope` coverage all observe the same served generation (a swap
    // mid-request can never mix gen-A candidates with gen-B coverage).
    let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
    reject_multi_corpus_zone(&*snapshot)?;
    ensure_zone_retrieval_readiness(&mut *snapshot, needs_dense, expected_fingerprint.as_deref())?;

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

    // Candidate retrieval AND the response `scope` coverage run through the SAME `snapshot` opened above
    // (the one already used for the readiness gate) — so the gate, the candidates, and
    // `scope.indexed_decisions` cannot straddle a generation swap.
    let (mut response, coverage) = zone_candidates_and_coverage_in_snapshot(
        &mut *snapshot,
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
    )?;
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

#[cfg(test)]
mod snapshot_consistency_tests {
    use super::*;
    use jurisearch_storage::generations::{
        ActivationStamps, activate_generation, create_generation_from_public,
    };
    use jurisearch_storage::runtime::{ManagedPostgres, PgConfig};

    const FP: &str = "bge-m3:1024:cls:normalize=true";

    fn stamps(sequence: i64, baseline_id: &'static str) -> ActivationStamps<'static> {
        ActivationStamps {
            sequence,
            baseline_id,
            schema_version: 24,
            embedding_fingerprint: FP,
            builder_versions: &serde_json::Value::Null,
            last_package_id: None,
            last_package_digest: None,
        }
    }

    /// Seed one ready Cassation decision in `public` (document + dense-ready chunk) plus one official
    /// `motivations` zone unit whose `search_body` matches the bm25 probe `alpha`.
    fn seed_decision_with_zone(postgres: &ManagedPostgres, doc: &str, zone_unit_id: &str) {
        postgres
            .execute_sql(&format!(
                "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
                   valid_from, source_payload_hash, canonical_json) \
                 VALUES ('{doc}','cass','decision','{doc}','Cass','Arret','corps','2024-01-01', \
                   'sha256:{doc}','{{}}'); \
                 INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
                   source_payload_hash, chunk_builder_version, embedding_fingerprint) \
                 VALUES ('{doc}#0','{doc}',0,'corps','ctx corps','sha256:c-{doc}','c1','{FP}'); \
                 INSERT INTO zone_units (zone_unit_id, document_id, zone, fragment_index, body, \
                   search_body, source, text_hash, zone_unit_builder_version) \
                 VALUES ('{zone_unit_id}','{doc}','motivations',0,'alpha corps','alpha corps','cass', \
                   'h-{zone_unit_id}','v1');"
            ))
            .unwrap();
        let vector = (0..1024).map(|_| "0.01").collect::<Vec<_>>().join(",");
        postgres
            .execute_sql(&format!(
                "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, \
                   dimension) VALUES ('{doc}#0','{FP}','[{vector}]'::vector,'m',1024);"
            ))
            .unwrap();
    }

    fn bm25_zone_query<'a>(
        query_text: &'a str,
        zone: &'a str,
        as_of: &'a str,
    ) -> ZoneCandidateQuery<'a> {
        ZoneCandidateQuery {
            query_text,
            query_embedding: None,
            embedding_fingerprint: None,
            retrieval_mode: RetrievalMode::Bm25,
            options: RetrievalOptions::default(),
            after_cursor: None,
            zone,
            as_of,
            project_authority: false,
            decision_filters: DecisionFilters::default(),
            lexical_limit: 50,
            dense_limit: 50,
            limit: 10,
        }
    }

    fn decisions(coverage: &Value) -> u64 {
        coverage["zone_units"]["decisions"].as_u64().unwrap_or(0)
    }

    fn candidate_count(candidates: &Value) -> usize {
        candidates["candidates"].as_array().map_or(0, Vec::len)
    }

    /// work/09 P3C: a multi-corpus `mc:` cursor is rejected on the zone path BEFORE any retrieval, so it
    /// can never be silently dropped into a first-page replay (even under a single-corpus zone topology).
    #[test]
    fn zone_search_rejects_a_multi_corpus_cursor() {
        // A real `mc:document:...` cursor parses to a MultiCorpus cursor (the id keeps its `:`)...
        let parsed =
            parse_search_cursor("mc:document:0.01639344:core:cass:D1", CliGroupBy::Document)
                .expect("a well-formed mc: cursor parses");
        assert!(matches!(parsed, ParsedSearchCursor::MultiCorpus { .. }));
        // ...and the zone path rejects it loudly (the main hybrid path rejects it too).
        let error = reject_multi_corpus_zone_cursor(&Some(parsed))
            .expect_err("zone search must reject a multi-corpus cursor");
        assert!(error.message.contains("multi-corpus"), "{}", error.message);
        // A normal single-corpus cursor and no cursor both pass.
        assert!(reject_multi_corpus_zone_cursor(&None).is_ok());
    }

    /// work/09 P3C: `search --zone` fails closed over a multi-corpus topology (fan-out deferred), and a
    /// single-corpus topology passes the guard.
    #[test]
    fn multi_corpus_zone_search_fails_closed() {
        let Ok(pg_config) = PgConfig::discover() else {
            return;
        };
        let root = tempfile::Builder::new()
            .prefix("jurisearch-p3c-zoneguard.")
            .tempdir()
            .unwrap();
        let postgres = ManagedPostgres::start_durable(pg_config, root.path()).unwrap();

        seed_decision_with_zone(&postgres, "cass:ZG", "zg-mot");
        let generation =
            create_generation_from_public(&postgres, "core", 1, Some("core-zg-g0001")).unwrap();
        activate_generation(
            &postgres,
            "core",
            &generation,
            &stamps(1, "core-zg-g0001"),
            None,
        )
        .unwrap();

        // Single corpus → the guard passes.
        {
            let snapshot = postgres.begin_snapshot().unwrap();
            assert!(reject_multi_corpus_zone(&*snapshot).is_ok());
        }

        // Install a second active corpus → the guard fails closed.
        postgres
            .execute_sql(
                "INSERT INTO jurisearch_control.corpus_state \
                   (corpus, active_generation, sequence, baseline_id, schema_version, \
                    embedding_fingerprint) \
                 VALUES ('inpi','inpi_g0001',1,'inpi-g0001',24,'fp');",
            )
            .unwrap();
        let snapshot = postgres.begin_snapshot().unwrap();
        let error = reject_multi_corpus_zone(&*snapshot)
            .expect_err("multi-corpus --zone must fail closed (3C deferral)");
        assert!(error.message.contains("multi-corpus"), "{}", error.message);
    }

    /// work/09 P3B (re-review fix): drives the FACTORED zone-search adapter core
    /// ([`zone_candidates_and_coverage_in_snapshot`]) plus the readiness gate across a no-sleep
    /// activation swap, and proves the candidate set AND `scope` coverage stay WHOLLY OLD for an
    /// already-open request and WHOLLY NEW for the next — i.e. they never straddle a generation swap.
    /// This would FAIL if the adapter opened a fresh snapshot for candidates or for coverage.
    #[test]
    fn zone_candidates_and_coverage_share_one_snapshot_across_a_swap() {
        let Ok(pg_config) = PgConfig::discover() else {
            return; // managed-PG harness absent → skip cleanly
        };
        let root = tempfile::Builder::new()
            .prefix("jurisearch-p3b-zoneswap.")
            .tempdir()
            .unwrap();
        let postgres = ManagedPostgres::start_durable(pg_config, root.path()).unwrap();
        postgres.run_migrations().unwrap();

        // Generation A: ONE Cassation decision with an official motivations zone unit.
        seed_decision_with_zone(&postgres, "cass:ZA", "za-mot");
        let g1 =
            create_generation_from_public(&postgres, "core", 1, Some("core-zone-g0001")).unwrap();
        activate_generation(&postgres, "core", &g1, &stamps(1, "core-zone-g0001"), None).unwrap();

        let query_text = "alpha";
        let zone = "motivations";
        let as_of = "2024-06-01";
        let query = bm25_zone_query(query_text, zone, as_of);

        // Open ONE request snapshot, run the readiness gate + the candidate/coverage reads on it.
        let mut snapshot = postgres.begin_snapshot().unwrap();
        ensure_zone_retrieval_readiness(&mut *snapshot, false, None).unwrap();
        let (cands_a, cov_a) =
            zone_candidates_and_coverage_in_snapshot(&mut *snapshot, &query).unwrap();
        assert_eq!(decisions(&cov_a), 1);
        assert_eq!(candidate_count(&cands_a), 1);

        // Swap in Generation B with a SECOND zone-bearing decision, at sequence 2.
        seed_decision_with_zone(&postgres, "cass:ZB", "zb-mot");
        let g2 =
            create_generation_from_public(&postgres, "core", 2, Some("core-zone-g0002")).unwrap();
        activate_generation(
            &postgres,
            "core",
            &g2,
            &stamps(2, "core-zone-g0002"),
            Some(1),
        )
        .unwrap();

        // The already-open request: candidates AND scope coverage are wholly-old (gen A: one decision).
        let (cands_old, cov_old) =
            zone_candidates_and_coverage_in_snapshot(&mut *snapshot, &query).unwrap();
        assert_eq!(
            decisions(&cov_old),
            1,
            "scope coverage is wholly-old for the open request"
        );
        assert_eq!(
            candidate_count(&cands_old),
            1,
            "candidates are wholly-old for the open request"
        );
        drop(snapshot);

        // The next request: candidates AND scope coverage are wholly-new (gen B: two decisions).
        let mut next = postgres.begin_snapshot().unwrap();
        let (cands_new, cov_new) =
            zone_candidates_and_coverage_in_snapshot(&mut *next, &query).unwrap();
        assert_eq!(
            decisions(&cov_new),
            2,
            "the next request sees the new scope coverage"
        );
        assert_eq!(
            candidate_count(&cands_new),
            2,
            "the next request sees the new candidates"
        );
    }
}
