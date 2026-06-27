//! Filesystem publishing of signed artifacts + the per-corpus remote manifest (plan P9).
//!
//! The CODE fixes the distribution CONTRACT — a deterministic published layout of signed artifacts +
//! a signed remote manifest, written ATOMICALLY (stage under `.tmp`, then rename), so a reader never
//! sees a `manifest.json` that references a half-staged artifact. Real TLS/HTTP/CDN/object-store
//! hosting is the ops boundary (the per-corpus path layout makes the expected ACL shape obvious).
//!
//! Layout: `root/<corpus>/manifest.json` (the signed `RemoteManifest`) and
//! `root/<corpus>/packages/<package_id>/{manifest.json, payload/...}` (each signed artifact).

use std::path::{Path, PathBuf};

use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::signed::Signed;

use crate::error::BuildError;

/// The published directory for one package artifact (`root/<corpus>/packages/<package_id>`).
#[must_use]
pub fn published_package_dir(root: &Path, corpus: &str, package_id: &str) -> PathBuf {
    root.join(corpus).join("packages").join(package_id)
}

/// The published signed remote-manifest path (`root/<corpus>/manifest.json`).
#[must_use]
pub fn published_manifest_path(root: &Path, corpus: &str) -> PathBuf {
    root.join(corpus).join("manifest.json")
}

/// Publish one built artifact directory under the deterministic root. Published package ids are
/// IMMUTABLE (plan P9 r1 WARN): a package id maps to exactly one artifact. The live directory is NEVER
/// removed — staging into a sibling `.tmp` then renaming is atomic ONLY when the destination did not
/// exist. Re-publishing the SAME id with byte-identical content (same embedded `artifact_sha256`) is an
/// idempotent no-op; re-publishing with DIFFERENT content is an error (rebuild under a new id, then
/// repoint `manifest.json` — the only in-place replacement surface).
///
/// # Errors
/// [`BuildError`] if the id is already published with different content, or on an IO failure.
pub fn publish_package(
    root: &Path,
    corpus: &str,
    package_id: &str,
    artifact_dir: &Path,
) -> Result<PathBuf, BuildError> {
    let dest = published_package_dir(root, corpus, package_id);
    if dest.exists() {
        let existing = artifact_sha256(&dest)?;
        let incoming = artifact_sha256(artifact_dir)?;
        if existing == incoming {
            return Ok(dest); // idempotent re-publish of the same immutable package
        }
        return Err(BuildError::Storage(
            jurisearch_storage::runtime::StorageError::Generations {
                message: format!(
                    "package id `{package_id}` is already published with DIFFERENT content \
                     ({existing} != {incoming}); publish a new id and repoint manifest.json"
                ),
            },
        ));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let staging = with_tmp_suffix(&dest);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    copy_dir_recursive(artifact_dir, &staging)?;
    std::fs::rename(&staging, &dest)?; // dest did not exist → atomic create
    Ok(dest)
}

/// The published artifact's logical digest (its embedded manifest's `integrity.artifact_sha256`).
fn artifact_sha256(artifact_dir: &Path) -> Result<String, BuildError> {
    let bytes = std::fs::read(jurisearch_package::artifact::manifest_path(artifact_dir))?;
    let signed: jurisearch_package::signed::Signed<jurisearch_package::manifest::EmbeddedManifest> =
        serde_json::from_slice(&bytes)?;
    Ok(signed.payload.integrity.artifact_sha256)
}

/// Write the signed remote manifest ATOMICALLY (stage to `manifest.json.tmp`, then rename). The caller
/// MUST have published every artifact the manifest references first.
///
/// # Errors
/// [`BuildError`] on a serialisation or IO failure.
pub fn publish_remote_manifest(
    root: &Path,
    corpus: &str,
    manifest: &Signed<RemoteManifest>,
) -> Result<PathBuf, BuildError> {
    let dest = published_manifest_path(root, corpus);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(manifest)?;
    let staging = with_tmp_suffix(&dest);
    std::fs::write(&staging, &bytes)?;
    std::fs::rename(&staging, &dest)?;
    Ok(dest)
}

fn with_tmp_suffix(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Recursively copy `src` into `dest` (files + subdirectories). `dest` must not yet exist.
fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), BuildError> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// The total byte size of an unpacked artifact directory (`manifest.json` + every payload file) — the
/// `uncompressed_size_bytes` the remote manifest advertises (plan P9: no fake compression ratio, so the
/// compressed size equals this until a real compressed transport artifact is introduced).
///
/// # Errors
/// [`BuildError`] on an IO failure.
pub fn artifact_dir_size_bytes(artifact_dir: &Path) -> Result<u64, BuildError> {
    let mut total = 0u64;
    let mut stack = vec![artifact_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}
