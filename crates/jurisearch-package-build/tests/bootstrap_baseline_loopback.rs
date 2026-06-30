//! INFRA-GATED acceptance (skips cleanly without `JURISEARCH_PG_CONFIG`): `bootstrap_first_baseline`
//! publishes an EXISTING in-DB `core` corpus as the producer's FIRST signed `core` baseline — WITHOUT
//! fetching or re-embedding — over a `DbClientSource`, and self-verifies the served root.
//!
//! Proves:
//! - a seeded + embedded corpus bootstraps to a `published` `core-1-1` catalog row, a served artifact +
//!   signed `core/manifest.json`, and `verify_published_root` accepts the result (head 1);
//! - a fresh `jurisearch-syncd` client (the REAL `DirectoryCatchupSource` + `fetch_verify_manifest` path)
//!   applies the published baseline and converges to the producer head;
//! - finalization is RESUMABLE: a crash after the catalog row/artifact but before `manifest.json` is
//!   re-run cleanly, and a corrupted published artifact is rejected PRE-rename (no client-visible
//!   manifest ever points at a missing/invalid artifact);
//! - the fail-closed preflights reject: an already-published baseline, a divergent catalog row (manual
//!   repair), a schema-version mismatch, missing chunk embeddings, missing zone embeddings, and a wrong
//!   publish fingerprint.

use std::collections::BTreeMap;
use std::path::Path;

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{Ed25519Signer, Ed25519Verifier, KeyEpoch, KeyId};
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, BootstrapBaselineConfig, BootstrapFault, RemoteManifestParams,
    bootstrap_first_baseline, bootstrap_first_baseline_faulted, published_manifest_path,
    verify_published_root,
};
use jurisearch_storage::outbox::{DigestSource, corpus_table_digests};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_storage::trust::{PACKAGE_PURPOSE, install_trust_anchor};
use jurisearch_syncd::{
    CatchupPlan, CatchupReport, DirectoryCatchupSource, corpus_status, fetch_verify_manifest,
    load_package_verifier, plan_catchup, run_catchup,
};

const URI_BASE: &str = "media://";
const FP: &str = "bge-m3:1024:normalize:true";

fn signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[9u8; 32], KeyId("producer-k1".to_owned()), KeyEpoch(1))
}

fn verifier(signer: &Ed25519Signer) -> Ed25519Verifier {
    Ed25519Verifier::from_anchors(&[signer.trust_anchor()]).expect("verifier")
}

fn vector() -> String {
    format!("[{}]", vec!["0.01"; 1024].join(","))
}

fn baseline_params(fingerprint: &str) -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: "core-bootstrap-v1".to_owned(),
        builder_run_id: "bootstrap".to_owned(),
        created_at: "2026-06-30T00:00:00Z".to_owned(),
        embedding_fingerprint: fingerprint.to_owned(),
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
        generated_at: "2026-06-30T03:00:00Z".to_owned(),
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

fn bootstrap_config(signer: &Ed25519Signer, fingerprint: &str) -> BootstrapBaselineConfig {
    BootstrapBaselineConfig {
        baseline_params: baseline_params(fingerprint),
        remote_manifest_params: remote_manifest_params(signer),
    }
}

/// Seed one fully-embedded document + chunk (the minimal query-ready `core` corpus). No zone units.
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

fn manifest(root: &Path) -> Signed<RemoteManifest> {
    let bytes = std::fs::read(published_manifest_path(root, "core")).expect("manifest bytes");
    serde_json::from_slice(&bytes).expect("manifest json")
}

/// A seeded, migrated producer + an empty served root (the common setup for the standard-corpus tests).
struct Harness {
    _proot: tempfile::TempDir,
    producer: ManagedPostgres,
    served: tempfile::TempDir,
}

fn harness(pg_config: &PgConfig) -> Result<Harness, StorageError> {
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?;
    let served = tempfile::tempdir().map_err(StorageError::Io)?;
    Ok(Harness {
        _proot: proot,
        producer,
        served,
    })
}

/// Flip the first hex nibble — a different but valid-length lowercase-hex string (a signature that will
/// no longer verify). Used to corrupt a published artifact's embedded signature on disk.
fn flip_first_hex(hex: &str) -> String {
    let mut chars: Vec<char> = hex.chars().collect();
    if let Some(c) = chars.first_mut() {
        *c = if *c == '0' { '1' } else { '0' };
    }
    chars.into_iter().collect()
}

