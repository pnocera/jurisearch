//! INFRA-GATED acceptance (skips cleanly without `JURISEARCH_PG_CONFIG`): the M3 `rebaseline_cycle`
//! publishes a signed `core` REBASELINE at the served root and advances the manifest, using the SAME
//! build/publish/manifest primitives a normal cycle uses. `rebaseline_cycle` is generic over
//! `DbClientSource`, so the producer drives it against an EXTERNAL PostgreSQL with this exact code path.
//!
//! Proves:
//! - after a baseline + a published manifest (head 1), `rebaseline_cycle` publishes a `Rebaseline`
//!   package that advances the head to 2 and flips `active_baseline.package_kind` to `rebaseline`;
//! - `verify_published_root` accepts the resulting served root (every referenced artifact present +
//!   integrity-checked) — the convergence/integrity gate existing and fresh sites re-anchor through.

use std::collections::BTreeMap;

use jurisearch_package::PackageKind;
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, Ed25519Signer, KeyEpoch, KeyId};
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, EnrichmentMode, ProducerCycleConfig, PublishFault, RebaselineCycleConfig,
    RemoteManifestParams, build_baseline, producer_cycle, publish_package, published_manifest_path,
    rebaseline_cycle, rebaseline_cycle_faulted, staged_pending_dir, verify_published_root,
};
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};

const URI_BASE: &str = "media://";
const FP: &str = "bge-m3:1024:normalize:true";

fn signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[7u8; 32], KeyId("producer-k".to_owned()), KeyEpoch(1))
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
        created_at: "2026-06-29T00:00:00Z".to_owned(),
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
        generated_at: "2026-06-29T03:00:00Z".to_owned(),
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

