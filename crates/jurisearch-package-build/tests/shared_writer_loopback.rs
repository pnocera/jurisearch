//! work/09 P2B — the existing writer path (baseline + incremental apply) run against a standalone PG
//! through the WRITER role identity (not superuser), with the activation read-visibility postcondition
//! stamped by the writer, after which the READ role can read the active topology. The work/08 loopbacks
//! already prove the self-managed (superuser) path; these prove the same machinery under the writer
//! handle. Skips cleanly when the managed PG harness is absent.

use std::collections::BTreeMap;

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::event::EventKind;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental,
};
use jurisearch_storage::backend::{
    DEFAULT_OWNER_ROLE, DEFAULT_READ_ROLE, DEFAULT_WRITER_ROLE, ManagedPostgresBackend, RoleSpec,
    StorageBackend, provision_roles,
};
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_syncd::{
    BaselineApplyOutcome, IncrementalApplyOutcome, apply_baseline, apply_incremental, corpus_status,
};

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
        builder_run_id: "build-0".to_owned(),
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

fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, valid_to, source_payload_hash, canonical_json) \
         VALUES ('cass:D1','cass','decision','cass:D1','Cass','Arret','corps','2024-01-01',NULL, \
           'sha256:d1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:D1#0','cass:D1',0,'premier moyen','ctx premier moyen','sha256:c0','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:D1#0','fp','{}'::vector,'m',1024);",
        vector("0.01"),
    ))?;
    Ok(())
}

/// Producer mutation + its outbox emit in one transaction (a real writer).
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

fn start(
    prefix: &str,
    pg_config: &PgConfig,
) -> Result<(tempfile::TempDir, ManagedPostgres), StorageError> {
    let root = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
    postgres.run_migrations()?;
    Ok((root, postgres))
}

fn provision_client(client: &ManagedPostgres) -> Result<(), StorageError> {
    let mut superuser = client.client()?;
    provision_roles(&mut superuser, &RoleSpec::default(), &client.database)?;
    Ok(())
}

