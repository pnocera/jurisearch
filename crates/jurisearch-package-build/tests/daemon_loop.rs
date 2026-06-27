//! work/09 P5 acceptance: the syncd DAEMON (not the one-shot `update`) converges a corpus offline→head
//! by COMPOSING the planner/apply substrate — poll→plan→verify→apply over a filesystem-published
//! `DirectoryCatchupSource`, classified + scheduled by the daemon loop. A test [`Clock`] drives the loop
//! with no real sleeps and stops it after one converged cycle. Also: a manifest with a bad signature is
//! REJECTED with the cursor untouched (the daemon keeps running), and the daemon-lifetime single-writer
//! advisory lease is exclusive (a second acquirer is blocked).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use jurisearch_package::artifact;
use jurisearch_package::compat::Version;
use jurisearch_package::corpus::Corpus;
use jurisearch_package::crypto::{Ed25519Signer, KeyEpoch, KeyId, Signature};
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::remote::{
    BaselineRef, CatchupPolicy, EntitlementListing, EntitlementTier, RemoteManifest,
    RemotePackageEntry, SigningInfo,
};
use jurisearch_package::sequence::PackageSequence;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental,
};
use jurisearch_storage::generations::{release_daemon_lock, try_acquire_daemon_lock};
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_storage::trust::{PACKAGE_PURPOSE, install_trust_anchor};
use jurisearch_syncd::{
    CatchupSource, Clock, DaemonConfig, DirectoryCatchupSource, ManifestSource, ShutdownToken,
    SyncError, corpus_status, read_client_cursor, run_daemon,
};

fn signer(seed: u8, key: &str) -> Ed25519Signer {
    Ed25519Signer::from_seed(&[seed; 32], KeyId(key.to_owned()), KeyEpoch(1))
}

fn placeholder_sig() -> Signature {
    Signature {
        algorithm: "ed25519".to_owned(),
        key_id: KeyId("k".to_owned()),
        key_epoch: KeyEpoch(0),
        signature_hex: String::new(),
    }
}

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
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:CU1','cass','decision','cass:CU1','Cass','Arret','corps','2024-01-01', \
           'sha256:cu1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:CU1#0','cass:CU1',0,'corps','ctx corps','sha256:c','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:CU1#0','fp','{}'::vector,'m',1024);",
        vector("0.01"),
    ))?;
    Ok(())
}

fn mutate(producer: &ManagedPostgres, sql: &str, scope_key: &str) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("mutation-run", 24);
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

fn embedded(dir: &Path) -> EmbeddedManifest {
    let bytes = std::fs::read(artifact::manifest_path(dir)).expect("manifest bytes");
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes).expect("manifest json");
    signed.payload
}

