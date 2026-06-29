//! work/10 M1-B S1 parity: prove each generalized read helper's client-source `*_with_client` variant
//! returns BYTE-IDENTICAL output to the existing `&ManagedPostgres` shim. This is the highest-risk part
//! of the generalization — the helpers that previously shelled their SQL through `psql -qAt`
//! (`execute_sql`) now run it over a `postgres::Client` via `simple_query_text`, and the snapshot-backed
//! `zone_retrieval_coverage_json` now has a direct-client twin.
//!
//! Two layers:
//! * the SEEDED-DATA case ([`with_client_read_helpers_match_managed_postgres_shims_on_seeded_data`])
//!   inserts non-empty fixtures exercising every translated helper — row ordering, the cursor/`since`
//!   branches, JSON/JSONB values, boolean/numeric counts, and null values inside the returned JSON — so a
//!   byte-for-byte mismatch in the psql→client translation is caught, not just empty-result shape drift;
//! * the EMPTY case ([`with_client_read_helpers_match_managed_postgres_shims_on_empty_db`]) keeps the
//!   freshly-migrated empty-database coverage as a separate guard.
//!
//! A THIRD test ([`db_client_source_pins_public_search_path_despite_role_default`]) is the M1-B BLOCKER
//! regression: it proves a [`DbClientSource`] client pins `search_path = public` even when the connecting
//! role carries a non-public role-level default, so a translated helper still reads/writes `public`.
//!
//! PG-asset-gated: needs `pgvector` + `pg_search` discoverable via `JURISEARCH_PG_CONFIG`. When absent,
//! every test skips (returns `Ok(())`) — exactly like the other storage integration tests. None of these
//! tests connect to any live/external PostgreSQL; they only use the loopback managed harness.

mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    backend::{ConnectionConfig, DbClientSource},
    legislation_citations::{
        InsertCitationOccurrence, insert_citation_occurrence_with_client,
        legislation_citations_coverage_json, legislation_citations_coverage_json_with_client,
        load_archived_decisions_with_visa_json, load_archived_decisions_with_visa_json_with_client,
        load_pending_citation_resolutions_json, load_pending_citation_resolutions_json_with_client,
        update_citation_resolution_with_client, upsert_citation_resolution_pending_with_client,
    },
    official_api_archive::{InsertOfficialApiResponse, insert_official_api_response_with_client},
    runtime::{ManagedPostgres, StorageError},
    zone_units::{
        EnrichZoneOrder, ZoneUnitEmbeddingInsert, ZoneUnitRow, enrich_zone_candidates_json,
        enrich_zone_candidates_json_with_client, insert_zone_unit_embeddings,
        load_derivable_decision_zones_json, load_derivable_decision_zones_json_with_client,
        replace_zone_units_for_document, zone_retrieval_coverage_json,
        zone_retrieval_coverage_with_client,
    },
};
use serde_json::json;

const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
const BUILDER: &str = "zone-units:v1";
/// The managed harness's superuser role (mirrors the private `runtime::SUPERUSER`). Connections by this
/// role are what the parity/regression tests open.
const HARNESS_ROLE: &str = "postgres";

/// Seed one Cassation decision plus its `decision_zones` overlay row (source `cass`). `text_hash = None`
/// models the lazy / pre-hash-fix rows (a nullable column). `zones_json` is stored as JSONB.
fn seed_decision(
    postgres: &ManagedPostgres,
    document_id: &str,
    pourvoi: &str,
    status: &str,
    text_hash: Option<&str>,
    zones_json: &str,
) -> Result<(), StorageError> {
    let hash_literal = match text_hash {
        Some(hash) => format!("'{hash}'"),
        None => "NULL".to_owned(),
    };
    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('{document_id}', 'cass', 'decision', '{document_id}', 'Cass. civ. {pourvoi}', \
            'Arret', 'corps de la decision', '2024-01-01', 'sha256:{document_id}', \
            '{{\"case_numbers\":[\"{pourvoi}\"]}}'); \
         INSERT INTO decision_zones \
           (document_id, provider, provider_decision_id, source_uid, status, \
            fetched_at, expires_at, text_hash, offset_unit, zones_json, raw_json) \
         VALUES \
           ('{document_id}', 'judilibre', 'jdl:{document_id}', '{document_id}', '{status}', \
            now(), now() + interval '30 days', {hash_literal}, 'char', \
            '{zones_json}'::jsonb, '{{}}'::jsonb);",
    ))?;
    Ok(())
}

