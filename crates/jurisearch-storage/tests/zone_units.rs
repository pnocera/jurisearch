mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    decision_zones::{UpsertDecisionZones, upsert_decision_zones},
    dense::DenseRebuildSpec,
    france_juris::{FranceJurisZoneGoldLimits, france_juris_zone_gold_json},
    retrieval::{DecisionFilters, RetrievalMode, RetrievalOptions},
    runtime::{ManagedPostgres, StorageError},
    zone_retrieval::{ZoneCandidateQuery, zone_candidates_json},
    zone_units::{
        EnrichZoneOrder, ZONE_UNIT_VECTOR_INDEX_NAME, ZoneUnitEmbeddingInsert, ZoneUnitRow,
        enrich_zone_candidates_json, finalize_zone_dense_rebuild, insert_zone_unit_embeddings,
        load_derivable_decision_zones_json, load_zone_unit_embedding_inputs,
        replace_zone_units_for_document, zone_resolver_reachable_json,
        zone_retrieval_coverage_json,
    },
};

const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
const BUILDER: &str = "zone-units:v1";

/// Seed a Cassation decision plus its `decision_zones` overlay row (source `cass`). `text_hash` may be
/// NULL to model the lazy / pre-hash-fix rows.
fn seed_decision(
    postgres: &ManagedPostgres,
    document_id: &str,
    pourvoi: &str,
    status: &str,
    text_hash: Option<&str>,
    zones_json: &str,
) -> Result<(), StorageError> {
    seed_decision_full(
        postgres,
        document_id,
        "cass",
        "decision",
        pourvoi,
        status,
        text_hash,
        zones_json,
        false,
    )
}

/// Full control over the seeded decision: source/kind, pourvoi, status, hash, and whether the
/// `decision_zones` row is expired (for the freshness/scope tests).
#[allow(clippy::too_many_arguments)]
fn seed_decision_full(
    postgres: &ManagedPostgres,
    document_id: &str,
    source: &str,
    kind: &str,
    pourvoi: &str,
    status: &str,
    text_hash: Option<&str>,
    zones_json: &str,
    expired: bool,
) -> Result<(), StorageError> {
    let hash_literal = match text_hash {
        Some(hash) => format!("'{hash}'"),
        None => "NULL".to_owned(),
    };
    let expires = if expired {
        "now() - interval '1 day'"
    } else {
        "now() + interval '30 days'"
    };
    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('{document_id}', '{source}', '{kind}', '{document_id}', 'Cass. civ. {pourvoi}', \
            'Arret', 'corps de la decision', '2024-01-01', 'sha256:{document_id}', \
            '{{\"case_numbers\":[\"{pourvoi}\"]}}'); \
         INSERT INTO decision_zones \
           (document_id, provider, provider_decision_id, source_uid, status, \
            fetched_at, expires_at, text_hash, offset_unit, zones_json, raw_json) \
         VALUES \
           ('{document_id}', 'judilibre', 'jdl:{document_id}', '{document_id}', '{status}', \
            now(), {expires}, {hash_literal}, 'char', \
            '{zones_json}'::jsonb, '{{}}'::jsonb);",
    ))?;
    Ok(())
}

