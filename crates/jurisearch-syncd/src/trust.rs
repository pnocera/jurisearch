//! Client-side trust wiring (plan P6): build the Ed25519 package verifier from the client's installed
//! trust anchors, enforce entitlement as an apply precondition, and install signature-verified license
//! tokens. The production binary uses [`load_package_verifier`] — NEVER `AcceptAllVerifier`.

use jurisearch_package::RejectCode;
use jurisearch_package::crypto::{Ed25519Verifier, TrustAnchor};
use jurisearch_package::license::{LicenseToken, tier_is_open};
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;
use jurisearch_storage::backend::WriterConnection;
use jurisearch_storage::trust::{
    LICENSE_PURPOSE, PACKAGE_PURPOSE, install_license_token, license_token_blobs,
    load_trust_anchors, token_not_after_in_future,
};

use crate::error::SyncError;

/// Install a producer verifying key the client trusts, for `purpose` (`"package"` or `"license"`) —
/// the `trust install-anchor` bootstrap (plan P9). Without this the production client cannot establish
/// trust except via tests/manual SQL.
///
/// # Errors
/// [`SyncError`] on a DB error.
pub fn install_trust_anchor(
    client: &dyn WriterConnection,
    anchor: &TrustAnchor,
    purpose: &str,
) -> Result<(), SyncError> {
    let mut db = client.writer_client()?;
    jurisearch_storage::trust::install_trust_anchor(&mut db, anchor, purpose)?;
    Ok(())
}

/// Build the PACKAGE-signature verifier from the client's installed trust anchors. This is NEVER an
/// accept-all fallback: an empty trust store yields a verifier that rejects every signature
/// (`UnknownKey`), and a malformed anchor is a hard `SignatureInvalid` (the trust root is broken).
///
/// # Errors
/// [`SyncError`] if the trust store is unreadable or carries malformed key material.
pub fn load_package_verifier(client: &dyn WriterConnection) -> Result<Ed25519Verifier, SyncError> {
    build_verifier(client, PACKAGE_PURPOSE)
}

fn build_verifier(
    client: &dyn WriterConnection,
    purpose: &str,
) -> Result<Ed25519Verifier, SyncError> {
    let mut db = client.writer_client()?;
    let anchors = load_trust_anchors(&mut db, purpose)?;
    Ed25519Verifier::from_anchors(&anchors).map_err(|error| {
        SyncError::reject(
            RejectCode::SignatureInvalid,
            format!("trust store (purpose `{purpose}`) is unusable: {error}"),
        )
    })
}

/// The §11.3 entitlement precondition (an apply precondition, not URL hiding). An `open`-tier package
/// needs no token; otherwise a valid installed [`LicenseToken`] — signature re-verified against a
/// license-purpose anchor, unexpired (DB-clock), covering `(corpus, tier)` at `license_epoch` — must
/// exist, else `MissingEntitlement`. Runs BEFORE any row mutation.
///
/// # Errors
/// [`SyncError`] with [`RejectCode::MissingEntitlement`] when no valid token covers the package, or a
/// storage error.
pub fn check_entitlement(
    client: &dyn WriterConnection,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    let ent = &manifest.entitlement;
    let tier = ent.tier.as_str();
    if tier_is_open(tier) {
        return Ok(()); // open packages apply given the bytes
    }
    let corpus = ent.entitlement_corpus.as_str();
    let license_verifier = build_verifier(client, LICENSE_PURPOSE)?;
    let mut db = client.writer_client()?;
    // EVERY trust predicate is derived from the SIGNED payload, never the denormalized columns (plan P6
    // r1 BLOCKER). The `corpus` column is only a cheap index; we then re-verify the signature, confirm
    // corpus/tier/epoch coverage on the payload, AND check the payload's own `not_after` against the DB
    // clock — so tampering the local `not_after` column cannot resurrect an expired signed token.
    let blobs = license_token_blobs(&mut db, corpus)?;
    for blob in &blobs {
        let Ok(signed) = serde_json::from_str::<Signed<LicenseToken>>(blob) else {
            continue;
        };
        if signed.verify(&license_verifier).is_err() {
            continue;
        }
        if !signed.payload.covers(corpus, tier, ent.license_epoch) {
            continue;
        }
        if token_not_after_in_future(&mut db, signed.payload.not_after.as_deref())? {
            return Ok(());
        }
    }
    Err(SyncError::reject(
        RejectCode::MissingEntitlement,
        format!(
            "no valid installed license token covers corpus `{corpus}` tier `{tier}` at epoch {}",
            ent.license_epoch
        ),
    ))
}

/// Install a license token (the `subscribe <corpus>` path), after verifying its signature against a
/// license-purpose trust anchor. An invalid signature is `SignatureInvalid` (distinct from
/// `MissingEntitlement` at apply time).
///
/// # Errors
/// [`SyncError`] with [`RejectCode::SignatureInvalid`] on a bad token signature, or a storage error.
pub fn install_verified_license_token(
    client: &dyn WriterConnection,
    signed_token_json: &str,
) -> Result<(), SyncError> {
    let license_verifier = build_verifier(client, LICENSE_PURPOSE)?;
    let signed: Signed<LicenseToken> = serde_json::from_str(signed_token_json)?;
    signed.verify(&license_verifier).map_err(|error| {
        SyncError::reject(
            RejectCode::SignatureInvalid,
            format!("license token signature invalid: {error}"),
        )
    })?;
    let mut db = client.writer_client()?;
    install_license_token(&mut db, &signed.payload, signed_token_json)?;
    Ok(())
}
