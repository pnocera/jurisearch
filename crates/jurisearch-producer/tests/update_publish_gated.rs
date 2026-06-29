//! INFRA-GATED acceptance gates (skip cleanly when `pgvector`/`pg_search` are not discoverable via
//! `JURISEARCH_PG_CONFIG`). These drive `producer_cycle("core")` through the producer's OWN external
//! `WriterHandle` (a `ConnectionConfig`-backed `DbClientSource`) — NEVER a `ManagedPostgres` — built
//! from a `ProducerConfig`. They prove, over the real external-PG seam:
//!
//! - an empty outbox window builds NO incremental but STILL refreshes the signed manifest (exit zero);
//! - one fixture delta publishes EXACTLY ONE new signed incremental;
//! - running the cycle again with no new change publishes nothing (running twice publishes once);
//! - the producer never falls back to `ManagedPostgres` for its DB-mutating work.
//!
//! The harness `ManagedPostgres` is used ONLY as the operator's external server (seeding + mutation);
//! the producer's writes go exclusively through `config.writer_handle()`.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use jurisearch_package::compat::Version;
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, EnrichmentMode, PublishFault, build_baseline, producer_cycle,
    producer_cycle_faulted, publish_package, published_manifest_path, staged_pending_dir,
};
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::update::cycle_config;
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};

const FINGERPRINT: &str = "bge-m3:1024:normalize:true";

fn vector() -> String {
    format!("[{}]", vec!["0.01"; 1024].join(","))
}

fn producer_config(port: u16, work: &Path) -> ProducerConfig {
    let secrets = work.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let seed = secrets.join("producer-signing.seed");
    std::fs::write(&seed, "03".repeat(32)).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o600)).unwrap();

    let toml = format!(
        r#"
[producer]
corpora_dir = "{root}/packages"
archives_dir = "{root}/archives"
state_dir = "{root}/state"
publisher = "jurisearch"
environment = "test"

[database]
host = "127.0.0.1"
port = {port}
name = "jurisearch"
admin_user = "postgres"
admin_database = "jurisearch"
writer_user = "postgres"
read_user = "postgres"
owner_role = "postgres"

[fetch]
base_url = "https://echanges.dila.gouv.fr/OPENDATA"
user_agent = "jurisearch-producer-test/0.1"
retain_deltas = "all"

[[fetch_group]]
name = "jurisprudence"
sources = ["cass"]
cadence = "daily"

[package]
corpus = "core"
signing_key_id = "producer-k1"
signing_key_seed_file = "{seed}"
uri_base = "media://"

[enrichment]
mode = "disabled"

[embedding]
provider = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"
model_name = "bge-m3"
request_model = "baai/bge-m3"
dimension = 1024
normalize = true
pooling = "cls"

[baseline_refresh]
mode = "auto-on-new-baseline"
"#,
        root = work.display(),
        port = port,
        seed = seed.display(),
    );
    let config = ProducerConfig::parse_str(&toml, Path::new("producer.toml")).unwrap();
    config.validate().unwrap();
    config
}

fn seed_baseline_rows(pg: &ManagedPostgres) -> Result<(), StorageError> {
    pg.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P1','cass','decision','cass:P1','Cass','Arret','corps','2024-01-01', \
           'sha256:p1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P1#0','cass:P1',0,'corps','ctx corps','sha256:c','c1','bge-m3:1024:normalize:true');",
    )?;
    pg.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:P1#0','{FINGERPRINT}','{}'::vector,'bge-m3',1024);",
        vector(),
    ))?;
    Ok(())
}