/// STRONGEST PROOF: a fresh `jurisearch-syncd` client, through the REAL `DirectoryCatchupSource` +
/// `fetch_verify_manifest` path, verifies the signed manifest, plans + applies the published baseline,
/// and converges to the producer head (table-digest match).
fn assert_fresh_client_converges(
    pg_config: &PgConfig,
    producer: &ManagedPostgres,
    served: &Path,
) -> Result<(), StorageError> {
    let sgnr = signer();
    let croot = tempfile::tempdir().map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config.clone(), croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &sgnr.trust_anchor(), PACKAGE_PURPOSE)?;
    }
    let client_verifier = load_package_verifier(&client).expect("client verifier");

    // The real client transport: read+verify `<served>/core/manifest.json`, resolve each `artifact_uri`.
    let source = DirectoryCatchupSource::new(served.to_path_buf(), URI_BASE);
    let manifest = fetch_verify_manifest(&source, &client_verifier, "core")
        .expect("client verifies the manifest");
    assert_eq!(
        manifest.head_sequence.get(),
        1,
        "the manifest head is the first baseline"
    );

    let plan = plan_catchup(&manifest, None);
    assert!(
        matches!(plan, CatchupPlan::FreshBaseline(_)),
        "a fresh client routes to the baseline"
    );
    let applied = run_catchup(&client, &source, &client_verifier, plan).expect("client applies");
    assert!(matches!(applied, CatchupReport::BaselineApplied(_)));
    assert_eq!(corpus_status(&client)?[0].sequence, 1);

    let producer_digests = corpus_table_digests(producer, "core", DigestSource::ProducerPublic)?;
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
    Ok(())
}

#[test]
fn bootstrap_publishes_a_verifiable_first_baseline_a_fresh_client_applies()
-> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        eprintln!("SKIP: no pgvector/pg_search assets via JURISEARCH_PG_CONFIG");
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);

    // Producer: a hand-loaded, fully-embedded corpus that bypassed the publisher (no catalog rows).
    let h = harness(&pg_config)?;

    // Bootstrap publishes the first signed baseline + self-verifies.
    let report = bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect("bootstrap publishes the first baseline");
    assert_eq!(report.package_id, "core-1-1");
    assert_eq!(report.generation, "core_g0001");
    assert_eq!(report.sequence, 1);
    assert_eq!(report.head_sequence, 1);
    assert!(
        report.artifacts_verified >= 1,
        "at least the baseline verified"
    );

    // The catalog row is PUBLISHED, the served artifact + signed manifest exist, and the root verifies.
    let status = h
        .producer
        .execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-1';")?;
    assert_eq!(status.trim(), "published", "the baseline row is published");
    assert!(
        h.served
            .path()
            .join("core/packages/core-1-1/manifest.json")
            .exists(),
        "served artifact exists"
    );
    assert!(
        published_manifest_path(h.served.path(), "core").exists(),
        "signed remote manifest exists"
    );
    assert_eq!(manifest(h.served.path()).payload.head_sequence.get(), 1);
    let verified = verify_published_root(h.served.path(), "core", URI_BASE, &vrf)
        .expect("published root verifies");
    assert_eq!(verified.head_sequence, 1);

    // STRONGEST PROOF: the real syncd client path converges to the producer head.
    assert_fresh_client_converges(&pg_config, &h.producer, h.served.path())?;
    Ok(())
}

/// Resume after a crash that left a `built` catalog row + STAGED artifact (no `packages/` artifact, no
/// manifest): the re-run publishes the staged artifact, marks it, and finalizes a verifiable root.
#[test]
fn bootstrap_resumes_after_a_crash_before_publish() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    assert_crash_then_resume(&pg_config, BootstrapFault::AfterCatalogInsert)
}

/// Resume after a crash that left a SERVED artifact but a still-`built` row (no manifest): the re-run
/// marks it published and finalizes.
#[test]
fn bootstrap_resumes_after_a_crash_before_mark_published() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    assert_crash_then_resume(&pg_config, BootstrapFault::AfterPublishPackage)
}

/// Resume after a crash that left a SERVED, PUBLISHED artifact but no manifest: the re-run only needs to
/// build + publish the manifest.
#[test]
fn bootstrap_resumes_after_a_crash_before_manifest() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    assert_crash_then_resume(&pg_config, BootstrapFault::AfterMarkPublished)
}

