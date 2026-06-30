//! Read-only verification of a PUBLISHED root (plan P9 r1 BLOCKER): the producer/operator QA gate that
//! validates the actual `manifest.json` clients poll — with a PUBLIC verifier, never the private signing
//! key. It checks the signed remote manifest's signature + corpus, then for every referenced artifact
//! (the active baseline + each retained incremental): the artifact exists, its embedded signature
//! matches the remote entry's signature AND verifies with the public verifier, and its
//! `integrity.artifact_sha256` equals the remote entry's `sha256`. This catches a deleted, stale,
//! corrupted, or wrong-corpus published manifest that the catalog-only build path would miss.

use std::path::{Component, Path, PathBuf};

use jurisearch_package::Verifier;
use jurisearch_package::crypto::Signature;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::remote::RemoteManifest;
use jurisearch_package::signed::Signed;

use jurisearch_storage::runtime::StorageError;

use crate::error::BuildError;
use crate::publish::published_manifest_path;

/// A summary of a verified published root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedVerifyReport {
    pub corpus: String,
    pub head_sequence: u64,
    pub packages: usize,
    /// Total artifacts checked (active baseline + retained incrementals).
    pub artifacts_checked: usize,
}

/// Verify the published manifest + every artifact it references under `root`, with the PUBLIC
/// `verifier` and the `uri_base` the producer published with.
///
/// # Errors
/// [`BuildError`] if the manifest is missing/unsigned/wrong-corpus, an artifact is missing, a remote
/// entry's `sha256`/signature disagrees with the published artifact, or on an IO failure.
pub fn verify_published_root(
    root: &Path,
    corpus: &str,
    uri_base: &str,
    verifier: &dyn Verifier,
) -> Result<PublishedVerifyReport, BuildError> {
    let manifest_path = published_manifest_path(root, corpus);
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        fail(format!(
            "published manifest `{}` is missing/unreadable ({error})",
            manifest_path.display()
        ))
    })?;
    let signed: Signed<RemoteManifest> = serde_json::from_slice(&bytes)?;
    verify_signed_remote_manifest(root, corpus, uri_base, &signed, verifier)
}

/// Verify an IN-MEMORY `Signed<RemoteManifest>` plus every artifact it references under `root`, WITHOUT
/// re-reading `manifest.json` from disk. Backs both [`verify_published_root`]'s post-publish readback and
/// the producer's PRE-rename gate (verify the manifest before it ever becomes client-visible — plan P9
/// r1+ BLOCKER): signature + corpus, then for the active baseline + each retained incremental the
/// artifact exists, its embedded signature matches the remote entry AND verifies with `verifier`, and its
/// `integrity.artifact_sha256` equals the remote `sha256`.
///
/// # Errors
/// [`BuildError`] if the signature/corpus is wrong, an artifact is missing, a remote entry's
/// `sha256`/signature disagrees with the published artifact, or on an IO failure.
pub fn verify_signed_remote_manifest(
    root: &Path,
    corpus: &str,
    uri_base: &str,
    signed: &Signed<RemoteManifest>,
    verifier: &dyn Verifier,
) -> Result<PublishedVerifyReport, BuildError> {
    signed.verify(verifier).map_err(|error| {
        fail(format!(
            "published remote manifest signature invalid: {error}"
        ))
    })?;
    if signed.payload.corpus.as_str() != corpus {
        return Err(fail(format!(
            "published manifest is for corpus `{}`, not `{corpus}`",
            signed.payload.corpus.as_str()
        )));
    }

    let mut checked = 0;
    verify_artifact(
        root,
        uri_base,
        &signed.payload.active_baseline.artifact_uri,
        &signed.payload.active_baseline.sha256,
        &signed.payload.active_baseline.signature,
        verifier,
    )?;
    checked += 1;
    for entry in &signed.payload.packages {
        verify_artifact(
            root,
            uri_base,
            &entry.artifact_uri,
            &entry.sha256,
            &entry.signature,
            verifier,
        )?;
        checked += 1;
    }

    Ok(PublishedVerifyReport {
        corpus: corpus.to_owned(),
        head_sequence: signed.payload.head_sequence.get(),
        packages: signed.payload.packages.len(),
        artifacts_checked: checked,
    })
}

fn verify_artifact(
    root: &Path,
    uri_base: &str,
    artifact_uri: &str,
    expected_sha256: &str,
    expected_signature: &Signature,
    verifier: &dyn Verifier,
) -> Result<(), BuildError> {
    let dir = resolve_uri(root, uri_base, artifact_uri)?;
    let path = jurisearch_package::artifact::manifest_path(&dir);
    let bytes = std::fs::read(&path).map_err(|error| {
        fail(format!(
            "referenced artifact `{artifact_uri}` is missing ({error})"
        ))
    })?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes)?;
    // The remote listing's signature must BE the artifact's own embedded signature...
    if signed.signature != *expected_signature {
        return Err(fail(format!(
            "artifact `{artifact_uri}` signature differs from the remote-manifest entry"
        )));
    }
    // ...and that signature must verify with the public trust anchor.
    signed.verify(verifier).map_err(|error| {
        fail(format!(
            "artifact `{artifact_uri}` signature invalid: {error}"
        ))
    })?;
    // ...and the remote `sha256` must equal the artifact's own aggregate digest.
    if signed.payload.integrity.artifact_sha256 != expected_sha256 {
        return Err(fail(format!(
            "artifact `{artifact_uri}` digest {} != remote-manifest sha256 {expected_sha256}",
            signed.payload.integrity.artifact_sha256
        )));
    }
    Ok(())
}

/// Resolve an `artifact_uri` to a local dir under `root`, rejecting a non-base URI or `..`/absolute
/// traversal (mirrors the client's `DirectoryCatchupSource`).
fn resolve_uri(root: &Path, uri_base: &str, artifact_uri: &str) -> Result<PathBuf, BuildError> {
    let relative = artifact_uri.strip_prefix(uri_base).ok_or_else(|| {
        fail(format!(
            "artifact_uri `{artifact_uri}` does not carry the base"
        ))
    })?;
    let relative = Path::new(relative);
    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(fail(format!(
            "artifact_uri `{artifact_uri}` is not a safe relative path"
        )));
    }
    Ok(root.join(relative))
}

fn fail(message: String) -> BuildError {
    BuildError::Storage(StorageError::Generations { message })
}