/// Recursively copy an artifact directory into the published tree.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir dst");
    for entry in std::fs::read_dir(src).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let target = dst.join(entry.file_name());
        if entry.file_type().expect("file_type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

fn package_entry(dir: &Path, uri: String) -> RemotePackageEntry {
    let m = embedded(dir);
    RemotePackageEntry {
        package_id: m.identity.package_id.clone(),
        from_sequence: m.identity.from_sequence,
        to_sequence: m.identity.to_sequence,
        artifact_uri: uri,
        compressed_size_bytes: 10,
        uncompressed_size_bytes: 100,
        estimated_apply_seconds: 1,
        row_counts: BTreeMap::new(),
        requires_baseline: false,
        minimum_client_version: m.compatibility.minimum_client_version,
        schema_version: m.compatibility.schema_version,
        embedding_fingerprint: m.compatibility.embedding_fingerprint.clone(),
        builder_versions: m.compatibility.builder_versions.clone(),
        sha256: m.integrity.artifact_sha256.clone(),
        signature: placeholder_sig(),
    }
}

fn baseline_ref(dir: &Path, uri: String) -> BaselineRef {
    let m = embedded(dir);
    BaselineRef {
        baseline_id: m.identity.baseline_id.clone(),
        generation: m.identity.generation.clone(),
        package_kind: m.identity.package_kind,
        sequence: m.identity.to_sequence,
        schema_version: m.compatibility.schema_version,
        minimum_client_version: m.compatibility.minimum_client_version,
        artifact_uri: uri,
        compressed_size_bytes: 1000,
        uncompressed_size_bytes: 10_000,
        estimated_load_seconds: 600,
        sha256: m.integrity.artifact_sha256.clone(),
        signature: placeholder_sig(),
    }
}

/// Publish a baseline + incrementals into the `DirectoryCatchupSource` layout under `<root>/core/`:
/// copy each artifact dir to `core/{baselines,packages}/<id>`, build the remote manifest with matching
/// `media://core/...` URIs, sign it with `manifest_signer`, and write `<root>/core/manifest.json`.
fn publish(
    root: &Path,
    baseline_dir: &Path,
    incrementals: &[&Path],
    manifest_signer: &Ed25519Signer,
    package_signer: &Ed25519Signer,
) {
    let corpus_root = root.join("core");
    let base_id = embedded(baseline_dir).identity.baseline_id;
    copy_tree(baseline_dir, &corpus_root.join("baselines").join(&base_id));
    let baseline = baseline_ref(baseline_dir, format!("media://core/baselines/{base_id}"));

    let mut packages = Vec::new();
    for inc in incrementals {
        let pkg_id = embedded(inc).identity.package_id;
        copy_tree(inc, &corpus_root.join("packages").join(&pkg_id));
        packages.push(package_entry(
            inc,
            format!("media://core/packages/{pkg_id}"),
        ));
    }
    let head = packages
        .iter()
        .map(|p| p.to_sequence.get())
        .max()
        .unwrap_or(1);

    let manifest = RemoteManifest {
        manifest_version: 1,
        generated_at: "2026-06-27T02:00:00Z".to_owned(),
        publisher: "jurisearch".to_owned(),
        corpus: Corpus::new("core").unwrap(),
        environment: "test".to_owned(),
        head_sequence: PackageSequence::new(head),
        min_available_sequence: PackageSequence::new(1),
        active_baseline: baseline,
        packages,
        catchup_ranges: vec![],
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 100,
            max_cumulative_diff_to_baseline_permille: 330,
            max_cumulative_uncompressed_to_baseline_permille: 500,
            max_apply_seconds_budget: 2700,
        },
        entitlement: EntitlementListing {
            corpus: Corpus::new("core").unwrap(),
            tier: EntitlementTier::Open,
            license_epoch: 0,
            audience: None,
        },
        signing: SigningInfo {
            key_id: package_signer.key_id().clone(),
            algorithm: "ed25519".to_owned(),
        },
    };
    let signed = Signed::seal(manifest, manifest_signer).expect("sign remote manifest");
    std::fs::create_dir_all(&corpus_root).expect("mkdir corpus root");
    std::fs::write(
        corpus_root.join("manifest.json"),
        serde_json::to_vec(&signed).expect("manifest bytes"),
    )
    .expect("write manifest");
}

/// A test clock that NEVER sleeps: it records each requested wait and then requests shutdown, so
/// `run_daemon` returns after exactly ONE post-cycle sleep (i.e. one converged/handled cycle).
struct StopAfterOneCycle {
    sleeps: Mutex<Vec<Duration>>,
    shutdown: Arc<ShutdownToken>,
}

impl Clock for StopAfterOneCycle {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wait_or_shutdown(&self, duration: Duration, shutdown: &ShutdownToken) -> bool {
        self.sleeps.lock().unwrap().push(duration);
        self.shutdown.request();
        shutdown.is_requested()
    }
}

fn build_producer_with_two_incrementals(
    pg_config: &PgConfig,
    signer: &Ed25519Signer,
) -> Result<
    (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
    ),
    StorageError,
