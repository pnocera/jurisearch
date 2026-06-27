//! P7 acceptance (milestone): a client offline for N packages polls a SIGNED remote manifest, the
//! planner routes it to the in-order incremental chain, and `run_catchup` applies exactly the missing
//! packages in order and converges to the producer head (digest match). A client past the retention
//! window is instead routed to a fresh baseline. The signed-manifest verification + the existing
//! P4/P5/P6 apply gates remain the authoritative trust boundary.

use std::collections::BTreeMap;
use std::path::PathBuf;

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

/// P7 defers per-entry remote signatures (codex): the whole-manifest `Signed<RemoteManifest>` signature
/// is the trust anchor, so the per-entry/ref `signature` fields are unused placeholders here.
fn placeholder_sig() -> Signature {
    Signature {
        algorithm: "ed25519".to_owned(),
        key_id: KeyId("k".to_owned()),
        key_epoch: KeyEpoch(0),
        signature_hex: String::new(),
    }
}
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental,
};
use jurisearch_storage::outbox::{
    DigestSource, OutboxContext, OutboxEvent, corpus_table_digests, emit_change, scope_kind,
};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_storage::trust::{PACKAGE_PURPOSE, install_trust_anchor};
use jurisearch_syncd::{
    CatchupPlan, CatchupReport, CatchupSource, apply_baseline, corpus_status,
    load_package_verifier, plan_catchup, read_client_cursor, run_catchup,
};

fn signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[5u8; 32], KeyId("producer-k".to_owned()), KeyEpoch(1))
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

/// Read a built artifact's embedded manifest (the producer just signed it).
fn embedded(dir: &std::path::Path) -> EmbeddedManifest {
    let bytes = std::fs::read(artifact::manifest_path(dir)).expect("manifest bytes");
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes).expect("manifest json");
    signed.payload
}

/// A local-directory catch-up source: maps `package_id` / `baseline_id` to the on-disk artifact dir.
struct LocalSource {
    by_package: BTreeMap<String, PathBuf>,
    by_baseline: BTreeMap<String, PathBuf>,
}

impl CatchupSource for LocalSource {
    fn fetch_baseline(
        &self,
        baseline: &BaselineRef,
    ) -> Result<PathBuf, jurisearch_syncd::SyncError> {
        Ok(self
            .by_baseline
            .get(&baseline.baseline_id)
            .cloned()
            .expect("baseline dir"))
    }

    fn fetch_package(
        &self,
        entry: &RemotePackageEntry,
    ) -> Result<PathBuf, jurisearch_syncd::SyncError> {
        Ok(self
            .by_package
            .get(&entry.package_id)
            .cloned()
            .expect("package dir"))
    }
}

fn package_entry(dir: &std::path::Path) -> RemotePackageEntry {
    let m = embedded(dir);
    RemotePackageEntry {
        package_id: m.identity.package_id.clone(),
        from_sequence: m.identity.from_sequence,
        to_sequence: m.identity.to_sequence,
        artifact_uri: format!("local://{}", m.identity.package_id),
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

fn baseline_ref(dir: &std::path::Path) -> BaselineRef {
    let m = embedded(dir);
    BaselineRef {
        baseline_id: m.identity.baseline_id.clone(),
        generation: m.identity.generation.clone(),
        package_kind: m.identity.package_kind,
        sequence: m.identity.to_sequence,
        schema_version: m.compatibility.schema_version,
        minimum_client_version: m.compatibility.minimum_client_version,
        artifact_uri: format!("media://{}", m.identity.baseline_id),
        compressed_size_bytes: 1000,
        uncompressed_size_bytes: 10_000,
        estimated_load_seconds: 600,
        sha256: m.integrity.artifact_sha256.clone(),
        signature: placeholder_sig(),
    }
}

fn build_manifest(
    baseline_dir: &std::path::Path,
    package_dirs: &[&std::path::Path],
    min_available: u64,
    signer: &Ed25519Signer,
) -> Signed<RemoteManifest> {
    let packages: Vec<_> = package_dirs.iter().map(|d| package_entry(d)).collect();
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
        min_available_sequence: PackageSequence::new(min_available),
        active_baseline: baseline_ref(baseline_dir),
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
            key_id: signer.key_id().clone(),
            algorithm: "ed25519".to_owned(),
        },
    };
    Signed::seal(manifest, signer).expect("sign remote manifest")
}