#[test]
fn zone_units_derive_embed_finalize_roundtrip() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("zone units roundtrip")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-units.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // Migrations must have reached the current head (zone tables present).
    let schema = postgres.execute_sql(
        "SELECT (value->>'schema_version') FROM index_manifest WHERE key = 'schema';",
    )?;
    assert_eq!(schema.trim(), "21");

    let zones = r#"{"motivations":[{"start":0,"end":5,"text":"motif un"},{"start":6,"end":10,"text":"motif deux"}],"moyens":[{"start":0,"end":4,"text":"moyen"}],"dispositif":[]}"#;
    seed_decision(
        &postgres,
        "cass:JURITEXT0001",
        "12-34567",
        "ok",
        Some("hash-1"),
        zones,
    )?;

    // load_derivable returns the ok+hash row with its zones object.
    let derivable: serde_json::Value = serde_json::from_str(&load_derivable_decision_zones_json(
        &postgres, BUILDER, false, None, 100,
    )?)
    .expect("derivable JSON");
    let candidates = derivable["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["document_id"], "cass:JURITEXT0001");
    assert!(candidates[0]["zones"]["motivations"].is_array());

    // Derive units (the CLI's job; here we build the rows directly): 2 motivations + 1 moyens.
    let rows = vec![
        ZoneUnitRow {
            document_id: "cass:JURITEXT0001",
            zone: "motivations",
            fragment_index: 0,
            body: "motif un",
            search_body: "motif un",
            source: "cass",
            text_hash: "hash-1",
            builder_version: BUILDER,
        },
        ZoneUnitRow {
            document_id: "cass:JURITEXT0001",
            zone: "motivations",
            fragment_index: 1,
            body: "motif deux",
            search_body: "motif deux",
            source: "cass",
            text_hash: "hash-1",
            builder_version: BUILDER,
        },
        ZoneUnitRow {
            document_id: "cass:JURITEXT0001",
            zone: "moyens",
            fragment_index: 0,
            body: "moyen",
            search_body: "moyen",
            source: "cass",
            text_hash: "hash-1",
            builder_version: BUILDER,
        },
    ];
    replace_zone_units_for_document(&postgres, "cass:JURITEXT0001", &rows, None)?;
    let unit_count = postgres.execute_sql("SELECT count(*)::text FROM zone_units;")?;
    assert_eq!(unit_count.trim(), "3");

    // After derivation, the decision is no longer derivable (units current).
    let none: serde_json::Value = serde_json::from_str(&load_derivable_decision_zones_json(
        &postgres, BUILDER, false, None, 100,
    )?)
    .expect("derivable JSON");
    assert_eq!(none["candidates"].as_array().expect("candidates").len(), 0);
    // A builder-version bump makes it derivable again.
    let stale: serde_json::Value = serde_json::from_str(&load_derivable_decision_zones_json(
        &postgres,
        "zone-units:v2",
        false,
        None,
        100,
    )?)
    .expect("derivable JSON");
    assert_eq!(stale["candidates"].as_array().expect("candidates").len(), 1);

    // Embedding inputs: 3 pending units in stable order.
    let inputs =
        load_zone_unit_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0].zone_unit_id, "cass:JURITEXT0001#motivations#0");
    assert_eq!(inputs[0].embedding_text, "motif un");

    // finalize refuses while units are unembedded (the finalize-gap guard).
    let spec = DenseRebuildSpec {
        embedding_fingerprint: EMBEDDING_FINGERPRINT,
        model: "bge-m3",
        dimension: 1024,
        normalize: true,
        provisional: true,
        reembeddable: true,
        index_lists: 1,
    };
    assert!(matches!(
        finalize_zone_dense_rebuild(&postgres, &spec, None).unwrap_err(),
        StorageError::DenseRebuild { .. }
    ));

    // Embed all three (a distinct vector each), then finalize.
    let vectors: Vec<String> = (0..3).map(vector_literal).collect();
    let inserts: Vec<ZoneUnitEmbeddingInsert> = inputs
        .iter()
        .enumerate()
        .map(|(i, input)| ZoneUnitEmbeddingInsert {
            zone_unit_id: &input.zone_unit_id,
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal: &vectors[i],
            model: "bge-m3",
            dimension: 1024,
        })
        .collect();
    assert_eq!(insert_zone_unit_embeddings(&postgres, &inserts, None)?, 3);

    let report = finalize_zone_dense_rebuild(&postgres, &spec, None)?;
    assert_eq!(report.zone_units, 3);
    assert_eq!(report.embeddings, 3);
    assert_eq!(report.index_name, ZONE_UNIT_VECTOR_INDEX_NAME);
    // Explicit `index_lists = 1` is honored verbatim and the manifest carries the derived probes.
    assert_eq!(report.index_lists, 1);

    let index = postgres.execute_sql(&format!(
        "SELECT indexname FROM pg_indexes WHERE schemaname='public' AND indexname='{ZONE_UNIT_VECTOR_INDEX_NAME}';",
    ))?;
    assert_eq!(index.trim(), ZONE_UNIT_VECTOR_INDEX_NAME);

    let manifest: serde_json::Value = serde_json::from_str(
        &postgres.execute_sql("SELECT value FROM index_manifest WHERE key = 'zone_embedding';")?,
    )
    .expect("zone embedding manifest is stable JSON");
    assert_eq!(manifest["vector_index"]["lists"], 1);
    assert_eq!(manifest["vector_index"]["default_probes"], 1);

    // `index_lists == 0` auto-scales to the corpus size (3 zone units → a single list, probes follow),
    // and the report reflects the list count actually built rather than the requested 0.
    let auto_report = finalize_zone_dense_rebuild(
        &postgres,
        &DenseRebuildSpec {
            index_lists: 0,
            ..spec
        },
        None,
    )?;
    assert_eq!(auto_report.index_lists, 1);
    let auto_manifest: serde_json::Value = serde_json::from_str(
        &postgres.execute_sql("SELECT value FROM index_manifest WHERE key = 'zone_embedding';")?,
    )
    .expect("zone embedding manifest is stable JSON");
    assert_eq!(auto_manifest["vector_index"]["lists"], 1);
    assert_eq!(auto_manifest["vector_index"]["default_probes"], 1);

    // Coverage block reflects the seeded state.
    let coverage: serde_json::Value =
        serde_json::from_str(&zone_retrieval_coverage_json(&postgres)?).expect("coverage JSON");
    assert_eq!(coverage["zone_units"]["total"], 3);
    assert_eq!(coverage["zone_units"]["decisions"], 1);
    assert_eq!(coverage["embeddings"]["total"], 3);
    assert_eq!(coverage["embeddings"]["units_pending"], 0);

    // T5.1: the resolver-reachable denominator counts the seeded cass decision (parser-valid pourvoi).
    let reach: serde_json::Value =
        serde_json::from_str(&zone_resolver_reachable_json(&postgres)?).expect("reachable JSON");
    assert_eq!(reach["resolver_reachable_total"], 1);

    Ok(())
}