> {
    let proot = tempfile::Builder::new()
        .prefix("js-dl-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    let base = tempfile::Builder::new()
        .prefix("js-dl-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(&producer, "core", base.path(), signer, &baseline_params()).expect("baseline");

    mutate(
        &producer,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:CU1';",
        "cass:CU1",
    )?;
    let inc1 = tempfile::Builder::new()
        .prefix("js-dl-i1.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc1.path(),
        signer,
        &incremental_params("r1"),
    )
    .expect("inc1")
    .expect("inc1 changes");

    mutate(
        &producer,
        "UPDATE documents SET title='rev2' WHERE document_id='cass:CU1';",
        "cass:CU1",
    )?;
    let inc2 = tempfile::Builder::new()
        .prefix("js-dl-i2.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc2.path(),
        signer,
        &incremental_params("r2"),
    )
    .expect("inc2")
    .expect("inc2 changes");

    // Keep `proot` alive (the producer PG dir) by returning it.
    Ok((proot, base, inc1, inc2))
}

#[test]
fn the_daemon_converges_a_fresh_client_offline_to_head_in_one_cycle() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer(5, "producer-k");
    let (_proot, base, inc1, inc2) = build_producer_with_two_incrementals(&pg_config, &sgnr)?;

    // Publish baseline + 2 incrementals into the DirectoryCatchupSource layout.
    let published = tempfile::Builder::new()
        .prefix("js-dl-pub.")
        .tempdir()
        .map_err(StorageError::Io)?;
    publish(
        published.path(),
        base.path(),
        &[inc1.path(), inc2.path()],
        &sgnr,
        &sgnr,
    );

    // A FRESH client (no cursor) trusting the producer key.
    let croot = tempfile::Builder::new()
        .prefix("js-dl-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &sgnr.trust_anchor(), PACKAGE_PURPOSE)?;
    }

    // Run the daemon: one cycle bursts baseline → incrementals → up-to-date, then the stop-clock halts it.
    let source = DirectoryCatchupSource::new(published.path(), "media://");
    let shutdown = Arc::new(ShutdownToken::new());
    let clock = StopAfterOneCycle {
        sleeps: Mutex::new(Vec::new()),
        shutdown: Arc::clone(&shutdown),
    };
    let config = DaemonConfig {
        corpora: vec!["core".to_owned()],
        poll_interval: Duration::from_secs(30),
        ..DaemonConfig::default()
    };
    let mut lock_alive = || true;
    run_daemon(
        &client,
        &source,
        &clock,
        &shutdown,
        &mut lock_alive,
        &config,
    )
    .expect("daemon runs to graceful shutdown");

    // Converged to the producer head (sequence 3), in a single daemon cycle.
    assert_eq!(
        corpus_status(&client)?[0].sequence,
        3,
        "daemon caught up to head"
    );
    let cursor = read_client_cursor(&client, "core")
        .expect("cursor")
        .expect("installed");
    assert_eq!(cursor.sequence, 3);
    // Exactly one post-cycle sleep was requested (the normal interval — a clean cycle, no backoff).
    let sleeps = clock.sleeps.lock().unwrap();
    assert_eq!(sleeps.len(), 1);
    assert_eq!(sleeps[0], Duration::from_secs(30));
    Ok(())
}

/// A source that requests shutdown the FIRST time the manifest is fetched — simulating SIGTERM arriving
/// while the daemon's first (in-flight) apply runs. The daemon must finish that apply but NOT start the
/// next burst's apply.
struct ShutdownOnFirstManifest {
    inner: DirectoryCatchupSource,
    shutdown: Arc<ShutdownToken>,
    manifests_fetched: Mutex<usize>,
}

impl ManifestSource for ShutdownOnFirstManifest {
    fn fetch_manifest(
        &self,
        corpus: &str,
    ) -> Result<jurisearch_package::signed::Signed<RemoteManifest>, SyncError> {
        {
            let mut fetched = self.manifests_fetched.lock().unwrap();
            *fetched += 1;
            if *fetched == 1 {
                self.shutdown.request();
            }
        }
        self.inner.fetch_manifest(corpus)
    }
}

impl CatchupSource for ShutdownOnFirstManifest {
    fn fetch_baseline(&self, baseline: &BaselineRef) -> Result<std::path::PathBuf, SyncError> {
        self.inner.fetch_baseline(baseline)
    }
    fn fetch_package(&self, entry: &RemotePackageEntry) -> Result<std::path::PathBuf, SyncError> {
        self.inner.fetch_package(entry)
    }
}

/// A clock that must NEVER be asked to wait — the daemon must exit via the mid-burst shutdown check,
/// before reaching any post-cycle sleep.
struct NeverSleepClock;
impl Clock for NeverSleepClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wait_or_shutdown(&self, _duration: Duration, _shutdown: &ShutdownToken) -> bool {
        panic!("the daemon reached a post-cycle sleep instead of stopping mid-burst on shutdown");
    }
}