/// Seed a non-empty working set that flows through EVERY translated read helper, then build a fresh client
/// the parity assertions reuse.
fn seed_parity_fixtures(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    // Zone overlay: four cass decisions with parser-valid pourvois in distinct states.
    // - 0001: ok + hash, zones with fragments  -> derivable once (units seeded below make it current).
    // - 0002: ok + NULL hash                    -> an enrich candidate (null-hash ok), a NULLABLE column.
    // - 0003: not_found + NULL hash             -> a decision_zones row in a non-ok status.
    // - 0004: ok + hash, zones, NO units        -> stays derivable (drives the non-empty derivable case).
    let zones = r#"{"motivations":[{"start":0,"end":5,"text":"motif un"},{"start":6,"end":10,"text":"motif deux"}],"moyens":[],"dispositif":[]}"#;
    seed_decision(
        postgres,
        "cass:JURITEXT0001",
        "12-34567",
        "ok",
        Some("hash-1"),
        zones,
    )?;
    seed_decision(postgres, "cass:JURITEXT0002", "22-22222", "ok", None, "{}")?;
    seed_decision(
        postgres,
        "cass:JURITEXT0003",
        "33-33333",
        "not_found",
        None,
        "{}",
    )?;
    seed_decision(
        postgres,
        "cass:JURITEXT0004",
        "44-44444",
        "ok",
        Some("hash-4"),
        zones,
    )?;

    // Materialize zone_units (+ one embedding) for 0001 so the coverage counts and the JSONB aggregates
    // are non-trivial — and 0001 is no longer derivable while 0004 still is.
    let units = vec![
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
    ];
    replace_zone_units_for_document(postgres, "cass:JURITEXT0001", &units, None)?;
    let vector = vector_literal(0);
    insert_zone_unit_embeddings(
        postgres,
        &[ZoneUnitEmbeddingInsert {
            zone_unit_id: "cass:JURITEXT0001#motivations#0",
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal: &vector,
            model: "bge-m3",
            dimension: 1024,
        }],
        None,
    )?;
    // NB: the `zone_embedding` index_manifest row is intentionally NOT seeded, so the coverage JSON
    // carries `"embedding_manifest": null` — exercising NULL rendering inside the returned JSON.

    // Legislation-citation side: two archived /decision responses carrying a visa ARRAY (JSONB), two
    // citation occurrences, and two pending resolutions across two corpora (the composite cursor branch).
    let mut client = postgres.client()?;
    let visa = json!([{ "title": "Article 609 du code de procédure civile." }]);
    let key = "legi-cite:art609cpc";
    for doc in ["cass:JURITEXT0001", "cass:JURITEXT0002"] {
        let response = json!({ "id": doc, "visa": visa });
        let response_id = insert_official_api_response_with_client(
            &mut client,
            &InsertOfficialApiResponse {
                provider: "judilibre",
                api_environment: "production",
                endpoint: "/cassation/judilibre/v1.0/decision",
                http_method: "GET",
                subject_document_id: Some(doc),
                subject_source_uid: Some(doc),
                provider_object_id: Some(doc),
                citation_key: None,
                corpus: None,
                request_fingerprint: doc,
                request_url: None,
                request_json: &json!({}),
                request_body: None,
                outcome: "ok",
                http_status: Some(200),
                response_body: &response.to_string(),
                response_json: Some(&response),
                response_body_sha256: "sha256:x",
                error: None,
                run_id: None,
                code_version: Some("test"),
            },
            None,
        )?;
        insert_citation_occurrence_with_client(
            &mut client,
            &InsertCitationOccurrence {
                decision_document_id: doc,
                decision_source_uid: doc,
                source_response_id: response_id,
                visa_index: 0,
                citation_key: key,
                article_number_raw: Some("609"),
                article_number_norm: "609",
                code_name_raw: Some("code de procédure civile"),
                code_name_norm: "code de procédure civile",
                canonical_query: "609 code de procédure civile",
                legifrance_url: None,
                raw_title: "Article 609 du code de procédure civile.",
                extraction_method: "visa_title_regex",
            },
            None,
        )?;
    }
    // Deduped pending resolution for corpus `core` (derived from the cass document), plus an independent
    // `inpi` resolution for the SAME citation_key — so the pending pager returns two rows and the
    // (corpus, citation_key) composite cursor branch is exercised.
    upsert_citation_resolution_pending_with_client(
        &mut client,
        key,
        "609",
        "code de procédure civile",
        "609 code de procédure civile",
        "cass:JURITEXT0001",
        None,
    )?;
    postgres.execute_sql(&format!(
        "INSERT INTO legislation_citation_resolutions \
            (corpus, citation_key, article_number_norm, code_name_norm, canonical_query) \
         VALUES ('inpi', '{key}', '609', 'code de procédure civile', '609 cpc');"
    ))?;
    Ok(())
}