fn mutate(producer: &ManagedPostgres, sql: &str) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("rebaseline-seed", 24);
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
fn rebaseline_cycle_publishes_a_verifiable_rebaseline_and_advances_the_head()
-> Result<(), StorageError> {
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

    // Operator one-time baseline (head 1) + an empty cycle to publish the initial signed manifest.
    let base_art = tempfile::tempdir().map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params("core-2025-g0001"),
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
            incremental_params: jurisearch_package_build::IncrementalParams {
                builder_run_id: "init".to_owned(),
                created_at: "2026-06-29T01:00:00Z".to_owned(),
                embedding_fingerprint: FP.to_owned(),
                embedding_model: "bge-m3".to_owned(),
                embedding_dimension: 1024,
                embedding_normalize: true,
                builder_versions: {
                    let mut bv = BTreeMap::new();
                    bv.insert("chunker".to_owned(), "c1".to_owned());
                    bv
                },
                minimum_client_version: Version::new(0, 1, 0),
            },
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("init cycle");
    assert_eq!(manifest(served.path()).payload.head_sequence.get(), 1);
    assert_eq!(
        manifest(served.path()).payload.active_baseline.package_kind,
        PackageKind::Baseline
    );

    // A newer DILA baseline was ingested (model the changed corpus with a mutation); adopt it via the
    // RECORDED rebaseline cycle — a full re-anchor, not a delta across the boundary.
    mutate(
        &producer,
        "UPDATE documents SET title='rebased' WHERE document_id='cass:P1';",
    )?;
    let report = rebaseline_cycle(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &RebaselineCycleConfig {
            baseline_params: baseline_params("core-2026-g-rebaseline"),
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("rebaseline cycle publishes");
    assert_eq!(report.from_sequence, 1);
    assert_eq!(report.to_sequence, 2);

    // The head advanced to the rebaseline, and the active baseline now dispatches the REBASELINE applier.
    let m = manifest(served.path());
    assert_eq!(
        m.payload.head_sequence.get(),
        2,
        "head advanced to the rebaseline"
    );
    assert_eq!(
        m.payload.active_baseline.package_kind,
        PackageKind::Rebaseline,
        "the served manifest re-anchors fresh + existing sites to the rebaseline"
    );

    // The served root verifies end to end (every referenced artifact present + integrity-checked) — the
    // convergence/integrity gate a site re-anchors through.
    let verified = verify_published_root(served.path(), "core", URI_BASE, &AcceptAllVerifier)
        .expect("published root verifies");
    assert_eq!(verified.head_sequence, 2);
    Ok(())
}

/// BLOCKER gate (rebaseline discard-and-rebuild, M3 r3): a rebaseline publish that crashes AFTER the
/// catalog row + staged artifact but BEFORE publish must NOT strand the producer on an unpublished
/// package. The next `rebaseline_cycle` DISCARDS the incomplete attempt (its orphaned `'built'` row +
/// staging slot) and rebuilds a FRESH rebaseline from the current DB head — which, since the prior head
/// is still the baseline, lands on the SAME `package_id` `core-1-2`. The manifest advances only after that
/// artifact exists, and exactly ONE rebaseline is ever cataloged (the orphaned row was deleted, not left
/// to conflict the re-insert or surface in the manifest).
#[test]
fn a_pre_publish_rebaseline_failure_is_discarded_and_rebuilt_to_one_published_package()
-> Result<(), StorageError> {
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

    // Operator one-time baseline (head 1) + an empty cycle to publish the initial signed manifest.
    let base_art = tempfile::tempdir().map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params("core-2025-g0001"),
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
            incremental_params: jurisearch_package_build::IncrementalParams {
                builder_run_id: "init".to_owned(),
                created_at: "2026-06-29T01:00:00Z".to_owned(),
                embedding_fingerprint: FP.to_owned(),
                embedding_model: "bge-m3".to_owned(),
                embedding_dimension: 1024,
                embedding_normalize: true,
                builder_versions: {
                    let mut bv = BTreeMap::new();
                    bv.insert("chunker".to_owned(), "c1".to_owned());
                    bv
                },
                minimum_client_version: Version::new(0, 1, 0),
            },
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("init cycle");
    assert_eq!(manifest(served.path()).payload.head_sequence.get(), 1);

    // A newer DILA baseline was ingested (model the changed corpus); the rebaseline crashes AFTER the
    // catalog row + staged artifact, BEFORE publish.
    mutate(
        &producer,
        "UPDATE documents SET title='rebased' WHERE document_id='cass:P1';",
    )?;
    let cfg = || RebaselineCycleConfig {
        baseline_params: baseline_params("core-2026-g-rebaseline"),
        remote_manifest_params: remote_manifest_params(&sgnr),
        enrichment: EnrichmentMode::Disabled,
    };
    let faulted = rebaseline_cycle_faulted(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &cfg(),
        PublishFault::AfterStageBeforePublish,
    );
    assert!(
        faulted.is_err(),
        "the injected pre-publish fault propagates"
    );

    // The catalog row exists as `built`, the artifact is NOT published, but it IS staged for resume, and
    // the manifest has NOT advanced past the baseline.
    let status =
        producer.execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-2';")?;
    assert_eq!(
        status.trim(),
        "built",
        "the rebaseline row is cataloged but unpublished"
    );
    let published_dir = served.path().join("core").join("packages").join("core-1-2");
    assert!(
        !published_dir.exists(),
        "no served artifact after the failed publish"
    );
    assert!(
        staged_pending_dir(served.path(), "core")
            .join("manifest.json")
            .exists(),
        "the built rebaseline is staged for resume"
    );
    assert_eq!(
        manifest(served.path()).payload.head_sequence.get(),
        1,
        "manifest did not advance over a missing artifact"
    );

    // Re-run: the cycle DISCARDS the incomplete attempt and rebuilds fresh, landing on the SAME id
    // (the published head is still the baseline), publishes it, and advances once.
    let resumed = rebaseline_cycle(&producer, "core", served.path(), &sgnr, &cfg())
        .expect("rebuilt rebaseline succeeds");
    assert_eq!(
        resumed.package_id, "core-1-2",
        "the rebuilt rebaseline takes the SAME id over the baseline head, not a successor"
    );
    assert_eq!(resumed.from_sequence, 1);
    assert_eq!(resumed.to_sequence, 2);
    assert!(
        published_dir.exists(),
        "the rebaseline now exists at the served root"
    );
    let m = manifest(served.path());
    assert_eq!(
        m.payload.head_sequence.get(),
        2,
        "manifest advanced to the rebuilt rebaseline"
    );
    assert_eq!(
        m.payload.active_baseline.package_kind,
        PackageKind::Rebaseline
    );
    let status =
        producer.execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-2';")?;
    assert_eq!(status.trim(), "published", "the rebuilt row is published");

    // Exactly once: only ONE rebaseline was ever cataloged (the orphaned `'built'` row was discarded,
    // not left to conflict the re-insert or chain a successor `core-2-3`).
    let count = producer
        .execute_sql("SELECT count(*) FROM package_catalog WHERE package_kind='rebaseline';")?;
    assert_eq!(
        count.trim(),
        "1",
        "exactly one rebaseline was ever cataloged"
    );
    Ok(())
}

/// BLOCKER gate (Codex r3 — cross-baseline discard-and-rebuild): a rebaseline staged for an OLDER baseline
/// B1 that crashed before publish must, on a rerun by a run routed for a NEWER baseline B2, be DISCARDED
/// and REBUILT FRESH from the current DB state — so the published artifact re-anchors to B2 (the run's
/// intended identity), NOT a stale B1. The published baseline set always equals the current run's pending
/// baselines, so the producer adopts B2 exactly (per-source) with no stale-resume identity to reconcile,
/// and exactly ONE rebaseline is cataloged (B1's orphaned `'built'` row was deleted, not chained over).
#[test]
fn a_crashed_rebaseline_is_discarded_and_rebuilt_fresh_for_the_current_baseline()
-> Result<(), StorageError> {
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

    // Operator one-time baseline (head 1) + an empty cycle to publish the initial signed manifest.
    let base_art = tempfile::tempdir().map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params("core-2025-g0001"),
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
            incremental_params: jurisearch_package_build::IncrementalParams {
                builder_run_id: "init".to_owned(),
                created_at: "2026-06-29T01:00:00Z".to_owned(),
                embedding_fingerprint: FP.to_owned(),
                embedding_model: "bge-m3".to_owned(),
                embedding_dimension: 1024,
                embedding_normalize: true,
                builder_versions: {
                    let mut bv = BTreeMap::new();
                    bv.insert("chunker".to_owned(), "c1".to_owned());
                    bv
                },
                minimum_client_version: Version::new(0, 1, 0),
            },
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("init cycle");
    assert_eq!(manifest(served.path()).payload.head_sequence.get(), 1);

    // Run A: routed for baseline B1. Ingest B1 (model the changed corpus), then crash AFTER the catalog
    // row + staged artifact, BEFORE publish — the rebaseline `core-1-2` is staged carrying baseline_id B1.
    const B1: &str = "core-baseline-b1";
    const B2: &str = "core-baseline-b2";
    mutate(
        &producer,
        "UPDATE documents SET title='rev-b1' WHERE document_id='cass:P1';",
    )?;
    let cfg_b1 = || RebaselineCycleConfig {
        baseline_params: baseline_params(B1),
        remote_manifest_params: remote_manifest_params(&sgnr),
        enrichment: EnrichmentMode::Disabled,
    };
    let faulted = rebaseline_cycle_faulted(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &cfg_b1(),
        PublishFault::AfterStageBeforePublish,
    );
    assert!(
        faulted.is_err(),
        "the injected pre-publish fault propagates"
    );
    assert!(
        staged_pending_dir(served.path(), "core")
            .join("manifest.json")
            .exists(),
        "the B1 rebaseline is staged for resume"
    );

    // Run B: routed for the NEWER baseline B2 (fetch advanced the cursor); ingest B2's change, then the
    // rebaseline cycle DISCARDS the staged B1 artifact and REBUILDS FRESH from the current DB. The
    // rebuilt artifact incorporates the current state (both B1's and B2's changes) and re-anchors to B2 —
    // so the report MUST carry B2's `baseline_id`, NOT the stale B1 it discarded. The id is still
    // `core-1-2` because the published head is still the baseline.
    mutate(
        &producer,
        "UPDATE documents SET title='rev-b2' WHERE document_id='cass:P1';",
    )?;
    let rebuilt = rebaseline_cycle(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &RebaselineCycleConfig {
            baseline_params: baseline_params(B2),
            remote_manifest_params: remote_manifest_params(&sgnr),
            enrichment: EnrichmentMode::Disabled,
        },
    )
    .expect("discard-and-rebuild publishes a fresh B2 rebaseline");
    assert_eq!(
        rebuilt.package_id, "core-1-2",
        "the rebuilt rebaseline takes the id over the baseline head"
    );
    assert_eq!(
        rebuilt.baseline_id, B2,
        "the rebuilt artifact re-anchors to the run's intended B2, not the discarded stale B1"
    );
    assert_eq!(rebuilt.from_sequence, 1);
    assert_eq!(rebuilt.to_sequence, 2);
    assert_eq!(
        manifest(served.path()).payload.active_baseline.baseline_id,
        B2,
        "the served manifest re-anchored to B2 (the current pending baseline)"
    );

    // Exactly ONE rebaseline was ever cataloged: B1's orphaned `'built'` row was discarded during the
    // rebuild, never published and never chained over.
    let count = producer
        .execute_sql("SELECT count(*) FROM package_catalog WHERE package_kind='rebaseline';")?;
    assert_eq!(
        count.trim(),
        "1",
        "only the rebuilt B2 rebaseline is cataloged; the discarded B1 left no row"
    );
    Ok(())
}
