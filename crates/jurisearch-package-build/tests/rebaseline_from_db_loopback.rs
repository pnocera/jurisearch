//! INFRA-GATED acceptance (skips cleanly without `JURISEARCH_PG_CONFIG`): the `--from-db` snapshot-only
//! rebaseline PRIMITIVES at the package-build layer. The producer's `rebaseline --from-db` skips
//! fetch/ingest/enrich/embed and drives the SAME `rebaseline_cycle` after a `rebaseline_preflight` media
//! coverage gate; those two calls are exactly what this test exercises against an EXTERNAL PostgreSQL.
//!
//! Proves:
//! - `rebaseline_preflight` ACCEPTS a fully-embedded, fingerprint-consistent corpus;
//! - a from-DB rebaseline over a head at package-sequence 2 (generation `core_g0001`) publishes
//!   `core-2-3`, generation `core_g0002`, flips `active_baseline.package_kind` to `rebaseline`, advances
//!   the head to 3, and RETAINS the older `core-1-1` baseline artifact directory;
//! - `rebaseline_preflight` REFUSES an under-embedded corpus (a chunk with no matching embedding) and a
//!   fingerprint-inconsistent corpus — the fail-closed guard the snapshot-only path relies on because it
//!   deliberately skips the embed pass.

use std::collections::BTreeMap;

use jurisearch_package::PackageKind;
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, Ed25519Signer, KeyEpoch, KeyId};
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, EnrichmentMode, IncrementalParams, ProducerCycleConfig, RebaselineCycleConfig,
    RemoteManifestParams, build_baseline, producer_cycle, publish_package, published_manifest_path,
    rebaseline_cycle, rebaseline_preflight, verify_published_root,
};
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};

const URI_BASE: &str = "media://";
const FP: &str = "bge-m3:1024:normalize:true";

fn signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[9u8; 32], KeyId("producer-k".to_owned()), KeyEpoch(1))
}

fn vector() -> String {
    format!("[{}]", vec!["0.01"; 1024].join(","))
}

fn baseline_params(id: &str) -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: id.to_owned(),
        builder_run_id: "b0".to_owned(),
        created_at: "2026-07-01T00:00:00Z".to_owned(),
        embedding_fingerprint: FP.to_owned(),
        embedding_model: "bge-m3".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn incremental_params(run: &str, at: &str) -> IncrementalParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    IncrementalParams {
        builder_run_id: run.to_owned(),
        created_at: at.to_owned(),
        embedding_fingerprint: FP.to_owned(),
        embedding_model: "bge-m3".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn remote_manifest_params(signer: &Ed25519Signer) -> RemoteManifestParams {
    RemoteManifestParams {
        publisher: "jurisearch".to_owned(),
        environment: "test".to_owned(),
        generated_at: "2026-07-01T03:00:00Z".to_owned(),
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 100,
            max_cumulative_diff_to_baseline_permille: 100_000,
            max_cumulative_uncompressed_to_baseline_permille: 100_000,
            max_apply_seconds_budget: 2700,
        },
        entitlement_tier: EntitlementTier::Open,
        license_epoch: 0,
        audience: None,
        signing_key_id: signer.key_id().clone(),
        uri_base: URI_BASE.to_owned(),
        max_retained_incrementals: 200,
        default_apply_seconds: 5,
        default_load_seconds: 600,
    }
}

/// Seed one fully-embedded document+chunk (mirrors the fixture in `rebaseline_cycle_loopback`).
fn seed(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P1','cass','decision','cass:P1','Cass','Arret','corps','2024-01-01', \
           'sha256:p1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P1#0','cass:P1',0,'corps','ctx corps','sha256:c','c1','bge-m3:1024:normalize:true');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:P1#0','{FP}','{}'::vector,'bge-m3',1024);",
        vector(),
    ))?;
    Ok(())
}