/// Assert the shim and the client-source variant agree byte-for-byte on the seeded data, across the
/// cursor / `since` / order / retry branches of every translated helper.
#[test]
fn with_client_read_helpers_match_managed_postgres_shims_on_seeded_data() -> Result<(), StorageError>
{
    let Some(pg_config) = discover_pg_config("client-source parity (seeded)")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-client-source-parity-seeded.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_parity_fixtures(&postgres)?;

    let mut client = postgres.client()?;

    // --- enrich_zone_candidates_json: default, then the cursor + `since` + Recent-order branch ---------
    let enrich_default =
        enrich_zone_candidates_json(&postgres, "cass", None, None, 25, EnrichZoneOrder::Oldest)?;
    assert_eq!(
        enrich_default,
        enrich_zone_candidates_json_with_client(
            &mut client,
            "cass",
            None,
            None,
            25,
            EnrichZoneOrder::Oldest
        )?,
        "enrich_zone_candidates_json (default) must match the shim"
    );
    assert!(
        enrich_default.contains("cass:JURITEXT0002"),
        "fixture must make the default enrich page non-empty: {enrich_default}"
    );
    assert_eq!(
        enrich_zone_candidates_json(
            &postgres,
            "cass",
            Some("cass:JURITEXT0099"),
            Some("2999-01-01T00:00:00Z"),
            10,
            EnrichZoneOrder::Recent
        )?,
        enrich_zone_candidates_json_with_client(
            &mut client,
            "cass",
            Some("cass:JURITEXT0099"),
            Some("2999-01-01T00:00:00Z"),
            10,
            EnrichZoneOrder::Recent
        )?,
        "enrich_zone_candidates_json (cursor + since + Recent) must match the shim"
    );

    // --- load_derivable_decision_zones_json: default, rebuild=true, and the cursor branch -------------
    let derivable_default =
        load_derivable_decision_zones_json(&postgres, BUILDER, false, None, 50)?;
    assert_eq!(
        derivable_default,
        load_derivable_decision_zones_json_with_client(&mut client, BUILDER, false, None, 50)?,
        "load_derivable_decision_zones_json (default) must match the shim"
    );
    assert!(
        derivable_default.contains("cass:JURITEXT0004"),
        "fixture must make the derivable page non-empty: {derivable_default}"
    );
    assert_eq!(
        load_derivable_decision_zones_json(&postgres, BUILDER, true, None, 50)?,
        load_derivable_decision_zones_json_with_client(&mut client, BUILDER, true, None, 50)?,
        "load_derivable_decision_zones_json (rebuild) must match the shim"
    );
    assert_eq!(
        load_derivable_decision_zones_json(
            &postgres,
            BUILDER,
            false,
            Some("cass:JURITEXT0003"),
            50
        )?,
        load_derivable_decision_zones_json_with_client(
            &mut client,
            BUILDER,
            false,
            Some("cass:JURITEXT0003"),
            50
        )?,
        "load_derivable_decision_zones_json (cursor) must match the shim"
    );

    // --- zone_retrieval_coverage: counts, JSONB aggregates, and the NULL embedding_manifest -----------
    let coverage = zone_retrieval_coverage_json(&postgres)?;
    assert_eq!(
        coverage,
        zone_retrieval_coverage_with_client(&mut client)?,
        "zone_retrieval_coverage_with_client must match the snapshot-backed shim on the public working set"
    );
    let coverage_json: serde_json::Value = serde_json::from_str(&coverage).expect("coverage JSON");
    assert_eq!(coverage_json["zone_units"]["total"], 2, "{coverage}");
    assert_eq!(coverage_json["embeddings"]["total"], 1, "{coverage}");
    assert_eq!(
        coverage_json["embeddings"]["units_pending"], 1,
        "{coverage}"
    );
    assert!(
        coverage_json["embedding_manifest"].is_null(),
        "the unseeded manifest must render as JSON null: {coverage}"
    );

    // --- legislation_citations_coverage_json: counts + by_legifrance_status aggregate -----------------
    let citations_coverage = legislation_citations_coverage_json(&postgres)?;
    assert_eq!(
        citations_coverage,
        legislation_citations_coverage_json_with_client(&mut client)?,
        "legislation_citations_coverage_json must match the shim"
    );
    let citations_json: serde_json::Value =
        serde_json::from_str(&citations_coverage).expect("citations coverage JSON");
    assert_eq!(citations_json["occurrences"], 2, "{citations_coverage}");
    assert_eq!(
        citations_json["unique_citations"], 2,
        "{citations_coverage}"
    );

    // --- load_archived_decisions_with_visa_json: default, cursor, and the limit boundary --------------
    let archived = load_archived_decisions_with_visa_json(&postgres, None, 20)?;
    assert_eq!(
        archived,
        load_archived_decisions_with_visa_json_with_client(&mut client, None, 20)?,
        "load_archived_decisions_with_visa_json (default) must match the shim"
    );
    assert!(
        archived.contains("Article 609"),
        "archived visa fixture must be non-empty: {archived}"
    );
    assert_eq!(
        load_archived_decisions_with_visa_json(&postgres, Some("cass:JURITEXT0001"), 20)?,
        load_archived_decisions_with_visa_json_with_client(
            &mut client,
            Some("cass:JURITEXT0001"),
            20
        )?,
        "load_archived_decisions_with_visa_json (cursor) must match the shim"
    );
    assert_eq!(
        load_archived_decisions_with_visa_json(&postgres, None, 1)?,
        load_archived_decisions_with_visa_json_with_client(&mut client, None, 1)?,
        "load_archived_decisions_with_visa_json (limit boundary) must match the shim"
    );

    // --- load_pending_citation_resolutions_json: pending, retry_errors, and the composite cursor ------
    let pending = load_pending_citation_resolutions_json(&postgres, None, false, 20)?;
    assert_eq!(
        pending,
        load_pending_citation_resolutions_json_with_client(&mut client, None, false, 20)?,
        "load_pending_citation_resolutions_json (pending) must match the shim"
    );
    assert!(
        pending.contains("\"corpus\":\"core\"") && pending.contains("\"corpus\":\"inpi\""),
        "both per-corpus pending resolutions must be present: {pending}"
    );
    assert_eq!(
        load_pending_citation_resolutions_json(&postgres, None, true, 20)?,
        load_pending_citation_resolutions_json_with_client(&mut client, None, true, 20)?,
        "load_pending_citation_resolutions_json (retry_errors) must match the shim"
    );
    let composite_cursor = "core\u{1f}legi-cite:art609cpc";
    assert_eq!(
        load_pending_citation_resolutions_json(&postgres, Some(composite_cursor), false, 20)?,
        load_pending_citation_resolutions_json_with_client(
            &mut client,
            Some(composite_cursor),
            false,
            20
        )?,
        "load_pending_citation_resolutions_json (composite cursor) must match the shim"
    );

    Ok(())
}