#[test]
fn zone_gold_strips_identifiers_dedupes_and_honors_caps() -> Result<(), StorageError> {
    // T5.2: zone gold = an identifier-stripped excerpt of the OFFICIAL zone text → the source decision,
    // ONE qrel per decision (first fragment), with a 0 cap skipping a zone.
    let Some(pg_config) = discover_pg_config("zone gold")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-gold.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:GOLD1", "12-34567", "ok", Some("h"), "{}")?;

    // Fragment 0 (the one gold must pick) embeds document identifiers — an ECLI plus the decision's own
    // pourvoi in both plain and dotted form — that must all be stripped from the query; fragment 1 of
    // the same decision must be deduped away. A short moyens fragment exists too.
    let motif0 = "Sur le pourvoi n 12-34567 ECLI:FR:CCASS:2024:C100123 et le pourvoi connexe 98-76.543, \
                  la responsabilite civile du gardien de la chose suppose la garde et le fait de la chose \
                  ainsi que le lien de causalite entre eux et le dommage subi par la victime.";
    let rows = vec![
        ZoneUnitRow {
            document_id: "cass:GOLD1",
            zone: "motivations",
            fragment_index: 0,
            body: motif0,
            search_body: motif0,
            source: "cass",
            text_hash: "h",
            builder_version: BUILDER,
        },
        ZoneUnitRow {
            document_id: "cass:GOLD1",
            zone: "motivations",
            fragment_index: 1,
            body: "Un second fragment de motivations qui ne doit pas produire un second qrel pour la \
                   meme decision puisque le gold est dedoublonne par document.",
            search_body: "x",
            source: "cass",
            text_hash: "h",
            builder_version: BUILDER,
        },
        ZoneUnitRow {
            document_id: "cass:GOLD1",
            zone: "moyens",
            fragment_index: 0,
            body: "Le moyen tire de la prescription quinquennale doit etre apprecie au regard de la \
                   date de la connaissance des faits par le demandeur a l'action en responsabilite.",
            search_body: "y",
            source: "cass",
            text_hash: "h",
            builder_version: BUILDER,
        },
    ];
    replace_zone_units_for_document(&postgres, "cass:GOLD1", &rows, None)?;

    // moyens cap 0 -> the moyens zone is skipped even though a fragment exists.
    let gold: serde_json::Value = serde_json::from_str(&france_juris_zone_gold_json(
        &postgres,
        FranceJurisZoneGoldLimits {
            motivations: 60,
            moyens: 0,
            dispositif: 60,
        },
    )?)
    .expect("zone gold JSON");

    let motivations = gold["motivations"].as_array().expect("motivations");
    assert_eq!(motivations.len(), 1, "one qrel per decision (deduped)");
    assert_eq!(motivations[0]["gold_document_id"], "cass:GOLD1");
    assert_eq!(motivations[0]["source"], "cass");
    let query = motivations[0]["query"].as_str().expect("query");
    assert!(
        !query.contains("ECLI:FR:CCASS:2024:C100123"),
        "the document identifier must be stripped from the gold query: {query:?}"
    );
    assert!(
        !query.contains("12-34567") && !query.contains("98-76.543"),
        "Cassation pourvoi identifiers (plain and dotted) must be stripped: {query:?}"
    );
    assert!(
        query.contains("responsabilite civile du gardien"),
        "the semantic zone text must survive stripping: {query:?}"
    );
    assert!(
        gold["moyens"].as_array().expect("moyens").is_empty(),
        "a 0 cap skips the zone"
    );
    Ok(())
}

