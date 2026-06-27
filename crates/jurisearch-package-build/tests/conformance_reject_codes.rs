//! P10 reject-code conformance (design §6.3 / INV-9): prove that EVERY closed-vocabulary `RejectCode`
//! is produced by at least one real plan/apply/trust path — the codes are collected into a set and
//! asserted equal to `RejectCode::all()`. The per-phase suites keep the behavioural detail; this only
//! drives each refusal once and asserts the structured `SyncError::Reject { code, .. }` (not substrings).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use jurisearch_package::artifact;
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{Ed25519Signer, KeyEpoch, KeyId};
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::embedded::ExtensionRequirement;
use jurisearch_package::reject::RejectCode;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental,
};
use jurisearch_storage::generations::APPLY_ADVISORY_LOCK_KEY;
use jurisearch_storage::outbox::{OutboxContext, OutboxEvent, emit_change, scope_kind};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_storage::trust::{LICENSE_PURPOSE, PACKAGE_PURPOSE};
use jurisearch_syncd::{
    SyncError, apply_baseline, apply_incremental, install_trust_anchor, load_package_verifier,
};

fn pkg_signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[4u8; 32], KeyId("producer-k".to_owned()), KeyEpoch(1))
}
fn lic_signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[6u8; 32], KeyId("licensing-k".to_owned()), KeyEpoch(1))
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

fn seed(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:RC1','cass','decision','cass:RC1','Cass','v0','corps','2024-01-01', \
           'sha256:rc1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:RC1#0','cass:RC1',0,'corps','ctx','sha256:c','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:RC1#0','fp','{}'::vector,'m',1024);",
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
    let ctx = OutboxContext::new("rc", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope("core", table, op, scope_kind::DOCUMENT, "cass:RC1"),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), &to).unwrap();
        }
    }
}

/// Copy `artifact_dir` to a fresh tempdir, mutate its embedded manifest (optionally re-sealing with the
/// producer key), and return the tampered copy.
fn tampered(
    artifact_dir: &Path,
    reseal: bool,
    signer: &Ed25519Signer,
    mutate_manifest: impl FnOnce(&mut EmbeddedManifest),
) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("js-rc-tamper.")
        .tempdir()
        .unwrap();
    copy_dir(artifact_dir, dir.path());
    let path = artifact::manifest_path(dir.path());
    let signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut manifest = signed.payload.clone();
    mutate_manifest(&mut manifest);
    let out = if reseal {
        Signed::seal(manifest, signer).unwrap()
    } else {
        // Keep the OLD signature over the ORIGINAL bytes → signature no longer matches the mutation.
        Signed {
            payload: manifest,
            signature: signed.signature,
        }
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&out).unwrap()).unwrap();
    dir
}

fn code_of(result: Result<impl std::fmt::Debug, SyncError>) -> RejectCode {
    match result {
        Err(SyncError::Reject { code, .. }) => code,
        other => panic!("expected a Reject, got {other:?}"),
    }
}