/// For a given crash boundary: inject the fault, assert NO client-visible manifest exists at that
/// boundary, then run the normal bootstrap and assert it finalizes cleanly (published `core-1-1`, manifest
/// present + verifies, head 1) and that a fresh client converges. Each re-run never leaves a manifest
/// pointing at a missing artifact.
fn assert_crash_then_resume(
    pg_config: &PgConfig,
    fault: BootstrapFault,
) -> Result<(), StorageError> {
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let h = harness(pg_config)?;

    bootstrap_first_baseline_faulted(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
        fault,
    )
    .expect_err("the injected fault aborts the bootstrap");
    assert!(
        !published_manifest_path(h.served.path(), "core").exists(),
        "no client-visible manifest at the {fault:?} crash boundary"
    );

    let report = bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect("the re-run resumes + finalizes cleanly");
    assert_eq!(report.package_id, "core-1-1");
    assert_eq!(report.head_sequence, 1);

    let status = h
        .producer
        .execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-1';")?;
    assert_eq!(status.trim(), "published", "row published after resume");
    assert!(
        published_manifest_path(h.served.path(), "core").exists(),
        "manifest published after resume"
    );
    let verified = verify_published_root(h.served.path(), "core", URI_BASE, &vrf)
        .expect("the resumed root verifies");
    assert_eq!(verified.head_sequence, 1);

    assert_fresh_client_converges(pg_config, &h.producer, h.served.path())?;
    Ok(())
}

/// BLOCKER-2 regression: after a crash that published the artifact but no manifest, CORRUPTING the
/// published artifact's embedded signature must cause the resuming bootstrap to FAIL — the PRE-rename
/// verify rejects it, so NO client-visible manifest is ever published over a broken artifact.
#[test]
fn bootstrap_resume_rejects_a_corrupted_published_artifact() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let h = harness(&pg_config)?;

    // Crash after mark-published: a served, published artifact with no manifest.
    bootstrap_first_baseline_faulted(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
        BootstrapFault::AfterMarkPublished,
    )
    .expect_err("the injected fault aborts the bootstrap");
    assert!(
        !published_manifest_path(h.served.path(), "core").exists(),
        "no manifest at the crash boundary"
    );

    // Corrupt ONLY the published artifact's embedded signature (payload + digests unchanged).
    let pkg_manifest = h.served.path().join("core/packages/core-1-1/manifest.json");
    let bytes = std::fs::read(&pkg_manifest).map_err(StorageError::Io)?;
    let mut signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&bytes).expect("embedded manifest json");
    signed.signature.signature_hex = flip_first_hex(&signed.signature.signature_hex);
    std::fs::write(
        &pkg_manifest,
        serde_json::to_vec(&signed).expect("serialize embedded manifest"),
    )
    .map_err(StorageError::Io)?;

    // The resume builds the manifest but the pre-rename verify rejects the corrupted embedded signature.
    bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect_err("a corrupted published artifact is rejected pre-rename");
    assert!(
        !published_manifest_path(h.served.path(), "core").exists(),
        "no client-visible manifest after a rejected resume"
    );
    Ok(())
}

#[test]
fn bootstrap_refuses_an_already_published_baseline() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let h = harness(&pg_config)?;

    bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect("first bootstrap publishes");
    let again = bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    );
    let err = again.expect_err("a second bootstrap is refused");
    let msg = err.to_string();
    assert!(
        msg.contains("already published") && msg.contains("verifies"),
        "refusal names the already-published, verifying baseline: {err}"
    );
    // The first run's manifest is untouched and still verifies.
    assert!(
        published_manifest_path(h.served.path(), "core").exists(),
        "the published manifest is preserved"
    );
    verify_published_root(h.served.path(), "core", URI_BASE, &vrf)
        .expect("the preserved manifest still verifies");
    Ok(())
}