fn mutate(pg: &ManagedPostgres, sql: &str, scope_key: &str) -> Result<(), StorageError> {
    let mut client = pg.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("test-mutation", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope(
            "core",
            "documents",
            EventKind::Upsert,
            scope_kind::DOCUMENT,
            scope_key,
        ),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

fn baseline_params() -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert(
        "jurisearch-producer".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    BaselineParams {
        baseline_id: "core-2026-06-29-g0001".to_owned(),
        builder_run_id: "baseline".to_owned(),
        created_at: "2026-06-29T00:00:00Z".to_owned(),
        embedding_fingerprint: FINGERPRINT.to_owned(),
        embedding_model: "bge-m3".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn manifest_head(root: &Path) -> u64 {
    let bytes = std::fs::read(published_manifest_path(root, "core")).expect("manifest bytes");
    let signed: Signed<RemoteManifest> = serde_json::from_slice(&bytes).expect("manifest json");
    signed.payload.head_sequence.get()
}

#[test]
fn producer_cycle_over_external_writer_handle_is_exactly_once_and_refreshes_manifest()
-> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        eprintln!("SKIP: no pgvector/pg_search assets via JURISEARCH_PG_CONFIG");
        return Ok(());
    };
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let pg = ManagedPostgres::start_durable(pg_config, proot.path())?;
    pg.run_migrations()?;
    seed_baseline_rows(&pg)?;

    let work = tempfile::tempdir().map_err(StorageError::Io)?;
    let config = producer_config(pg.port, work.path());
    // The producer's DB-mutating identity is an EXTERNAL ConnectionConfig-backed WriterHandle.
    let db = config.writer_handle().expect("writer handle");
    let signer = config.signer().expect("signer");
    let served_root = config.producer.corpora_dir.clone();
    std::fs::create_dir_all(&served_root).unwrap();

    // The baseline is one-time operator setup (built on the server itself); the producer's incremental
    // CYCLE below runs through the external WriterHandle.
    let base_art = tempfile::tempdir().map_err(StorageError::Io)?;
    let base = build_baseline(&pg, "core", base_art.path(), &signer, &baseline_params())
        .expect("baseline builds");
    publish_package(&served_root, "core", &base.package_id, base_art.path()).expect("publish base");

    // --- Gate: empty outbox window → NO incremental, but the signed manifest STILL refreshes. ---
    let empty = producer_cycle(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-empty", EnrichmentMode::Disabled, &signer),
    )
    .expect("empty cycle succeeds");
    assert!(
        empty.built_incremental.is_none(),
        "empty window builds nothing"
    );
    assert!(
        published_manifest_path(&served_root, "core").exists(),
        "an empty run still refreshes the signed manifest"
    );
    assert_eq!(manifest_head(&served_root), 1, "head is the baseline only");
    // An empty cycle still reports the current published head (the baseline), window-high unchanged.
    assert_eq!(
        empty.head_sequence,
        Some(1),
        "head sequence is the baseline"
    );

    // --- Gate: one fixture delta publishes EXACTLY ONE new signed incremental. ---
    mutate(
        &pg,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:P1';",
        "cass:P1",
    )?;
    let delta = producer_cycle(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-delta", EnrichmentMode::Disabled, &signer),
    )
    .expect("delta cycle succeeds");
    assert!(
        delta.built_incremental.is_some(),
        "the delta builds one incremental"
    );
    assert_eq!(
        manifest_head(&served_root),
        2,
        "head advanced by exactly one"
    );
    // WARN fix: the cycle reports the real package coordinates (sequence + frozen change_seq window-high)
    // that `run_update` copies verbatim into its PackageHighWaterMark checkpoint.
    assert_eq!(delta.head_sequence, Some(2), "published head sequence");
    assert!(
        delta.included_change_seq_high.unwrap_or(0) > 0,
        "the incremental froze a real change_seq window-high: {:?}",
        delta.included_change_seq_high
    );

    // --- Gate: running again with no new change publishes nothing (twice → publishes once). ---
    let again = producer_cycle(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-again", EnrichmentMode::Disabled, &signer),
    )
    .expect("idempotent cycle succeeds");
    assert!(
        again.built_incremental.is_none(),
        "no new change → no new incremental"
    );
    assert_eq!(
        manifest_head(&served_root),
        2,
        "head unchanged on the no-op re-run"
    );
    Ok(())
}

/// BLOCKER gate: a publish failure AFTER the catalog row is inserted must NOT strand the producer on an
/// unpublished package. The next `producer_cycle` RESUMES the SAME `package_id` from its staged artifact,
/// the manifest advances only after that artifact exists, and the high-water mark advances exactly once.
#[test]
fn a_pre_publish_failure_is_resumed_to_the_same_package_exactly_once() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        eprintln!("SKIP: no pgvector/pg_search assets via JURISEARCH_PG_CONFIG");
        return Ok(());
    };
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let pg = ManagedPostgres::start_durable(pg_config, proot.path())?;
    pg.run_migrations()?;
    seed_baseline_rows(&pg)?;

    let work = tempfile::tempdir().map_err(StorageError::Io)?;
    let config = producer_config(pg.port, work.path());
    let db = config.writer_handle().expect("writer handle");
    let signer = config.signer().expect("signer");
    let served_root = config.producer.corpora_dir.clone();
    std::fs::create_dir_all(&served_root).unwrap();

    // Operator one-time baseline, then an empty cycle to publish the initial manifest (head = 1).
    let base_art = tempfile::tempdir().map_err(StorageError::Io)?;
    let base = build_baseline(&pg, "core", base_art.path(), &signer, &baseline_params())
        .expect("baseline builds");
    publish_package(&served_root, "core", &base.package_id, base_art.path()).expect("publish base");
    producer_cycle(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-init", EnrichmentMode::Disabled, &signer),
    )
    .expect("init cycle");
    assert_eq!(manifest_head(&served_root), 1);

    // One delta, then a cycle that crashes AFTER the catalog row + staged artifact, BEFORE publish.
    mutate(
        &pg,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:P1';",
        "cass:P1",
    )?;
    let faulted = producer_cycle_faulted(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-fault", EnrichmentMode::Disabled, &signer),
        PublishFault::AfterStageBeforePublish,
    );
    assert!(
        faulted.is_err(),
        "the injected pre-publish fault propagates"
    );

    // The catalog row exists as `built`, the artifact is NOT published, but it IS staged for resume, and
    // the manifest has NOT advanced past the baseline.
    let status =
        pg.execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-2';")?;
    assert_eq!(
        status.trim(),
        "built",
        "the catalog row is cataloged but unpublished"
    );
    let published_dir = served_root.join("core").join("packages").join("core-1-2");
    assert!(
        !published_dir.exists(),
        "no served artifact after the failed publish"
    );
    assert!(
        staged_pending_dir(&served_root, "core")
            .join("manifest.json")
            .exists(),
        "the built artifact is staged for resume"
    );
    assert_eq!(
        manifest_head(&served_root),
        1,
        "manifest did not advance over a missing artifact"
    );

    // Re-run: the cycle RESUMES the SAME package (no new build), publishes it, advances the manifest once.
    let resumed = producer_cycle(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-resume", EnrichmentMode::Disabled, &signer),
    )
    .expect("resume cycle succeeds");
    assert_eq!(
        resumed.built_incremental.as_deref(),
        Some("core-1-2"),
        "the SAME package id is published, not a new one"
    );
    assert!(
        published_dir.exists(),
        "the artifact now exists at the served root"
    );
    assert_eq!(
        manifest_head(&served_root),
        2,
        "manifest advanced to the resumed package"
    );
    assert_eq!(resumed.head_sequence, Some(2));
    let status =
        pg.execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-2';")?;
    assert_eq!(
        status.trim(),
        "published",
        "the resumed row is marked published"
    );

    // Exactly once: a no-op re-run with no new change neither rebuilds nor re-advances the head.
    let again = producer_cycle(
        &db,
        "core",
        &served_root,
        &signer,
        &cycle_config(&config, "run-after", EnrichmentMode::Disabled, &signer),
    )
    .expect("no-op cycle succeeds");
    assert!(
        again.built_incremental.is_none(),
        "nothing new to build or resume"
    );
    assert_eq!(
        manifest_head(&served_root),
        2,
        "head advanced exactly once overall"
    );
    let count =
        pg.execute_sql("SELECT count(*) FROM package_catalog WHERE package_kind='incremental';")?;
    assert_eq!(
        count.trim(),
        "1",
        "exactly one incremental was ever cataloged"
    );
    Ok(())
}