#[test]
fn zone_resolver_reachable_splits_pourvoi_from_skipped() -> Result<(), StorageError> {
    // T5.1: the denominator counts, per cass/inca source, the resolver-reachable decisions (parser-valid
    // pourvoi) vs. those skipped for lack of one — the honest base of the coverage fraction.
    let Some(pg_config) = discover_pg_config("zone resolver reachable")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-reachable.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    // Two cass decisions: one with a parser-valid pourvoi (reachable), one whose case number cannot
    // normalize to NN-NNNN (skipped). A capp decision is out of scope and must not be counted at all.
    seed_decision(&postgres, "cass:REACH", "12-34567", "ok", Some("h"), "{}")?;
    seed_decision(&postgres, "cass:SKIP", "FOOBAR", "not_found", None, "{}")?;
    seed_decision_full(
        &postgres,
        "capp:OUT",
        "capp",
        "decision",
        "44-44444",
        "ok",
        Some("h"),
        "{}",
        false,
    )?;

    let reach: serde_json::Value =
        serde_json::from_str(&zone_resolver_reachable_json(&postgres)?).expect("reachable JSON");
    assert_eq!(reach["resolver_reachable_total"], 1);
    let by_source = reach["by_source"].as_array().expect("by_source");
    let cass = by_source
        .iter()
        .find(|entry| entry["source"] == "cass")
        .expect("cass entry");
    assert_eq!(cass["total"], 2, "both cass decisions counted in the base");
    assert_eq!(cass["resolver_reachable"], 1);
    assert_eq!(cass["skipped_no_pourvoi"], 1);
    assert!(
        by_source.iter().all(|entry| entry["source"] != "capp"),
        "capp is out of the zone-enrichable scope: {by_source:?}"
    );
    Ok(())
}

