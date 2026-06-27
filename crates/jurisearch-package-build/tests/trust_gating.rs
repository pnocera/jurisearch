//! P6 acceptance: the real Ed25519 trust boundary + entitlement gate, end to end.
//!
//! - a baseline SIGNED by the producer's Ed25519 key applies on a client that has installed the
//!   matching trust anchor; a tampered manifest is rejected with `signature_invalid` and nothing is
//!   installed (one trust path — the same `Signed::verify` mechanism guards media and network);
//! - a `Subscription`-tier package is refused with `missing_entitlement` until a valid, signature-checked
//!   license token covering its corpus/tier/epoch is installed, after which it applies; a token for the
//!   wrong corpus does NOT satisfy the gate.

use std::collections::BTreeMap;

use jurisearch_package::artifact;
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{Ed25519Signer, KeyEpoch, KeyId};
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;
use jurisearch_package::{Corpus, LicenseToken};
use jurisearch_package_build::{BaselineParams, build_baseline};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_storage::trust::{LICENSE_PURPOSE, PACKAGE_PURPOSE, install_trust_anchor};
use jurisearch_syncd::{
    apply_baseline, corpus_status, install_verified_license_token, load_package_verifier,
};

fn params() -> BaselineParams {
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

fn package_signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(
        &[11u8; 32],
        KeyId("producer-pkg-k1".to_owned()),
        KeyEpoch(1),
    )
}

fn license_signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[22u8; 32], KeyId("licensing-k1".to_owned()), KeyEpoch(1))
}

fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:T1','cass','decision','cass:T1','Cass','Arret','corps','2024-01-01', \
           'sha256:t1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:T1#0','cass:T1',0,'corps','ctx corps','sha256:c','c1','fp');",
    )?;
    let vector = format!(
        "[{}]",
        (0..1024).map(|_| "0.01").collect::<Vec<_>>().join(",")
    );
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:T1#0','fp','{vector}'::vector,'m',1024);"
    ))?;
    Ok(())
}

/// Start a producer, seed it, and build a baseline signed with `signer` into a fresh artifact tempdir.
/// The producer DB is only needed during the build (the artifact is self-contained), so it is dropped
/// here; only the artifact dir is returned (kept alive by the caller).
fn build_signed_baseline(
    pg_config: &PgConfig,
    signer: &Ed25519Signer,
) -> Result<tempfile::TempDir, StorageError> {
    let proot = tempfile::Builder::new()
        .prefix("js-trust-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let art = tempfile::Builder::new()
        .prefix("js-trust-a.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(&producer, "core", art.path(), signer, &params()).expect("signed baseline");
    Ok(art)
}

#[test]
fn real_ed25519_baseline_applies_and_a_tampered_manifest_is_rejected() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let signer = package_signer();
    let art = build_signed_baseline(&pg_config, &signer)?;

    // Client installs the producer's package trust anchor, then builds its verifier from the store.
    let croot = tempfile::Builder::new()
        .prefix("js-trust-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &signer.trust_anchor(), PACKAGE_PURPOSE)?;
    }
    let verifier = load_package_verifier(&client).expect("verifier from trust store");
    assert_eq!(verifier.anchor_count(), 1);

    // A real, untampered signed baseline applies.
    apply_baseline(&client, art.path(), &verifier).expect("genuine signed baseline applies");
    assert_eq!(corpus_status(&client)?[0].sequence, 1);

    // Tamper the signed manifest payload WITHOUT re-signing → the Ed25519 signature no longer matches
    // its canonical bytes → `signature_invalid`. Use a fresh client so nothing is pre-installed.
    let manifest_path = artifact::manifest_path(art.path());
    let mut signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(StorageError::Io)?)
            .expect("manifest json");
    signed.payload.identity.builder_run_id = "tampered".to_owned();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&signed).expect("serialize"),
    )
    .map_err(StorageError::Io)?;

    let croot2 = tempfile::Builder::new()
        .prefix("js-trust-c2.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client2 = ManagedPostgres::start_durable(PgConfig::discover().expect("pg"), croot2.path())?;
    client2.run_migrations()?;
    {
        let mut db = client2.client()?;
        install_trust_anchor(&mut db, &signer.trust_anchor(), PACKAGE_PURPOSE)?;
    }
    let verifier2 = load_package_verifier(&client2).expect("verifier");
    let err = apply_baseline(&client2, art.path(), &verifier2)
        .expect_err("a tampered manifest must be rejected");
    assert!(
        err.to_string().to_lowercase().contains("signature"),
        "rejection cites the signature: {err}"
    );
    assert!(
        corpus_status(&client2)?.is_empty(),
        "a signature-rejected apply installs nothing"
    );
    Ok(())
}