#[test]
fn writer_role_applies_baseline_and_incremental_and_read_role_sees_them() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };

    // Producer (superuser/public): seed + build a baseline.
    let (_proot, producer) = start("js-p2b-producer.", &pg_config)?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-p2b-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &StubSigner,
        &baseline_params(),
    )
    .expect("baseline build");

    // Client: a FRESH site PG, migrated + role-provisioned (the supported shared-server path).
    let (_croot, client) = start("js-p2b-client.", &pg_config)?;
    provision_client(&client)?;
    let backend = ManagedPostgresBackend::new(
        &client,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );

    // Apply the baseline through the WRITER role (a WriterHandle, not the superuser ManagedPostgres).
    let writer = backend.writer_handle()?;
    let outcome = apply_baseline(&writer, base_art.path(), &AcceptAllVerifier)
        .expect("baseline apply via writer");
    assert!(
        matches!(outcome, BaselineApplyOutcome::Applied { sequence: 1, .. }),
        "writer-role baseline applies: {outcome:?}"
    );
    assert_eq!(corpus_status(&writer)?[0].sequence, 1);

    // The READ role can read the new active topology (the writer stamped its visibility at activation).
    let mut read = backend.read_handle()?.client()?;
    let seq: i64 = read
        .query_one(
            "SELECT sequence FROM jurisearch_control.corpus_state WHERE corpus='core'",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(seq, 1, "read role sees the baseline cursor");
    let docs: i64 = read
        .query_one(
            "SELECT count(*) FROM jurisearch_server.documents WHERE document_id='cass:D1'",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(
        docs, 1,
        "read role reads the applied document through the view"
    );

    // Mutate the producer, build an incremental, and apply it through a FRESH writer handle.
    mutate(
        &producer,
        "UPDATE documents SET valid_to='2024-12-31' WHERE document_id='cass:D1';",
        "documents",
        EventKind::Upsert,
        "cass:D1",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-p2b-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("incremental build")
    .expect("incremental has changes");
    let writer2 = backend.writer_handle()?;
    apply_incremental(&writer2, inc_art.path(), &AcceptAllVerifier)
        .expect("incremental apply via writer");
    assert_eq!(
        corpus_status(&writer)?[0].sequence,
        2,
        "cursor advanced after incremental"
    );

    // The read role observes the incremental change (the closed `valid_to`).
    let mut read = backend.read_handle()?.client()?;
    let valid_to: Option<String> = read
        .query_one(
            "SELECT valid_to::text FROM jurisearch_server.documents WHERE document_id='cass:D1'",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(
        valid_to.as_deref(),
        Some("2024-12-31"),
        "read role sees the incremental's valid_to change"
    );

    // work/09 P3A: the writer RE-STAMPED readiness at the incremental (sequence 2), so the read role's
    // readiness LOOKUP resolves with NO write — proving the read path performs no query-time recompute
    // under the SELECT-only identity (a write attempt would fail; the lookup succeeds).
    let report =
        jurisearch_storage::ingest_accounting::load_query_readiness_with_client(&mut read)?;
    assert_eq!(report.embedding_coverage.covered, 1);
    assert_eq!(report.projection_coverage.covered, 1);

    Ok(())
}

#[test]
fn activation_through_writer_fails_without_read_membership() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };

    let (_proot, producer) = start("js-p2b-neg-producer.", &pg_config)?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-p2b-neg-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &StubSigner,
        &baseline_params(),
    )
    .expect("baseline build");

    let (_croot, client) = start("js-p2b-neg-client.", &pg_config)?;
    provision_client(&client)?;
    // Revoke the writer's read-role membership: the activation `SET LOCAL ROLE <read>` visibility probe
    // can then no longer assume the read identity, so the switch must abort.
    {
        let mut superuser = client.client()?;
        superuser
            .batch_execute(&format!(
                "REVOKE {DEFAULT_READ_ROLE} FROM {DEFAULT_WRITER_ROLE};"
            ))
            .map_err(StorageError::PostgresClient)?;
    }

    let backend = ManagedPostgresBackend::new(
        &client,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let writer = backend.writer_handle()?;
    let error = apply_baseline(&writer, base_art.path(), &AcceptAllVerifier)
        .expect_err("activation must fail when the writer cannot SET ROLE the read role");

    // Prove the failure is precisely the activation `SET LOCAL ROLE <read>` probe — SQLSTATE 42501
    // (insufficient_privilege) with a "set role" message — not an earlier writer-privilege regression
    // in schema creation / COPY / index build / activation DML.
    let db_error = sync_error_db(&error)
        .unwrap_or_else(|| panic!("expected a postgres DbError, got {error:?}"));
    assert_eq!(
        db_error.code(),
        &postgres::error::SqlState::INSUFFICIENT_PRIVILEGE,
        "expected 42501 from the SET ROLE probe, got {error:?}"
    );
    assert!(
        db_error.message().to_lowercase().contains("set role"),
        "the failing edge must be the SET ROLE probe: {}",
        db_error.message()
    );

    // The generation was BUILT but the switch rolled back, so it is left `building`, never `active`,
    // and the cursor is unchanged.
    let registry = client.execute_sql(
        "SELECT coalesce(string_agg(state, ','), 'none') FROM jurisearch_control.generation_registry \
         WHERE corpus='core';",
    )?;
    assert_eq!(
        registry.trim(),
        "building",
        "the generation is left building, never activated"
    );
    let cursor = client.execute_sql(
        "SELECT count(*)::text FROM jurisearch_control.corpus_state WHERE corpus='core';",
    )?;
    assert_eq!(
        cursor.trim(),
        "0",
        "a failed activation leaves the cursor unchanged"
    );

    Ok(())
}

#[test]
fn an_incremental_that_breaks_coverage_rolls_back_cursor_unchanged() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };

    // Producer + a clean baseline applied to a fresh client (cursor at sequence 1, fully ready).
    let (_proot, producer) = start("js-p3a-inc-producer.", &pg_config)?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-p3a-inc-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &StubSigner,
        &baseline_params(),
    )
    .expect("baseline build");

    let (_croot, client) = start("js-p3a-inc-client.", &pg_config)?;
    provision_client(&client)?;
    let backend = ManagedPostgresBackend::new(
        &client,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let writer = backend.writer_handle()?;
    apply_baseline(&writer, base_art.path(), &AcceptAllVerifier).expect("baseline apply");
    assert_eq!(corpus_status(&writer)?[0].sequence, 1);

    // Mutate the producer so the incremental is internally valid (a new decision + its chunk, scoped and
    // emitted) but leaves the chunk WITHOUT an embedding under the active fingerprint — so applying it
    // would make the active generation dense-incomplete. The restamp gate must refuse, inside the apply
    // transaction, AFTER advancing the cursor — proving the whole diff rolls back.
    mutate(
        &producer,
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, valid_to, source_payload_hash, canonical_json) \
         VALUES ('cass:D2','cass','decision','cass:D2','Cass','Arret','corps2','2024-02-01',NULL, \
           'sha256:d2','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:D2#0','cass:D2',0,'second moyen','ctx second moyen','sha256:c2','c1','fp');",
        "documents",
        EventKind::Upsert,
        "cass:D2",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-p3a-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("incremental build")
    .expect("incremental has changes");

    let writer2 = backend.writer_handle()?;
    let error = apply_incremental(&writer2, inc_art.path(), &AcceptAllVerifier).expect_err(
        "an incremental that breaks dense coverage must be refused by the restamp gate",
    );
    assert!(
        error.to_string().to_lowercase().contains("coverage"),
        "the apply fails on the readiness coverage gate: {error}"
    );

    // The cursor is unchanged (still sequence 1) and the diff rolled back: the active topology never sees
    // the new, un-embedded document.
    assert_eq!(
        corpus_status(&writer)?[0].sequence,
        1,
        "a coverage-breaking incremental leaves the cursor unchanged"
    );
    let leaked = client.execute_sql(
        "SELECT count(*)::text FROM jurisearch_server.documents WHERE document_id='cass:D2';",
    )?;
    assert_eq!(
        leaked.trim(),
        "0",
        "the rejected incremental's rows are rolled back (no partial apply)"
    );

    Ok(())
}