#[test]
fn shutdown_during_a_progress_burst_does_not_start_the_next_apply() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer(5, "producer-k");
    let (_proot, base, inc1, inc2) = build_producer_with_two_incrementals(&pg_config, &sgnr)?;
    let published = tempfile::Builder::new()
        .prefix("js-dl-sd.")
        .tempdir()
        .map_err(StorageError::Io)?;
    publish(
        published.path(),
        base.path(),
        &[inc1.path(), inc2.path()],
        &sgnr,
        &sgnr,
    );

    let croot = tempfile::Builder::new()
        .prefix("js-dl-sdc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &sgnr.trust_anchor(), PACKAGE_PURPOSE)?;
    }

    let shutdown = Arc::new(ShutdownToken::new());
    let source = ShutdownOnFirstManifest {
        inner: DirectoryCatchupSource::new(published.path(), "media://"),
        shutdown: Arc::clone(&shutdown),
        manifests_fetched: Mutex::new(0),
    };
    let config = DaemonConfig {
        corpora: vec!["core".to_owned()],
        max_burst: 16,
        ..DaemonConfig::default()
    };
    let mut lock_alive = || true;
    run_daemon(
        &client,
        &source,
        &NeverSleepClock,
        &shutdown,
        &mut lock_alive,
        &config,
    )
    .expect("graceful shutdown mid-burst");

    // Burst 0 applied the baseline (seq 1) with shutdown ALREADY requested; burst 1 (which would have
    // applied the incrementals to seq 3) was NOT started — the in-flight apply finished, then the daemon
    // stopped before beginning the next one.
    assert_eq!(
        corpus_status(&client)?[0].sequence,
        1,
        "only the in-flight baseline applied; the next burst's apply was not started"
    );
    assert_eq!(
        *source.manifests_fetched.lock().unwrap(),
        1,
        "the second burst's planning (manifest fetch) never ran after shutdown"
    );
    Ok(())
}

#[test]
fn a_bad_signature_manifest_is_rejected_with_the_cursor_untouched() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let producer_key = signer(5, "producer-k");
    let attacker_key = signer(9, "attacker-k");
    let (_proot, base, inc1, inc2) =
        build_producer_with_two_incrementals(&pg_config, &producer_key)?;

    // Publish a manifest signed by the ATTACKER key (the client does not trust it).
    let published = tempfile::Builder::new()
        .prefix("js-dl-bad.")
        .tempdir()
        .map_err(StorageError::Io)?;
    publish(
        published.path(),
        base.path(),
        &[inc1.path(), inc2.path()],
        &attacker_key,
        &producer_key,
    );

    let croot = tempfile::Builder::new()
        .prefix("js-dl-bc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        // Trust ONLY the producer key — the attacker-signed manifest must be refused.
        install_trust_anchor(&mut db, &producer_key.trust_anchor(), PACKAGE_PURPOSE)?;
    }

    let source = DirectoryCatchupSource::new(published.path(), "media://");
    let shutdown = Arc::new(ShutdownToken::new());
    let clock = StopAfterOneCycle {
        sleeps: Mutex::new(Vec::new()),
        shutdown: Arc::clone(&shutdown),
    };
    let config = DaemonConfig {
        corpora: vec!["core".to_owned()],
        ..DaemonConfig::default()
    };
    let mut lock_alive = || true;
    // The daemon classifies the bad signature as Rejected, logs it, leaves the cursor unchanged, and
    // KEEPS RUNNING (no crash) — the stop-clock then halts it gracefully.
    run_daemon(
        &client,
        &source,
        &clock,
        &shutdown,
        &mut lock_alive,
        &config,
    )
    .expect("a reject must NOT crash the daemon");

    // Nothing was applied: the client is still uninstalled (cursor untouched).
    assert!(
        corpus_status(&client)?.is_empty(),
        "a rejected manifest must not advance the cursor"
    );
    Ok(())
}