/// The original empty-database parity: each helper, run via the shim and via the client-source variant
/// over a fresh `pg.client()`, must agree byte-for-byte on the same freshly-migrated (empty) database.
#[test]
fn with_client_read_helpers_match_managed_postgres_shims_on_empty_db() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("client-source parity (empty)")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-client-source-parity-empty.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let mut client = postgres.client()?;
    assert_eq!(
        enrich_zone_candidates_json(&postgres, "cass", None, None, 25, EnrichZoneOrder::Oldest)?,
        enrich_zone_candidates_json_with_client(
            &mut client,
            "cass",
            None,
            None,
            25,
            EnrichZoneOrder::Oldest
        )?,
        "enrich_zone_candidates_json client-source variant must match the shim"
    );
    assert_eq!(
        enrich_zone_candidates_json(
            &postgres,
            "inca",
            Some("cass:JURITEXT0009"),
            Some("2020-01-01T00:00:00Z"),
            10,
            EnrichZoneOrder::Recent
        )?,
        enrich_zone_candidates_json_with_client(
            &mut client,
            "inca",
            Some("cass:JURITEXT0009"),
            Some("2020-01-01T00:00:00Z"),
            10,
            EnrichZoneOrder::Recent
        )?,
        "enrich_zone_candidates_json (cursor+since variant) must match the shim"
    );
    assert_eq!(
        load_derivable_decision_zones_json(&postgres, "zone-units:v1", false, None, 50)?,
        load_derivable_decision_zones_json_with_client(
            &mut client,
            "zone-units:v1",
            false,
            None,
            50
        )?,
        "load_derivable_decision_zones_json client-source variant must match the shim"
    );
    assert_eq!(
        zone_retrieval_coverage_json(&postgres)?,
        zone_retrieval_coverage_with_client(&mut client)?,
        "zone_retrieval_coverage_with_client must match the snapshot-backed shim on the public working set"
    );
    assert_eq!(
        legislation_citations_coverage_json(&postgres)?,
        legislation_citations_coverage_json_with_client(&mut client)?,
        "legislation_citations_coverage_json client-source variant must match the shim"
    );
    assert_eq!(
        load_archived_decisions_with_visa_json(&postgres, None, 20)?,
        load_archived_decisions_with_visa_json_with_client(&mut client, None, 20)?,
        "load_archived_decisions_with_visa_json client-source variant must match the shim"
    );
    assert_eq!(
        load_pending_citation_resolutions_json(&postgres, None, true, 20)?,
        load_pending_citation_resolutions_json_with_client(&mut client, None, true, 20)?,
        "load_pending_citation_resolutions_json client-source variant must match the shim"
    );

    Ok(())
}