#[test]
fn an_idempotent_reapply_repairs_a_missing_readiness_stamp() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };

    // Producer: seed, build a baseline, then mutate + build an incremental.
    let (_proot, producer) = start("js-p3a-repair-producer.", &pg_config)?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-p3a-repair-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &StubSigner,
        &baseline_params(),
    )
    .expect("baseline build");
    mutate(
        &producer,
        "UPDATE documents SET valid_to='2024-12-31' WHERE document_id='cass:D1';",
        "documents",
        EventKind::Upsert,
        "cass:D1",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-p3a-repair-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("incremental build")
    .expect("incremental has changes");

    let (_croot, client) = start("js-p3a-repair-client.", &pg_config)?;
    provision_client(&client)?;
    let backend = ManagedPostgresBackend::new(
        &client,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let stamp_is_readable = |backend: &ManagedPostgresBackend| -> Result<bool, StorageError> {
        let mut read = backend.read_handle()?.client()?;
        Ok(
            jurisearch_storage::ingest_accounting::load_query_readiness_with_client(&mut read)
                .is_ok(),
        )
    };

    // BASELINE idempotent-repair: apply the baseline (cursor 1, stamped), then simulate a pre-P3A /
    // corrupted site by deleting the stamp — reads now fail closed.
    apply_baseline(
        &backend.writer_handle()?,
        base_art.path(),
        &AcceptAllVerifier,
    )
    .expect("baseline apply");
    client.execute_sql("DELETE FROM public.index_manifest WHERE key='query_readiness';")?;
    assert!(
        !stamp_is_readable(&backend)?,
        "with the stamp deleted, the read path fails closed"
    );
    // Re-applying the SAME baseline is an idempotent no-op that REPAIRS the stamp.
    let outcome = apply_baseline(
        &backend.writer_handle()?,
        base_art.path(),
        &AcceptAllVerifier,
    )
    .expect("idempotent baseline reapply");
    assert!(
        matches!(
            outcome,
            BaselineApplyOutcome::AlreadyApplied { sequence: 1, .. }
        ),
        "the reapply is a no-op: {outcome:?}"
    );
    assert!(
        stamp_is_readable(&backend)?,
        "the idempotent baseline reapply repaired the missing stamp"
    );

    // INCREMENTAL idempotent-repair: advance to sequence 2, delete the stamp, reapply the SAME
    // incremental → idempotent no-op that repairs it.
    apply_incremental(
        &backend.writer_handle()?,
        inc_art.path(),
        &AcceptAllVerifier,
    )
    .expect("incremental apply");
    assert_eq!(corpus_status(&backend.writer_handle()?)?[0].sequence, 2);
    client.execute_sql("DELETE FROM public.index_manifest WHERE key='query_readiness';")?;
    assert!(
        !stamp_is_readable(&backend)?,
        "with the stamp deleted again, the read path fails closed"
    );
    let outcome = apply_incremental(
        &backend.writer_handle()?,
        inc_art.path(),
        &AcceptAllVerifier,
    )
    .expect("idempotent incremental reapply");
    assert!(
        matches!(
            outcome,
            IncrementalApplyOutcome::AlreadyApplied { sequence: 2, .. }
        ),
        "the reapply is a no-op: {outcome:?}"
    );
    assert!(
        stamp_is_readable(&backend)?,
        "the idempotent incremental reapply repaired the missing stamp"
    );

    Ok(())
}

/// Extract the underlying PostgreSQL `DbError` from a [`jurisearch_syncd::SyncError`] (the activation
/// probe failure surfaces as a `Storage(PostgresClient(..))` or `Postgres(..)`), so a test can assert
/// the exact SQLSTATE rather than matching error text.
fn sync_error_db(error: &jurisearch_syncd::SyncError) -> Option<&postgres::error::DbError> {
    use jurisearch_syncd::SyncError;
    let pg = match error {
        SyncError::Postgres(pg) => pg,
        SyncError::Storage(StorageError::PostgresClient(pg)) => pg,
        _ => return None,
    };
    pg.as_db_error()
}