#[test]
fn the_daemon_single_writer_lease_is_exclusive() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let croot = tempfile::Builder::new()
        .prefix("js-dl-lock.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;

    // Daemon A acquires the lease on its session connection.
    let mut a = client.client()?;
    assert!(
        try_acquire_daemon_lock(&mut a)?,
        "first daemon acquires the lease"
    );
    // Daemon B (a separate connection) is BLOCKED — only one writer per database.
    let mut b = client.client()?;
    assert!(
        !try_acquire_daemon_lock(&mut b)?,
        "a second daemon cannot acquire the held single-writer lease"
    );
    // Once A releases (or its connection closes), B can take over.
    release_daemon_lock(&mut a)?;
    assert!(
        try_acquire_daemon_lock(&mut b)?,
        "after release the lease is available again"
    );
    Ok(())
}

/// A source that counts manifest fetches (no side effects) — to prove a burst's planning never ran.
struct CountingManifestSource {
    inner: DirectoryCatchupSource,
    manifests_fetched: Mutex<usize>,
}

impl ManifestSource for CountingManifestSource {
    fn fetch_manifest(
        &self,
        corpus: &str,
    ) -> Result<jurisearch_package::signed::Signed<RemoteManifest>, SyncError> {
        *self.manifests_fetched.lock().unwrap() += 1;
        self.inner.fetch_manifest(corpus)
    }
}

impl CatchupSource for CountingManifestSource {
    fn fetch_baseline(&self, baseline: &BaselineRef) -> Result<std::path::PathBuf, SyncError> {
        self.inner.fetch_baseline(baseline)
    }
    fn fetch_package(&self, entry: &RemotePackageEntry) -> Result<std::path::PathBuf, SyncError> {
        self.inner.fetch_package(entry)
    }
}

#[test]
fn lease_loss_during_a_progress_burst_is_fatal_and_stops_before_the_next_apply()
-> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer(5, "producer-k");
    let (_proot, base, inc1, inc2) = build_producer_with_two_incrementals(&pg_config, &sgnr)?;
    let published = tempfile::Builder::new()
        .prefix("js-dl-ll.")
        .tempdir()
        .map_err(StorageError::Io)?;
    publish(
        published.path(),
        base.path(),
        &[inc1.path(), inc2.path()],
        &sgnr,
        &sgnr,
    );

    let croot = tempfile::Builder::new()
        .prefix("js-dl-llc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &sgnr.trust_anchor(), PACKAGE_PURPOSE)?;
    }

    let source = CountingManifestSource {
        inner: DirectoryCatchupSource::new(published.path(), "media://"),
        manifests_fetched: Mutex::new(0),
    };
    let shutdown = Arc::new(ShutdownToken::new()); // never requested — this is the LEASE path
    // The lease is held for the FIRST guard check (burst 0), then LOST before the second (burst 1).
    let mut lease_checks = 0;
    let mut lock_alive = || {
        lease_checks += 1;
        lease_checks == 1
    };
    let config = DaemonConfig {
        corpora: vec!["core".to_owned()],
        max_burst: 16,
        ..DaemonConfig::default()
    };
    let result = run_daemon(
        &client,
        &source,
        &NeverSleepClock,
        &shutdown,
        &mut lock_alive,
        &config,
    );

    // A lost single-writer lease is FATAL — run_daemon exits with an error.
    assert!(result.is_err(), "a lost single-writer lease must be fatal");
    // Burst 0 applied the baseline (seq 1); burst 1 (the incrementals) never started — the lease was
    // checked again BEFORE it, found gone, and the daemon stopped.
    assert_eq!(
        corpus_status(&client)?[0].sequence,
        1,
        "only the in-flight baseline applied before the lease was found lost"
    );
    assert_eq!(
        *source.manifests_fetched.lock().unwrap(),
        1,
        "the second burst's manifest fetch never ran after the lease was lost"
    );
    Ok(())
}
