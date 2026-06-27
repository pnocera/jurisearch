//! P10 atomicity/concurrency soak (design §12, INV-3): prove the apply/switch never stalls behind
//! contention and a concurrent reader only ever sees old-or-new committed state.
//!
//! Two deterministic assertions (not a literal 24h run — that is operated acceptance evidence):
//! 1. Under advisory-lock CONTENTION the apply fails CLEAN (`pg_try_advisory_xact_lock` returns
//!    immediately with a refusal) and the cursor is unchanged — never a stall.
//! 2. While a reader thread continuously queries the active generation through the stable views, an
//!    incremental AND a re-baseline are applied; every observed value is in the allowed set (old or new,
//!    never empty/partial), and the reader converges to the final value after the switch commits.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::event::EventKind;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental, build_rebaseline,
};
use jurisearch_storage::generations::APPLY_ADVISORY_LOCK_KEY;
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_syncd::{apply_baseline, apply_incremental, apply_rebaseline, corpus_status};

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

fn seed(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:SK1','cass','decision','cass:SK1','Cass','v0','corps','2024-01-01', \
           'sha256:sk1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:SK1#0','cass:SK1',0,'corps','ctx','sha256:c','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:SK1#0','fp','{}'::vector,'m',1024);",
        vector("0.01"),
    ))?;
    Ok(())
}

fn mutate(
    producer: &ManagedPostgres,
    sql: &str,
    table: &str,
    op: EventKind,
) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("soak", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope("core", table, op, scope_kind::DOCUMENT, "cass:SK1"),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

#[test]
fn apply_fails_clean_under_advisory_lock_contention() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let proot = tempfile::Builder::new()
        .prefix("js-skc-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-skc-base.")
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

    let croot = tempfile::Builder::new()
        .prefix("js-skc-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    apply_baseline(&client, base_art.path(), &AcceptAllVerifier).expect("apply baseline");

    mutate(
        &producer,
        "UPDATE documents SET title='v1' WHERE document_id='cass:SK1';",
        "documents",
        EventKind::Upsert,
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-skc-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("inc")
    .expect("changes");

    // Hold the apply advisory lock on a SEPARATE connection (a transaction that does not commit).
    let mut holder = postgres::Client::connect(&client.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut held = holder.transaction().map_err(StorageError::PostgresClient)?;
    held.execute(
        "SELECT pg_advisory_xact_lock($1);",
        &[&APPLY_ADVISORY_LOCK_KEY],
    )
    .map_err(StorageError::PostgresClient)?;

    // The apply must FAIL CLEAN (pg_try_advisory_xact_lock returns immediately), never stall.
    let result = apply_incremental(&client, inc_art.path(), &AcceptAllVerifier);
    assert!(
        result.is_err(),
        "apply under lock contention must fail clean, got {result:?}"
    );
    assert_eq!(
        corpus_status(&client)?[0].sequence,
        1,
        "a contended apply never advances the cursor"
    );

    // Release the lock → the apply now succeeds (the cursor is the deterministic retry authority).
    held.rollback().map_err(StorageError::PostgresClient)?;
    apply_incremental(&client, inc_art.path(), &AcceptAllVerifier)
        .expect("apply after lock released");
    assert_eq!(corpus_status(&client)?[0].sequence, 2);
    Ok(())
}

#[test]
fn readers_only_see_old_or_new_state_during_concurrent_applies() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let proot = tempfile::Builder::new()
        .prefix("js-skr-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-skr-base.")
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

    let croot = tempfile::Builder::new()
        .prefix("js-skr-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    apply_baseline(&client, base_art.path(), &AcceptAllVerifier).expect("apply baseline");

    // A reader thread continuously queries the active generation through the STABLE VIEW (the same
    // indirection the CLI reads). Every observed title must be in the allowed committed set.
    let conn = client.connection_string();
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || -> Result<usize, String> {
        let mut db = postgres::Client::connect(&conn, postgres::NoTls)
            .map_err(|error| format!("reader connect: {error}"))?;
        let allowed = ["v0", "v1", "v2"];
        let mut reads = 0usize;
        while !reader_stop.load(Ordering::Relaxed) {
            let row = db
                .query_one(
                    "SELECT title FROM jurisearch_server.documents WHERE document_id='cass:SK1';",
                    &[],
                )
                .map_err(|error| format!("reader query: {error}"))?;
            let title: String = row.get("title");
            if !allowed.contains(&title.as_str()) {
                return Err(format!("reader observed an out-of-band value `{title}`"));
            }
            reads += 1;
        }
        Ok(reads)
    });

    // Apply an incremental (v0 -> v1) and a re-baseline (v1 -> v2, new generation) while the reader runs.
    mutate(
        &producer,
        "UPDATE documents SET title='v1' WHERE document_id='cass:SK1';",
        "documents",
        EventKind::Upsert,
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-skr-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("inc")
    .expect("changes");
    apply_incremental(&client, inc_art.path(), &AcceptAllVerifier).expect("apply inc");

    mutate(
        &producer,
        "UPDATE documents SET title='v2' WHERE document_id='cass:SK1'; \
         UPDATE chunks SET embedding_fingerprint='fp2'; UPDATE chunk_embeddings SET embedding_fingerprint='fp2';",
        "chunks",
        EventKind::ReplaceSet,
    )?;
    let rb_art = tempfile::Builder::new()
        .prefix("js-skr-rb.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_rebaseline(
        &producer,
        "core",
        rb_art.path(),
        &StubSigner,
        &rebaseline_params(),
    )
    .expect("rebaseline");
    apply_rebaseline(&client, rb_art.path(), &AcceptAllVerifier).expect("apply rebaseline");

    stop.store(true, Ordering::Relaxed);
    let reads = reader
        .join()
        .expect("reader thread")
        .expect("no out-of-band read");
    assert!(
        reads > 0,
        "the reader observed at least one committed state"
    );

    // Final convergence after the switch commits.
    let final_title =
        client.execute_read_sql("SELECT title FROM documents WHERE document_id='cass:SK1';")?;
    assert_eq!(
        final_title.trim(),
        "v2",
        "the reader path converges to the re-baselined value"
    );
    let status = corpus_status(&client)?;
    assert_eq!(status[0].sequence, 3, "cursor advanced to the re-baseline");
    assert_eq!(status[0].active_generation, "core_g0002");
    Ok(())
}
