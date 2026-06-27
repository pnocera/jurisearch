//! P8 acceptance (design §8): the writable-app soft-reference model + validator.
//!
//! - a PIN by `document_id` keeps resolving across an incremental AND a re-baseline (INV-4 —
//!   supersession retains old version rows, so the re-baseline package includes the pinned version);
//! - a LOGICAL reference (`version_group` + `as_of_date`) resolves to the version whose validity window
//!   contains the as-of date, and is flagged `changed` when a newer version supersedes it;
//! - a malformed reference is `invalid`, not `missing`;
//! - the server reload itself NEVER mutates `jurisearch_app` — only the validator writes the
//!   `resolved_*`/`validation_status` columns, after the cursor advances.

use std::collections::BTreeMap;

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::event::EventKind;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental, build_rebaseline,
};
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::reference::validate_references;
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_syncd::{apply_baseline, apply_incremental, apply_rebaseline};

fn vector(seed: &str) -> String {
    format!(
        "[{}]",
        (0..1024).map(|_| seed).collect::<Vec<_>>().join(",")
    )
}

fn baseline_params() -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: "core-2026-06-27-g0001".to_owned(),
        builder_run_id: "b0".to_owned(),
        created_at: "2026-06-27T00:00:00Z".to_owned(),
        embedding_fingerprint: "fp".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn incremental_params(run: &str) -> IncrementalParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    IncrementalParams {
        builder_run_id: run.to_owned(),
        created_at: "2026-06-27T01:00:00Z".to_owned(),
        embedding_fingerprint: "fp".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn rebaseline_params() -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: "core-2026-06-27-g0002".to_owned(),
        builder_run_id: "rb0".to_owned(),
        created_at: "2026-06-27T02:00:00Z".to_owned(),
        embedding_fingerprint: "fp2".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

/// Seed a `legi` article V1 (version_group VG-A1, valid 2020-01-01 → open) + a chunk + embedding.
fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, version_group, citation, \
           title, body, valid_from, valid_to, source_payload_hash, canonical_json) \
         VALUES ('legi:A1@2020','legi','article','A1','VG-A1','Art L1','Art L1','texte v1', \
           '2020-01-01',NULL,'sha256:a1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('legi:A1@2020#0','legi:A1@2020',0,'texte v1','ctx v1','sha256:c','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('legi:A1@2020#0','fp','{}'::vector,'m',1024);",
        vector("0.01"),
    ))?;
    Ok(())
}