#[test]
fn every_reject_code_is_produced_by_a_real_path() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let pkg = pkg_signer();
    let lic = lic_signer();
    let proot = tempfile::Builder::new()
        .prefix("js-rc-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed(&producer)?;

    // Baseline + two incrementals (1->2, 2->3), all Ed25519-signed.
    let base_art = tempfile::Builder::new()
        .prefix("js-rc-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(&producer, "core", base_art.path(), &pkg, &baseline_params()).expect("baseline");
    mutate(
        &producer,
        "UPDATE documents SET title='v1' WHERE document_id='cass:RC1';",
        "documents",
        EventKind::Upsert,
    )?;
    let inc1 = tempfile::Builder::new()
        .prefix("js-rc-i1.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc1.path(),
        &pkg,
        &incremental_params("r1"),
    )
    .expect("inc1")
    .expect("changes");
    mutate(
        &producer,
        "UPDATE documents SET title='v2' WHERE document_id='cass:RC1';",
        "documents",
        EventKind::Upsert,
    )?;
    let inc2 = tempfile::Builder::new()
        .prefix("js-rc-i2.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc2.path(),
        &pkg,
        &incremental_params("r2"),
    )
    .expect("inc2")
    .expect("changes");

    // Client with both trust anchors.
    let croot = tempfile::Builder::new()
        .prefix("js-rc-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    install_trust_anchor(&client, &pkg.trust_anchor(), PACKAGE_PURPOSE).expect("pkg anchor");
    install_trust_anchor(&client, &lic.trust_anchor(), LICENSE_PURPOSE).expect("lic anchor");
    let verifier = load_package_verifier(&client).expect("verifier");

    let mut codes: HashSet<RejectCode> = HashSet::new();

    // SignatureInvalid: tamper a field WITHOUT re-signing.
    let t = tampered(base_art.path(), false, &pkg, |m| {
        m.identity.builder_run_id = "x".to_owned()
    });
    codes.insert(code_of(apply_baseline(&client, t.path(), &verifier)));

    // BaselineRequired: apply_baseline on an INCREMENTAL artifact (kind mismatch).
    codes.insert(code_of(apply_baseline(&client, inc1.path(), &verifier)));

    // ClientTooOld: re-seal the baseline with an impossible minimum_client_version.
    let t = tampered(base_art.path(), true, &pkg, |m| {
        m.compatibility.minimum_client_version = Version::new(9, 9, 9)
    });
    codes.insert(code_of(apply_baseline(&client, t.path(), &verifier)));

    // SchemaAhead: re-seal with a schema_version beyond the client binary.
    let t = tampered(base_art.path(), true, &pkg, |m| {
        m.compatibility.schema_version += 1
    });
    codes.insert(code_of(apply_baseline(&client, t.path(), &verifier)));

    // ExtensionMissing: re-seal with an impossible required extension.
    let t = tampered(base_art.path(), true, &pkg, |m| {
        m.compatibility
            .requires_extensions
            .push(ExtensionRequirement {
                name: "does_not_exist".to_owned(),
                minimum_version: None,
            })
    });
    codes.insert(code_of(apply_baseline(&client, t.path(), &verifier)));

    // DigestMismatch: re-seal with a tampered aggregate artifact digest (signature valid, digest wrong).
    let t = tampered(base_art.path(), true, &pkg, |m| {
        m.integrity.artifact_sha256 = "sha256:tampered".to_owned()
    });
    codes.insert(code_of(apply_baseline(&client, t.path(), &verifier)));

    // MissingEntitlement: re-seal as a Subscription-tier package with no installed token.
    let t = tampered(base_art.path(), true, &pkg, |m| {
        m.entitlement.tier = "restricted".to_owned()
    });
    codes.insert(code_of(apply_baseline(&client, t.path(), &verifier)));

    // Now actually install the baseline so the incremental gates run.
    apply_baseline(&client, base_art.path(), &verifier).expect("apply real baseline");

    // SequenceGap: apply inc2 (from=2) while the cursor is still at 1.
    codes.insert(code_of(apply_incremental(&client, inc2.path(), &verifier)));

    // EmbeddingFingerprintMismatch / BuilderVersionMismatch: re-seal inc1 with a tampered precondition.
    let t = tampered(inc1.path(), true, &pkg, |m| {
        m.apply.preconditions.embedding_fingerprint = "WRONG".to_owned()
    });
    codes.insert(code_of(apply_incremental(&client, t.path(), &verifier)));
    let t = tampered(inc1.path(), true, &pkg, |m| {
        m.apply
            .preconditions
            .builder_versions
            .insert("chunker".to_owned(), "WRONG".to_owned());
    });
    codes.insert(code_of(apply_incremental(&client, t.path(), &verifier)));

    // WrongGeneration: an incremental whose `active_generation` precondition does not match the cursor
    // (a GENUINE cursor/generation conflict — passes signature + from-sequence + chain link, then the
    // precondition mismatch fires).
    let t = tampered(inc1.path(), true, &pkg, |m| {
        m.apply.preconditions.active_generation = Some("does-not-match".to_owned());
    });
    codes.insert(code_of(apply_incremental(&client, t.path(), &verifier)));

    // Apply/switch advisory-lock contention is a TRANSIENT, RETRYABLE signal (work/09 P5, codex): it
    // surfaces as `SyncError::LockBusy`, NOT a `WrongGeneration` reject — so the daemon backs off and
    // retries instead of mis-classifying it as a permanent cursor conflict. (Not a §6.3 reject code.)
    {
        let mut holder = postgres::Client::connect(&client.connection_string(), postgres::NoTls)
            .map_err(StorageError::PostgresClient)?;
        let mut held = holder.transaction().map_err(StorageError::PostgresClient)?;
        held.execute(
            "SELECT pg_advisory_xact_lock($1);",
            &[&APPLY_ADVISORY_LOCK_KEY],
        )
        .map_err(StorageError::PostgresClient)?;
        let busy = apply_incremental(&client, inc1.path(), &verifier);
        assert!(
            matches!(busy, Err(SyncError::LockBusy { .. })),
            "apply-lock contention must be LockBusy (retryable), got {busy:?}"
        );
        held.rollback().map_err(StorageError::PostgresClient)?;
    }

    // The closed §6.3 vocabulary is exhaustively exercised.
    let expected: HashSet<RejectCode> = RejectCode::all().into_iter().collect();
    assert_eq!(
        codes,
        expected,
        "every reject code must be produced by a real path; missing: {:?}",
        expected.difference(&codes).collect::<Vec<_>>()
    );
    Ok(())
}