#[test]
fn zone_candidates_json_scopes_to_zone_with_official_provenance() -> Result<(), StorageError> {
    // Z4: zone retrieval returns the decision under the matched zone (zone_accurate=true, judilibre),
    // and a query whose term lives in another zone (or an empty zone) returns nothing under this scope.
    let Some(pg_config) = discover_pg_config("zone candidates")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-candidates.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:Q1", "12-34567", "ok", Some("h"), "{}")?;
    let rows = vec![
        ZoneUnitRow {
            document_id: "cass:Q1",
            zone: "motivations",
            fragment_index: 0,
            body: "la responsabilite civile du gardien de la chose",
            search_body: "la responsabilite civile du gardien de la chose",
            source: "cass",
            text_hash: "h",
            builder_version: BUILDER,
        },
        ZoneUnitRow {
            document_id: "cass:Q1",
            zone: "moyens",
            fragment_index: 0,
            body: "le moyen tire de la prescription quinquennale",
            search_body: "le moyen tire de la prescription quinquennale",
            source: "cass",
            text_hash: "h",
            builder_version: BUILDER,
        },
    ];
    replace_zone_units_for_document(&postgres, "cass:Q1", &rows, None)?;

    let base = ZoneCandidateQuery {
        query_text: "responsabilite",
        query_embedding: None,
        embedding_fingerprint: None,
        retrieval_mode: RetrievalMode::Bm25,
        options: RetrievalOptions::default(),
        after_cursor: None,
        zone: "motivations",
        as_of: "2025-12-31",
        decision_filters: DecisionFilters::default(),
        project_authority: false,
        lexical_limit: 50,
        dense_limit: 50,
        limit: 10,
    };
    let hit: serde_json::Value =
        serde_json::from_str(&zone_candidates_json(&postgres, &base)?).expect("json");
    let candidates = hit["candidates"].as_array().expect("candidates");
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["document_id"], "cass:Q1");
    assert_eq!(candidates[0]["zone"], "motivations");
    assert_eq!(candidates[0]["zone_accurate"], true);
    assert_eq!(candidates[0]["provider"], "judilibre");
    assert_eq!(hit["zone"], "motivations");
    assert!(
        candidates[0].get("publication").is_none(),
        "OFF zone projection must not expose publication"
    );

    // A5 (zone projection gate): mirror the main-path A2 test. Even with a publication marker present in
    // canonical_json, the OFF path (project_authority=false) must omit it; only the ON path exposes it.
    postgres.execute_sql(
        "UPDATE documents SET canonical_json = canonical_json || '{\"publication\": \"oui\"}'::jsonb \
         WHERE document_id = 'cass:Q1';",
    )?;
    let off_again: serde_json::Value =
        serde_json::from_str(&zone_candidates_json(&postgres, &base)?).expect("json");
    assert!(
        off_again["candidates"][0].get("publication").is_none(),
        "OFF zone projection must omit publication even when present in canonical_json"
    );
    let projected: serde_json::Value = serde_json::from_str(&zone_candidates_json(
        &postgres,
        &ZoneCandidateQuery {
            project_authority: true,
            ..base
        },
    )?)
    .expect("json");
    assert_eq!(projected["candidates"][0]["publication"], "oui");

    // Z4-fix (as-of): the decision is dated 2024-01-01; an as_of BEFORE it excludes it.
    let historical = ZoneCandidateQuery {
        as_of: "2020-01-01",
        ..base
    };
    let before: serde_json::Value =
        serde_json::from_str(&zone_candidates_json(&postgres, &historical)?).expect("json");
    assert_eq!(
        before["candidates"].as_array().expect("c").len(),
        0,
        "as_of before the decision date must exclude it"
    );

    // Z4-fix (decision filters applied in-arm): a non-matching court filter excludes the hit.
    let wrong_court = ZoneCandidateQuery {
        decision_filters: DecisionFilters {
            jurisdiction: Some("Conseil d'Etat"),
            ..DecisionFilters::default()
        },
        ..base
    };
    let filtered: serde_json::Value =
        serde_json::from_str(&zone_candidates_json(&postgres, &wrong_court)?).expect("json");
    assert_eq!(
        filtered["candidates"].as_array().expect("c").len(),
        0,
        "a non-matching decision filter must exclude the hit"
    );

    // "responsabilite" lives in motivations, not moyens -> empty under the moyens scope.
    let moyens = ZoneCandidateQuery {
        zone: "moyens",
        ..base
    };
    let none: serde_json::Value =
        serde_json::from_str(&zone_candidates_json(&postgres, &moyens)?).expect("json");
    assert_eq!(none["candidates"].as_array().expect("c").len(), 0);

    // dispositif has no units at all -> empty.
    let dispositif = ZoneCandidateQuery {
        zone: "dispositif",
        ..base
    };
    let empty: serde_json::Value =
        serde_json::from_str(&zone_candidates_json(&postgres, &dispositif)?).expect("json");
    assert_eq!(empty["candidates"].as_array().expect("c").len(), 0);
    Ok(())
}