#[test]
fn subscription_package_requires_a_valid_license_token() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let pkg_signer = package_signer();
    let lic_signer = license_signer();
    let art = build_signed_baseline(&pg_config, &pkg_signer)?;

    // Re-seal the baseline manifest as a SUBSCRIPTION package for corpus `core`, tier `restricted`,
    // license_epoch 2 (signed by the producer's package key, which the client trusts).
    let manifest_path = artifact::manifest_path(art.path());
    let signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(StorageError::Io)?)
            .expect("manifest json");
    let mut manifest = signed.payload;
    manifest.entitlement.tier = "restricted".to_owned();
    manifest.entitlement.license_epoch = 2;
    let resealed = Signed::seal(manifest, &pkg_signer).expect("reseal");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&resealed).expect("serialize"),
    )
    .map_err(StorageError::Io)?;

    // Client installs BOTH the package anchor and the licensing anchor.
    let croot = tempfile::Builder::new()
        .prefix("js-ent-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &pkg_signer.trust_anchor(), PACKAGE_PURPOSE)?;
        install_trust_anchor(&mut db, &lic_signer.trust_anchor(), LICENSE_PURPOSE)?;
    }
    let verifier = load_package_verifier(&client).expect("verifier");

    // Without any token → missing_entitlement, nothing installed.
    let err = apply_baseline(&client, art.path(), &verifier)
        .expect_err("a subscription package without a token must be refused");
    assert!(
        err.to_string().to_lowercase().contains("entitlement"),
        "rejection cites entitlement: {err}"
    );
    assert!(corpus_status(&client)?.is_empty());

    // A token for the WRONG corpus does not satisfy the gate (genuinely the entitlement gate, before
    // any payload/cursor work — assert the code AND that nothing was installed).
    let wrong = sign_token(&lic_signer, "inpi", "restricted", 2, None);
    install_verified_license_token(&client, &wrong).expect("install wrong-corpus token");
    let err = apply_baseline(&client, art.path(), &verifier)
        .expect_err("a token for another corpus does not entitle this package");
    assert!(
        err.to_string().to_lowercase().contains("entitlement"),
        "wrong-corpus rejection cites entitlement: {err}"
    );
    assert!(corpus_status(&client)?.is_empty());

    // A valid token covering core/restricted at epoch >= 2 → applies.
    let good = sign_token(&lic_signer, "core", "restricted", 2, None);
    install_verified_license_token(&client, &good).expect("install valid token");
    apply_baseline(&client, art.path(), &verifier).expect("entitled package applies");
    assert_eq!(corpus_status(&client)?[0].sequence, 1);
    Ok(())
}

#[test]
fn an_expired_signed_token_cannot_be_resurrected_by_tampering_the_local_column()
-> Result<(), StorageError> {
    // P6 r1 BLOCKER regression: expiry is enforced from the SIGNED payload, not the denormalized
    // `license_token.not_after` column. Install an EXPIRED signed token, then tamper ONLY the local
    // column to NULL — the package must STILL be refused, because the payload's `not_after` is checked
    // against the DB clock and the signature still covers the (expired) payload.
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let pkg_signer = package_signer();
    let lic_signer = license_signer();
    let art = build_signed_baseline(&pg_config, &pkg_signer)?;

    let manifest_path = artifact::manifest_path(art.path());
    let signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(StorageError::Io)?)
            .expect("manifest json");
    let mut manifest = signed.payload;
    manifest.entitlement.tier = "restricted".to_owned();
    manifest.entitlement.license_epoch = 2;
    let resealed = Signed::seal(manifest, &pkg_signer).expect("reseal");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&resealed).expect("serialize"),
    )
    .map_err(StorageError::Io)?;

    let croot = tempfile::Builder::new()
        .prefix("js-ent-exp.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &pkg_signer.trust_anchor(), PACKAGE_PURPOSE)?;
        install_trust_anchor(&mut db, &lic_signer.trust_anchor(), LICENSE_PURPOSE)?;
    }
    let verifier = load_package_verifier(&client).expect("verifier");

    // Install an EXPIRED but correctly-signed token.
    let expired = sign_token(
        &lic_signer,
        "core",
        "restricted",
        2,
        Some("2020-01-01T00:00:00Z"),
    );
    install_verified_license_token(&client, &expired).expect("install expired token");
    assert!(
        apply_baseline(&client, art.path(), &verifier).is_err(),
        "an expired signed token does not entitle the package"
    );

    // Tamper ONLY the local column to remove the expiry; the signed payload is untouched.
    client.execute_sql(
        "UPDATE jurisearch_control.license_token SET not_after = NULL WHERE corpus = 'core';",
    )?;
    let err = apply_baseline(&client, art.path(), &verifier)
        .expect_err("clearing the local not_after column must not resurrect the expired token");
    assert!(
        err.to_string().to_lowercase().contains("entitlement"),
        "still refused on the SIGNED payload expiry: {err}"
    );
    assert!(corpus_status(&client)?.is_empty());
    Ok(())
}

#[test]
fn an_unsigned_license_token_is_refused_at_install() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let lic_signer = license_signer();
    let croot = tempfile::Builder::new()
        .prefix("js-ent-bad.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    {
        let mut db = client.client()?;
        install_trust_anchor(&mut db, &lic_signer.trust_anchor(), LICENSE_PURPOSE)?;
    }
    // A token signed by a DIFFERENT (untrusted) licensing key must be refused at install.
    let impostor =
        Ed25519Signer::from_seed(&[99u8; 32], KeyId("licensing-k1".to_owned()), KeyEpoch(1));
    let forged = sign_token(&impostor, "core", "restricted", 2, None);
    let err = install_verified_license_token(&client, &forged)
        .expect_err("a token from an untrusted key must be refused");
    assert!(err.to_string().to_lowercase().contains("signature"));
    Ok(())
}

/// Build a `Signed<LicenseToken>` JSON string signed by `signer`.
fn sign_token(
    signer: &Ed25519Signer,
    corpus: &str,
    tier: &str,
    epoch: u32,
    not_after: Option<&str>,
) -> String {
    let token = LicenseToken {
        entitlement_corpus: Corpus::new(corpus).unwrap(),
        tier: tier.to_owned(),
        license_epoch: epoch,
        audience: None,
        not_after: not_after.map(ToOwned::to_owned),
    };
    let signed = Signed::seal(token, signer).expect("seal token");
    serde_json::to_string(&signed).expect("serialize token")
}
