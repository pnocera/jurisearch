mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    dense::DenseRebuildSpec,
    runtime::{ManagedPostgres, StorageError},
    zone_units::{
        ZONE_UNIT_VECTOR_INDEX_NAME, ZoneUnitEmbeddingInsert, ZoneUnitRow,
        enrich_zone_candidates_json, finalize_zone_dense_rebuild, insert_zone_unit_embeddings,
        load_derivable_decision_zones_json, load_zone_unit_embedding_inputs,
        replace_zone_units_for_document, zone_retrieval_coverage_json,
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
    seed_decision_full(postgres, document_id, "cass", "decision", pourvoi, status, text_hash, zones_json, false)
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

    // Migrations must have reached v15 (zone tables present).
    let schema = postgres.execute_sql(
        "SELECT (value->>'schema_version') FROM index_manifest WHERE key = 'schema';",
    )?;
    assert_eq!(schema.trim(), "15");

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
    replace_zone_units_for_document(&postgres, "cass:JURITEXT0001", &rows)?;
    let unit_count = postgres.execute_sql("SELECT count(*)::text FROM zone_units;")?;
    assert_eq!(unit_count.trim(), "3");

    // After derivation, the decision is no longer derivable (units current).
    let none: serde_json::Value =
        serde_json::from_str(&load_derivable_decision_zones_json(&postgres, BUILDER, false, None, 100)?)
            .expect("derivable JSON");
    assert_eq!(none["candidates"].as_array().expect("candidates").len(), 0);
    // A builder-version bump makes it derivable again.
    let stale: serde_json::Value = serde_json::from_str(&load_derivable_decision_zones_json(
        &postgres, "zone-units:v2", false, None, 100,
    )?)
    .expect("derivable JSON");
    assert_eq!(stale["candidates"].as_array().expect("candidates").len(), 1);

    // Embedding inputs: 3 pending units in stable order.
    let inputs = load_zone_unit_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
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
        finalize_zone_dense_rebuild(&postgres, &spec).unwrap_err(),
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
    assert_eq!(insert_zone_unit_embeddings(&postgres, &inserts)?, 3);

    let report = finalize_zone_dense_rebuild(&postgres, &spec)?;
    assert_eq!(report.zone_units, 3);
    assert_eq!(report.embeddings, 3);
    assert_eq!(report.index_name, ZONE_UNIT_VECTOR_INDEX_NAME);

    let index = postgres.execute_sql(&format!(
        "SELECT indexname FROM pg_indexes WHERE schemaname='public' AND indexname='{ZONE_UNIT_VECTOR_INDEX_NAME}';",
    ))?;
    assert_eq!(index.trim(), ZONE_UNIT_VECTOR_INDEX_NAME);

    // Coverage block reflects the seeded state.
    let coverage: serde_json::Value =
        serde_json::from_str(&zone_retrieval_coverage_json(&postgres)?).expect("coverage JSON");
    assert_eq!(coverage["zone_units"]["total"], 3);
    assert_eq!(coverage["zone_units"]["decisions"], 1);
    assert_eq!(coverage["embeddings"]["total"], 3);
    assert_eq!(coverage["embeddings"]["units_pending"], 0);

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
    seed_decision(&postgres, "cass:WITHHASH", "11-11111", "ok", Some("h"), "{}")?;
    seed_decision(&postgres, "cass:NULLHASH", "22-22222", "ok", None, "{}")?;

    let page: serde_json::Value =
        serde_json::from_str(&enrich_zone_candidates_json(&postgres, "cass", None, None, 100)?)
            .expect("candidates JSON");
    let ids: Vec<&str> = page["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| c["document_id"].as_str().expect("id"))
        .collect();
    assert!(ids.contains(&"cass:NULLHASH"), "null-hash ok row must be re-enriched: {ids:?}");
    assert!(!ids.contains(&"cass:WITHHASH"), "hashed fresh ok row must be skipped: {ids:?}");

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
        &postgres, "cass:EXPIRED", "cass", "decision", "33-33333", "ok", Some("h"), "{}", true,
    )?;

    let derivable: serde_json::Value =
        serde_json::from_str(&load_derivable_decision_zones_json(&postgres, BUILDER, false, None, 100)?)
            .expect("derivable JSON");
    assert_eq!(
        derivable["candidates"].as_array().expect("candidates").len(),
        0,
        "expired ok row must not be derivable"
    );
    let enrich: serde_json::Value =
        serde_json::from_str(&enrich_zone_candidates_json(&postgres, "cass", None, None, 100)?)
            .expect("candidates JSON");
    let ids: Vec<&str> = enrich["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| c["document_id"].as_str().expect("id"))
        .collect();
    assert!(ids.contains(&"cass:EXPIRED"), "expired ok row must be a refresh candidate: {ids:?}");
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
        &postgres, "capp:FOREIGN", "capp", "decision", "44-44444", "ok", Some("h"), "{}", false,
    )?;
    let derivable: serde_json::Value =
        serde_json::from_str(&load_derivable_decision_zones_json(&postgres, BUILDER, false, None, 100)?)
            .expect("derivable JSON");
    assert_eq!(
        derivable["candidates"].as_array().expect("candidates").len(),
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
    let err = replace_zone_units_for_document(&postgres, "cass:DOCA", &[foreign]).unwrap_err();
    assert!(matches!(err, StorageError::Projection { .. }));
    let count = postgres.execute_sql("SELECT count(*)::text FROM zone_units;")?;
    assert_eq!(count.trim(), "0", "no units written on a rejected replace");
    Ok(())
}