#[test]
fn enrich_candidates_reenrich_fresh_ok_rows_with_null_text_hash() -> Result<(), StorageError> {
    // BLOCKER-1 regression: a FRESH `ok` decision_zones row whose text_hash is NULL (the lazy /
    // pre-hash-fix rows) must be selected for re-enrichment regardless of TTL, else it is never
    // derivable.
    let Some(pg_config) = discover_pg_config("enrich null-hash")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-enrich.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A: fresh ok row WITH a hash -> not a candidate. B: fresh ok row with NULL hash -> a candidate.
    seed_decision(
        &postgres,
        "cass:WITHHASH",
        "11-11111",
        "ok",
        Some("h"),
        "{}",
    )?;
    seed_decision(&postgres, "cass:NULLHASH", "22-22222", "ok", None, "{}")?;

    let page: serde_json::Value = serde_json::from_str(&enrich_zone_candidates_json(
        &postgres,
        "cass",
        None,
        None,
        100,
        EnrichZoneOrder::Oldest,
    )?)
    .expect("candidates JSON");
    let ids: Vec<&str> = page["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| c["document_id"].as_str().expect("id"))
        .collect();
    assert!(
        ids.contains(&"cass:NULLHASH"),
        "null-hash ok row must be re-enriched: {ids:?}"
    );
    assert!(
        !ids.contains(&"cass:WITHHASH"),
        "hashed fresh ok row must be skipped: {ids:?}"
    );

    Ok(())
}

#[test]
fn expired_ok_rows_are_refresh_candidates_not_derivable() -> Result<(), StorageError> {
    // Z1-fix 1: an EXPIRED ok row with a hash and no units must be a refresh candidate (enrich) but
    // NOT derivable (never materialize a stale zone before refresh).
    let Some(pg_config) = discover_pg_config("zone expired derive")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-expired.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision_full(
        &postgres,
        "cass:EXPIRED",
        "cass",
        "decision",
        "33-33333",
        "ok",
        Some("h"),
        "{}",
        true,
    )?;

    let derivable: serde_json::Value = serde_json::from_str(&load_derivable_decision_zones_json(
        &postgres, BUILDER, false, None, 100,
    )?)
    .expect("derivable JSON");
    assert_eq!(
        derivable["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        0,
        "expired ok row must not be derivable"
    );
    let enrich: serde_json::Value = serde_json::from_str(&enrich_zone_candidates_json(
        &postgres,
        "cass",
        None,
        None,
        100,
        EnrichZoneOrder::Oldest,
    )?)
    .expect("candidates JSON");
    let ids: Vec<&str> = enrich["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| c["document_id"].as_str().expect("id"))
        .collect();
    assert!(
        ids.contains(&"cass:EXPIRED"),
        "expired ok row must be a refresh candidate: {ids:?}"
    );
    Ok(())
}

#[test]
fn enrich_candidate_order_recent_walks_newest_first() -> Result<(), StorageError> {
    // Rollout fix: `--order recent` keysets newest->oldest so zoned (recent) decisions are reached
    // first; `oldest` is the reverse. Both must page deterministically via next_cursor.
    let Some(pg_config) = discover_pg_config("zone enrich order")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-order.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    // Three never-enriched cass decisions (no decision_zones row -> all candidates).
    seed_decision(&postgres, "cass:AAA", "11-11111", "ok", Some("h"), "{}")?;
    seed_decision(&postgres, "cass:BBB", "22-22222", "ok", Some("h"), "{}")?;
    seed_decision(&postgres, "cass:CCC", "33-33333", "ok", Some("h"), "{}")?;
    // Clear the decision_zones rows the seed inserted so all three are fresh candidates.
    postgres.execute_sql("DELETE FROM decision_zones;")?;

    // Recent order, page size 1 -> the newest id first, and next_cursor is that id (so the next page
    // is strictly older).
    let recent: serde_json::Value = serde_json::from_str(&enrich_zone_candidates_json(
        &postgres,
        "cass",
        None,
        None,
        1,
        EnrichZoneOrder::Recent,
    )?)
    .expect("candidates JSON");
    assert_eq!(recent["candidates"][0]["document_id"], "cass:CCC");
    assert_eq!(recent["next_cursor"], "cass:CCC");
    // Next recent page after cass:CCC -> cass:BBB.
    let recent2: serde_json::Value = serde_json::from_str(&enrich_zone_candidates_json(
        &postgres,
        "cass",
        Some("cass:CCC"),
        None,
        1,
        EnrichZoneOrder::Recent,
    )?)
    .expect("candidates JSON");
    assert_eq!(recent2["candidates"][0]["document_id"], "cass:BBB");

    // Oldest order, page size 1 -> the oldest id first (the original behavior).
    let oldest: serde_json::Value = serde_json::from_str(&enrich_zone_candidates_json(
        &postgres,
        "cass",
        None,
        None,
        1,
        EnrichZoneOrder::Oldest,
    )?)
    .expect("candidates JSON");
    assert_eq!(oldest["candidates"][0]["document_id"], "cass:AAA");
    assert_eq!(oldest["next_cursor"], "cass:AAA");
    Ok(())
}

#[test]
fn derivation_enforces_cassation_scope() -> Result<(), StorageError> {
    // Z1-fix 3: a foreign-source (e.g. capp) ok row, even with a hash and valid-looking pourvoi, must
    // not be derivable — derivation mirrors the Cassation-only enrichment reachability gate.
    let Some(pg_config) = discover_pg_config("zone derive scope")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-scope.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision_full(
        &postgres,
        "capp:FOREIGN",
        "capp",
        "decision",
        "44-44444",
        "ok",
        Some("h"),
        "{}",
        false,
    )?;
    let derivable: serde_json::Value = serde_json::from_str(&load_derivable_decision_zones_json(
        &postgres, BUILDER, false, None, 100,
    )?)
    .expect("derivable JSON");
    assert_eq!(
        derivable["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        0,
        "non-cass/inca ok row must not be derivable"
    );
    Ok(())
}

#[test]
fn replace_zone_units_rejects_foreign_document_rows() -> Result<(), StorageError> {
    // Z1-fix 2: replacing doc A must reject a row that belongs to doc B (no cross-document write).
    let Some(pg_config) = discover_pg_config("zone replace guard")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-replace-guard.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:DOCA", "55-55555", "ok", Some("h"), "{}")?;
    seed_decision(&postgres, "cass:DOCB", "66-66666", "ok", Some("h"), "{}")?;

    let foreign = ZoneUnitRow {
        document_id: "cass:DOCB",
        zone: "motivations",
        fragment_index: 0,
        body: "x",
        search_body: "x",
        source: "cass",
        text_hash: "h",
        builder_version: BUILDER,
    };
    let err =
        replace_zone_units_for_document(&postgres, "cass:DOCA", &[foreign], None).unwrap_err();
    assert!(matches!(err, StorageError::Projection { .. }));
    let count = postgres.execute_sql("SELECT count(*)::text FROM zone_units;")?;
    assert_eq!(count.trim(), "0", "no units written on a rejected replace");
    Ok(())
}

#[test]
fn non_derivable_refresh_invalidates_materialized_units() -> Result<(), StorageError> {
    // Z1 r3-fix: refreshing a derived decision_zones row to a non-derivable status (e.g. not_found)
    // must drop its already-materialized zone_units (and cascade zone_unit_embeddings), so retrieval
    // never serves official zones the cache just invalidated.
    let Some(pg_config) = discover_pg_config("zone refresh invalidate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-zone-invalidate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:INVAL", "77-77777", "ok", Some("h"), "{}")?;

    let row = ZoneUnitRow {
        document_id: "cass:INVAL",
        zone: "motivations",
        fragment_index: 0,
        body: "motif",
        search_body: "motif",
        source: "cass",
        text_hash: "h",
        builder_version: BUILDER,
    };
    replace_zone_units_for_document(&postgres, "cass:INVAL", &[row], None)?;
    let vector = vector_literal(0);
    insert_zone_unit_embeddings(
        &postgres,
        &[ZoneUnitEmbeddingInsert {
            zone_unit_id: "cass:INVAL#motivations#0",
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal: &vector,
            model: "bge-m3",
            dimension: 1024,
        }],
        None,
    )?;
    assert_eq!(
        postgres
            .execute_sql("SELECT count(*)::text FROM zone_units;")?
            .trim(),
        "1"
    );
    assert_eq!(
        postgres
            .execute_sql("SELECT count(*)::text FROM zone_unit_embeddings;")?
            .trim(),
        "1"
    );

    // Refresh the same decision to not_found (hashless, non-derivable) -> units + embeddings cleared.
    let empty = serde_json::json!({});
    upsert_decision_zones(
        &postgres,
        &UpsertDecisionZones {
            document_id: "cass:INVAL",
            provider: "judilibre",
            provider_decision_id: None,
            source_uid: "cass:INVAL",
            ecli: None,
            status: "not_found",
            upstream_update_date: None,
            upstream_decision_date: None,
            text_hash: None,
            offset_unit: None,
            zones_json: &empty,
            raw_json: &empty,
            error: None,
            ttl_seconds: Some(86_400),
        },
    )?;
    assert_eq!(
        postgres
            .execute_sql("SELECT count(*)::text FROM zone_units;")?
            .trim(),
        "0",
        "non-derivable refresh must drop zone_units"
    );
    assert_eq!(
        postgres
            .execute_sql("SELECT count(*)::text FROM zone_unit_embeddings;")?
            .trim(),
        "0",
        "embeddings cascade from zone_units"
    );
    Ok(())
}