/// A canonical-first-baseline row that has been TAMPERED to a non-canonical identity is a manual-repair
/// situation: the bootstrap refuses with a PRECISE message naming the diverging field, distinct from the
/// "already baselined" refusal.
#[test]
fn bootstrap_refuses_a_divergent_catalog_row() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let h = harness(&pg_config)?;

    bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect("first bootstrap publishes");

    // Tamper the catalog identity + remove the manifest so the re-run takes the classify path.
    h.producer.execute_sql(
        "UPDATE package_catalog SET baseline_id='tampered' WHERE package_id='core-1-1';",
    )?;
    std::fs::remove_file(published_manifest_path(h.served.path(), "core"))
        .map_err(StorageError::Io)?;

    let err = bootstrap_first_baseline(
        &h.producer,
        "core",
        h.served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect_err("a divergent catalog row is refused");
    let msg = err.to_string();
    assert!(
        msg.contains("baseline_id") && msg.contains("manual repair"),
        "precise repair-refusal names the diverging field: {err}"
    );
    assert!(
        !msg.contains("already"),
        "the repair-refusal is distinct from the already-baselined refusal: {err}"
    );
    Ok(())
}

#[test]
fn bootstrap_rejects_a_schema_version_mismatch() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?;
    // Drop the highest migration row so max(version) no longer equals CURRENT_SCHEMA_VERSION.
    producer.execute_sql(
        "DELETE FROM schema_migrations WHERE version = (SELECT max(version) FROM schema_migrations);",
    )?;
    let served = tempfile::tempdir().map_err(StorageError::Io)?;

    let err = bootstrap_first_baseline(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect_err("a schema mismatch is rejected");
    assert!(
        err.to_string().contains("schema_version"),
        "rejection names the schema version: {err}"
    );
    assert!(
        !published_manifest_path(served.path(), "core").exists(),
        "no manifest published on a rejected bootstrap"
    );
    Ok(())
}

#[test]
fn bootstrap_rejects_missing_chunk_embeddings() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    // A document + chunk WITHOUT a chunk_embeddings row (the corpus is not fully embedded).
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P1','cass','decision','cass:P1','Cass','Arret','corps','2024-01-01', \
           'sha256:p1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P1#0','cass:P1',0,'corps','ctx corps','sha256:c','c1','bge-m3:1024:normalize:true');",
    )?;
    let served = tempfile::tempdir().map_err(StorageError::Io)?;

    let err = bootstrap_first_baseline(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect_err("missing chunk embeddings are rejected");
    assert!(
        err.to_string().contains("chunk_embeddings"),
        "rejection names chunk_embeddings: {err}"
    );
    assert!(
        !published_manifest_path(served.path(), "core").exists(),
        "no manifest published on a rejected bootstrap"
    );
    Ok(())
}

#[test]
fn bootstrap_rejects_missing_zone_embeddings() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?; // chunks fully embedded (so chunk checks pass)
    // A zone unit with NO matching zone_unit_embeddings row (fingerprint stamped, but unembedded).
    producer.execute_sql(
        "INSERT INTO zone_units (zone_unit_id, document_id, zone, fragment_index, body, search_body, \
           source, text_hash, zone_unit_builder_version, embedding_fingerprint) \
         VALUES ('cass:P1#z0','cass:P1','motivations',0,'zone body','zone search', \
           'cass','sha256:z','z1','bge-m3:1024:normalize:true');",
    )?;
    let served = tempfile::tempdir().map_err(StorageError::Io)?;

    let err = bootstrap_first_baseline(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, FP),
    )
    .expect_err("missing zone embeddings are rejected");
    assert!(
        err.to_string().contains("zone_unit_embeddings"),
        "rejection names zone_unit_embeddings: {err}"
    );
    assert!(
        !published_manifest_path(served.path(), "core").exists(),
        "no manifest published on a rejected bootstrap"
    );
    Ok(())
}

#[test]
fn bootstrap_rejects_a_wrong_fingerprint() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let vrf = verifier(&sgnr);
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?; // embedded under the v1 locked fingerprint
    let served = tempfile::tempdir().map_err(StorageError::Io)?;

    // The `jurisearch-package` CLI `:cls:` default — NOT the producer storage fingerprint. No row matches.
    let wrong = "bge-m3:1024:cls:normalize=true";
    let err = bootstrap_first_baseline(
        &producer,
        "core",
        served.path(),
        &sgnr,
        &vrf,
        &bootstrap_config(&sgnr, wrong),
    )
    .expect_err("a wrong publish fingerprint is rejected");
    assert!(
        err.to_string().contains(wrong),
        "rejection names the wrong fingerprint: {err}"
    );
    assert!(
        !published_manifest_path(served.path(), "core").exists(),
        "no manifest published on a rejected bootstrap"
    );
    Ok(())
}