/// Apply `sql` and emit ONE outbox change so the next `producer_cycle` builds a non-empty incremental.
fn mutate(producer: &ManagedPostgres, sql: &str) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("from-db-seed", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope(
            "core",
            "documents",
            EventKind::Upsert,
            scope_kind::DOCUMENT,
            "cass:P1",
        ),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

fn manifest(root: &std::path::Path) -> Signed<RemoteManifest> {
    let bytes = std::fs::read(published_manifest_path(root, "core")).expect("manifest bytes");
    serde_json::from_slice(&bytes).expect("manifest json")
}

#[test]
fn from_db_rebaseline_publishes_core_2_3_and_preflight_guards_coverage() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        eprintln!("SKIP: no pgvector/pg_search assets via JURISEARCH_PG_CONFIG");
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?;

    let served = tempfile::tempdir().map_err(StorageError::Io)?;

    // Operator one-time baseline (head 1, generation core_g0001) + an empty cycle to publish the manifest.
    let base_art = tempfile::tempdir().map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params("core-bootstrap-v1"),
    )
    .expect("baseline builds");
    publish_package(served.path(), "core", &base.package_id, base_art.path())
        .expect("publish base");
    producer_cycle(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &ProducerCycleConfig {
            incremental_params: incremental_params("init", "2026-07-01T01:00:00Z"),
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("init cycle");
    assert_eq!(manifest(served.path()).payload.head_sequence.get(), 1);

    // An ordinary incremental advances the head to package-sequence 2 WITHOUT bumping the generation
    // (`active_baseline` stays the core-1-1 baseline at core_g0001) — shaping the "head at 2 / g0001"
    // state a from-DB rebaseline supersedes into core-2-3 / core_g0002.
    mutate(
        &producer,
        "UPDATE documents SET title='v2' WHERE document_id='cass:P1';",
    )?;
    producer_cycle(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &ProducerCycleConfig {
            incremental_params: incremental_params("inc-1", "2026-07-01T02:00:00Z"),
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("incremental cycle");
    let head2 = manifest(served.path());
    assert_eq!(head2.payload.head_sequence.get(), 2, "head advanced to 2");
    assert_eq!(
        head2.payload.active_baseline.generation, "core_g0001",
        "an incremental does not bump the active generation"
    );

    // A `--from-db` run runs the media coverage preflight BEFORE building — the clean, fully-embedded
    // corpus passes.
    rebaseline_preflight(&producer, &baseline_params("core-2026-from-db"))
        .expect("preflight accepts a fully-embedded, fingerprint-consistent corpus");

    // The snapshot-only publish path itself: `rebaseline_cycle` over the current locked DB state.
    let report = rebaseline_cycle(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &RebaselineCycleConfig {
            baseline_params: baseline_params("core-2026-from-db"),
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("from-db rebaseline publishes");
    assert_eq!(
        report.package_id, "core-2-3",
        "head 2 supersedes to core-2-3"
    );
    assert_eq!(report.from_sequence, 2);
    assert_eq!(report.to_sequence, 3);

    let m = manifest(served.path());
    assert_eq!(
        m.payload.head_sequence.get(),
        3,
        "head advanced to the rebaseline"
    );
    assert_eq!(
        m.payload.active_baseline.package_kind,
        PackageKind::Rebaseline,
        "active baseline flips to the rebaseline applier"
    );
    assert_eq!(
        m.payload.active_baseline.generation, "core_g0002",
        "the rebaseline bumps the generation counter to g0002"
    );

    // Old artifacts are RETAINED (immutable published dirs; nothing deletes prior packages).
    assert!(
        served
            .path()
            .join("core")
            .join("packages")
            .join("core-1-1")
            .is_dir(),
        "the original core-1-1 baseline artifact directory is retained"
    );

    // The served root verifies end to end after the from-DB rebaseline.
    let verified = verify_published_root(served.path(), "core", URI_BASE, &AcceptAllVerifier)
        .expect("published root verifies");
    assert_eq!(verified.head_sequence, 3);

    // --- Preflight fail-closed cases (the guard the snapshot-only path relies on) ---

    // Under-embedded: a new chunk with NO matching chunk_embeddings row.
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P2','cass','decision','cass:P2','Cass','Arret2','corps2','2024-01-02', \
           'sha256:p2','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P2#0','cass:P2',0,'corps2','ctx corps2','sha256:c2','c1','bge-m3:1024:normalize:true');",
    )?;
    let missing = rebaseline_preflight(&producer, &baseline_params("core-2026-from-db"));
    assert!(
        missing.is_err(),
        "preflight must REFUSE a corpus with an un-embedded chunk"
    );

    // Fingerprint-inconsistent: embed the new chunk but stamp a different fingerprint on the chunk row.
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:P2#0','{FP}','{}'::vector,'bge-m3',1024); \
         UPDATE chunks SET embedding_fingerprint='bge-m3:512:normalize:false' WHERE chunk_id='cass:P2#0';",
        vector(),
    ))?;
    let inconsistent = rebaseline_preflight(&producer, &baseline_params("core-2026-from-db"));
    assert!(
        inconsistent.is_err(),
        "preflight must REFUSE a corpus whose chunk fingerprint diverges from the publish fingerprint"
    );

    Ok(())
}