#[test]
fn an_offline_client_catches_up_the_incremental_chain_in_order() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();

    // Producer: baseline + two incrementals.
    let proot = tempfile::Builder::new()
        .prefix("js-cu-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    let base_art = tempfile::Builder::new()
        .prefix("js-cu-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");

    mutate(
        &producer,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:CU1';",
        "cass:CU1",
    )?;
    let inc1 = tempfile::Builder::new()
        .prefix("js-cu-i1.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc1.path(),
        &sgnr,
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
        .prefix("js-cu-i2.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc2.path(),
        &sgnr,
        &incremental_params("r2"),
    )
    .expect("inc2")
    .expect("inc2 changes");

    // Client: install trust anchor, apply the baseline (now at seq 1), but NOT the incrementals.
    let croot = tempfile::Builder::new()
        .prefix("js-cu-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &sgnr.trust_anchor(), PACKAGE_PURPOSE)?;
    }
    let verifier = load_package_verifier(&client).expect("verifier");
    apply_baseline(&client, base_art.path(), &verifier).expect("apply baseline");
    assert_eq!(corpus_status(&client)?[0].sequence, 1);

    // Build + verify the signed remote manifest, then PLAN from the client cursor.
    let signed_manifest = build_manifest(base_art.path(), &[inc1.path(), inc2.path()], 1, &sgnr);
    signed_manifest
        .verify(&verifier)
        .expect("remote manifest signature verifies");
    let cursor = read_client_cursor(&client, "core")
        .expect("cursor read")
        .expect("installed");
    let plan = plan_catchup(&signed_manifest.payload, Some(&cursor));
    match &plan {
        CatchupPlan::Incremental(chain) => {
            assert_eq!(chain.len(), 2, "plan the two missing packages")
        }
        other => panic!("expected Incremental, got {other:?}"),
    }

    let source = LocalSource {
        by_package: BTreeMap::from([
            (
                embedded(inc1.path()).identity.package_id,
                inc1.path().to_owned(),
            ),
            (
                embedded(inc2.path()).identity.package_id,
                inc2.path().to_owned(),
            ),
        ]),
        by_baseline: BTreeMap::new(),
    };
    let report = run_catchup(&client, &source, &verifier, plan).expect("catch-up runs");
    assert_eq!(report, CatchupReport::IncrementalApplied { applied: 2 });

    // Converged to the producer head.
    assert_eq!(corpus_status(&client)?[0].sequence, 3);
    let producer_digests = corpus_table_digests(&producer, "core", DigestSource::ProducerPublic)?;
    let client_digests = corpus_table_digests(
        &client,
        "core",
        DigestSource::Generation {
            schema: "jurisearch_server_core_g0001",
        },
    )?;
    assert_eq!(
        producer_digests, client_digests,
        "client converged to the producer head"
    );

    // Re-planning now reports up to date.
    let cursor2 = read_client_cursor(&client, "core")
        .expect("cursor read")
        .expect("cursor");
    assert_eq!(
        plan_catchup(&signed_manifest.payload, Some(&cursor2)),
        CatchupPlan::UpToDate
    );
    Ok(())
}

#[test]
fn a_client_past_retention_is_routed_to_a_fresh_baseline() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::Builder::new()
        .prefix("js-cu2-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-cu2-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");

    // A FRESH client (no cursor) plans to the active baseline, and run_catchup applies it.
    let croot = tempfile::Builder::new()
        .prefix("js-cu2-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &sgnr.trust_anchor(), PACKAGE_PURPOSE)?;
    }
    let verifier = load_package_verifier(&client).expect("verifier");

    // Manifest with min_available 5 (the fresh/behind client is below the retained window).
    let signed_manifest = build_manifest(base_art.path(), &[], 5, &sgnr);
    signed_manifest
        .verify(&verifier)
        .expect("manifest verifies");
    let plan = plan_catchup(&signed_manifest.payload, None);
    assert!(
        matches!(plan, CatchupPlan::FreshBaseline(_)),
        "fresh client → baseline"
    );

    let source = LocalSource {
        by_package: BTreeMap::new(),
        by_baseline: BTreeMap::from([(
            embedded(base_art.path()).identity.baseline_id,
            base_art.path().to_owned(),
        )]),
    };
    let report = run_catchup(&client, &source, &verifier, plan).expect("baseline catch-up runs");
    assert!(matches!(report, CatchupReport::BaselineApplied(_)));
    assert_eq!(corpus_status(&client)?[0].sequence, 1);
    Ok(())
}