/// M1-B BLOCKER regression: a [`DbClientSource`] client must pin `search_path = public`, so a translated
/// helper reads (and writes) the `public` tables even when the connecting role carries a NON-public
/// role-level `search_path` default — the exact shared-external-server hazard
/// (`ALTER ROLE … SET search_path`). Without the pin, every unqualified helper would silently resolve to
/// the wrong schema. This uses ONLY the loopback managed harness — no live/external PostgreSQL.
#[test]
fn db_client_source_pins_public_search_path_despite_role_default() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("client-source search_path pin")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-client-source-searchpath.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // The REAL public row the helper must see (schema-qualified, so it lands in public regardless of the
    // default we install next).
    postgres.execute_sql(
        "INSERT INTO public.legislation_citation_resolutions \
            (corpus, citation_key, article_number_norm, code_name_norm, canonical_query) \
         VALUES ('core', 'legi-cite:public-real', '609', 'code de procédure civile', '609 cpc');",
    )?;

    // The adversarial shared-server topology: a non-public DECOY schema holding a same-named/same-shaped
    // table with DIFFERENT rows, made the connecting role's default search_path. Everything is
    // schema-qualified, so it is unaffected by the default we are about to change.
    postgres.execute_sql(&format!(
        "CREATE SCHEMA decoy; \
         CREATE TABLE decoy.legislation_citation_resolutions \
            (LIKE public.legislation_citation_resolutions INCLUDING ALL); \
         INSERT INTO decoy.legislation_citation_resolutions \
            (corpus, citation_key, article_number_norm, code_name_norm, canonical_query) \
         VALUES ('core', 'legi-cite:DECOY-A', '1', 'x', 'x'), \
                ('core', 'legi-cite:DECOY-B', '2', 'y', 'y'); \
         ALTER ROLE {HARNESS_ROLE} SET search_path TO decoy;"
    ))?;

    let pending_keys = |raw: &str| -> Vec<String> {
        let value: serde_json::Value = serde_json::from_str(raw).expect("pending JSON");
        value["citations"]
            .as_array()
            .expect("citations array")
            .iter()
            .map(|c| c["citation_key"].as_str().expect("citation_key").to_owned())
            .collect()
    };

    // Hazard is real: a NON-pinned client (stock default, now the role's `decoy` default) resolves the
    // unqualified helper SQL to the decoy schema.
    let mut raw = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let raw_keys = pending_keys(&load_pending_citation_resolutions_json_with_client(
        &mut raw, None, false, 20,
    )?);
    assert_eq!(
        raw_keys,
        vec![
            "legi-cite:DECOY-A".to_owned(),
            "legi-cite:DECOY-B".to_owned()
        ],
        "without the pin a role-level search_path default redirects the helper off public"
    );

    // The DbClientSource contract pins public for EVERY client it hands out: the self-managed
    // `ManagedPostgres::client`, the `DbClientSource for ManagedPostgres` impl, and the external
    // `ConnectionConfig::connect` path (the producer's real seam) all read the public table, never decoy.
    let external = ConnectionConfig {
        host: "127.0.0.1".to_owned(),
        port: postgres.port,
        dbname: postgres.database.clone(),
        user: HARNESS_ROLE.to_owned(),
        password: None,
        application_name: "jurisearch-m1b-searchpath-test".to_owned(),
    };
    let mut pinned_clients: Vec<(&str, postgres::Client)> = vec![
        ("ManagedPostgres::client", postgres.client()?),
        (
            "DbClientSource for ManagedPostgres",
            DbClientSource::client(&postgres)?,
        ),
        (
            "ConnectionConfig::connect (external path)",
            external.client()?,
        ),
    ];
    for (label, client) in &mut pinned_clients {
        let keys = pending_keys(&load_pending_citation_resolutions_json_with_client(
            client, None, false, 20,
        )?);
        assert_eq!(
            keys,
            vec!["legi-cite:public-real".to_owned()],
            "{label}: pinned search_path=public must read the public table, not the decoy"
        );
    }

    // A translated WRITE helper must also land in public: update the public resolution via a pinned
    // client, then confirm (schema-qualified) that public changed and decoy is untouched.
    let (_, write_client) = &mut pinned_clients[0];
    update_citation_resolution_with_client(
        write_client,
        "core",
        "legi-cite:public-real",
        "ok",
        None,
        None,
        None,
        None,
    )?;
    let public_status = postgres.execute_sql(
        "SELECT legifrance_status FROM public.legislation_citation_resolutions \
         WHERE citation_key = 'legi-cite:public-real';",
    )?;
    assert_eq!(
        public_status.trim(),
        "ok",
        "the pinned write must hit the public table"
    );
    let decoy_ok = postgres.execute_sql(
        "SELECT count(*)::text FROM decoy.legislation_citation_resolutions \
         WHERE legifrance_status = 'ok';",
    )?;
    assert_eq!(
        decoy_ok.trim(),
        "0",
        "the pinned write must NOT touch the decoy table"
    );

    Ok(())
}