fn mutate(
    producer: &ManagedPostgres,
    sql: &str,
    table: &str,
    op: EventKind,
    scope_key: &str,
) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("mutation-run", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope("core", table, op, scope_kind::DOCUMENT, scope_key),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// The `validation_status` of a reference, located by `target_kind` + an identity hint.
fn status_of(client: &ManagedPostgres, predicate: &str) -> Result<String, StorageError> {
    client.execute_sql(&format!(
        "SELECT validation_status FROM jurisearch_app.app_reference WHERE {predicate};"
    ))
}

fn resolved_of(client: &ManagedPostgres, predicate: &str) -> Result<String, StorageError> {
    client.execute_sql(&format!(
        "SELECT coalesce(resolved_document_id,'NONE') FROM jurisearch_app.app_reference WHERE {predicate};"
    ))
}

/// A COMPLETE snapshot of every `app_reference` row (full row shape, ordered) — so a "reload never
/// mutates jurisearch_app" assertion cannot false-green by missing a column.
fn app_snapshot(client: &ManagedPostgres) -> Result<String, StorageError> {
    client.execute_sql(
        "SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY reference_id)::text, '[]') \
         FROM jurisearch_app.app_reference AS r;",
    )
}

#[test]
fn references_resolve_validate_and_survive_incrementals_and_rebaselines() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };

    // Producer: seed + baseline.
    let proot = tempfile::Builder::new()
        .prefix("js-ref-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-ref-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &StubSigner,
        &baseline_params(),
    )
    .expect("baseline");

    // Client: apply baseline, then insert the app references.
    let croot = tempfile::Builder::new()
        .prefix("js-ref-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    apply_baseline(&client, base_art.path(), &AcceptAllVerifier).expect("apply baseline");

    client.execute_sql(
        "INSERT INTO jurisearch_app.app_reference (target_kind, corpus, document_id) \
           VALUES ('document_version','core','legi:A1@2020'); \
         INSERT INTO jurisearch_app.app_reference \
             (target_kind, corpus, source, source_uid, version_group, as_of_date) \
           VALUES ('logical_article','core','legi','A1','VG-A1','2023-01-01'); \
         INSERT INTO jurisearch_app.app_reference (target_kind, corpus) \
           VALUES ('document_version','core');",
    )?;

    const PIN: &str = "target_kind='document_version' AND document_id='legi:A1@2020'";
    const LOGICAL: &str = "target_kind='logical_article'";
    const BAD: &str = "target_kind='document_version' AND document_id IS NULL";

    // First validation: pin resolves to V1; logical (as_of 2023) resolves to V1 (valid 2020 → open);
    // the malformed row is `invalid`.
    let report = validate_references(&client, "core")?;
    assert!(report.installed);
    assert_eq!(report.invalid, 1);
    assert_eq!(status_of(&client, PIN)?.trim(), "resolved");
    assert_eq!(resolved_of(&client, PIN)?.trim(), "legi:A1@2020");
    assert_eq!(status_of(&client, LOGICAL)?.trim(), "resolved");
    assert_eq!(resolved_of(&client, LOGICAL)?.trim(), "legi:A1@2020");
    assert_eq!(status_of(&client, BAD)?.trim(), "invalid");

    // --- A new version V2 lands: V1 closes at 2022, V2 opens at 2022 (supersession). One scope per
    //     changed row (the closing V1, the new V2 document, the new V2 chunks). ---
    mutate(
        &producer,
        "UPDATE documents SET valid_to='2022-01-01' WHERE document_id='legi:A1@2020';",
        "documents",
        EventKind::Upsert,
        "legi:A1@2020",
    )?;
    mutate(
        &producer,
        "INSERT INTO documents (document_id, source, kind, source_uid, version_group, citation, \
           title, body, valid_from, valid_to, source_payload_hash, canonical_json) \
         VALUES ('legi:A1@2022','legi','article','A1','VG-A1','Art L1','Art L1','texte v2', \
           '2022-01-01',NULL,'sha256:a2','{}');",
        "documents",
        EventKind::Upsert,
        "legi:A1@2022",
    )?;
    mutate(
        &producer,
        "INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('legi:A1@2022#0','legi:A1@2022',0,'texte v2','ctx v2','sha256:c2','c1','fp'); \
         INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('legi:A1@2022#0','fp','REEMBED'::vector,'m',1024);"
            .replace("REEMBED", &vector("0.02"))
            .as_str(),
        "chunks",
        EventKind::ReplaceSet,
        "legi:A1@2022",
    )?;
    let inc = tempfile::Builder::new()
        .prefix("js-ref-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("inc")
    .expect("inc changes");

    // The apply itself must NOT touch jurisearch_app (FULL-ROW snapshot around it — every column).
    let before_inc = app_snapshot(&client)?;
    apply_incremental(&client, inc.path(), &AcceptAllVerifier).expect("apply inc");
    let after_inc = app_snapshot(&client)?;
    assert_eq!(
        before_inc, after_inc,
        "an incremental apply never mutates jurisearch_app (any column)"
    );

    // Re-validate: pin still resolves to V1 (retained); logical (as_of 2023) now resolves to V2 and is
    // flagged `changed` (its prior resolution was V1).
    validate_references(&client, "core")?;
    assert_eq!(status_of(&client, PIN)?.trim(), "resolved");
    assert_eq!(resolved_of(&client, PIN)?.trim(), "legi:A1@2020");
    assert_eq!(status_of(&client, LOGICAL)?.trim(), "changed");
    assert_eq!(resolved_of(&client, LOGICAL)?.trim(), "legi:A1@2022");

    // --- A re-baseline (re-embed fp→fp2) on a NEW generation. The pin must survive (INV-4). ---
    mutate(
        &producer,
        "UPDATE chunks SET embedding_fingerprint='fp2'; \
         UPDATE chunk_embeddings SET embedding_fingerprint='fp2';",
        "chunks",
        EventKind::ReplaceSet,
        "legi:A1@2020",
    )?;
    let rb = tempfile::Builder::new()
        .prefix("js-ref-rb.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_rebaseline(
        &producer,
        "core",
        rb.path(),
        &StubSigner,
        &rebaseline_params(),
    )
    .expect("rebaseline");
    // The re-baseline reload must ALSO leave jurisearch_app byte-identical (the core INV-4/5 contract).
    let before_rb = app_snapshot(&client)?;
    apply_rebaseline(&client, rb.path(), &AcceptAllVerifier).expect("apply rebaseline");
    let after_rb = app_snapshot(&client)?;
    assert_eq!(
        before_rb, after_rb,
        "a re-baseline reload never mutates jurisearch_app (any column)"
    );

    validate_references(&client, "core")?;
    assert_eq!(
        status_of(&client, PIN)?.trim(),
        "resolved",
        "a pinned document_id survives the re-baseline (supersession retains it)"
    );
    assert_eq!(resolved_of(&client, PIN)?.trim(), "legi:A1@2020");
    // The logical ref now re-resolves to V2 again — unchanged from the prior V2 → `resolved`.
    assert_eq!(status_of(&client, LOGICAL)?.trim(), "resolved");
    assert_eq!(resolved_of(&client, LOGICAL)?.trim(), "legi:A1@2022");
    // The validator stamped the new active generation.
    let stamped_gen = client.execute_sql(&format!(
        "SELECT resolved_generation FROM jurisearch_app.app_reference WHERE {PIN};"
    ))?;
    assert_eq!(
        stamped_gen.trim(),
        "core_g0002",
        "stamped against the re-baselined generation"
    );
    Ok(())
}
