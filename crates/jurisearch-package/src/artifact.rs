//! On-disk artifact layout (plan P3): the agreed file names inside a package directory, so the
//! producer (`jurisearch-package-build`) and the consumer (`jurisearch-syncd`) never disagree on where
//! the signed manifest and per-table payload files live. Pure path helpers — **no I/O** (this crate is
//! a leaf; the two sides own the actual read/write).
//!
//! Layout of a baseline artifact directory:
//! ```text
//! <artifact_dir>/
//!   manifest.json            # serde JSON of Signed<EmbeddedManifest>
//!   payload/
//!     <table>.copybin        # raw `COPY ... (FORMAT binary)` bytes for that replicated table
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The signed embedded-manifest file name inside an artifact directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The sub-directory holding the per-table payload files.
pub const PAYLOAD_DIR: &str = "payload";

/// The `Signed<EmbeddedManifest>` JSON path inside `artifact_dir`.
#[must_use]
pub fn manifest_path(artifact_dir: &Path) -> PathBuf {
    artifact_dir.join(MANIFEST_FILE)
}

/// The payload sub-directory inside `artifact_dir`.
#[must_use]
pub fn payload_dir(artifact_dir: &Path) -> PathBuf {
    artifact_dir.join(PAYLOAD_DIR)
}

/// The per-table payload file name (e.g. `documents.copybin`) — also the key used in the manifest's
/// `per_file_digests` map, so a digest is bound to exactly one file.
#[must_use]
pub fn payload_file_name(table: &str) -> String {
    format!("{table}.copybin")
}

/// The full path of `table`'s payload file inside `artifact_dir`.
#[must_use]
pub fn payload_file_path(artifact_dir: &Path, table: &str) -> PathBuf {
    payload_dir(artifact_dir).join(payload_file_name(table))
}

/// The aggregate package/payload digest (plan P3): `sha256` over the per-file payload digests
/// concatenated by `|` in `apply_order`. This is the SINGLE definition both the producer (manifest
/// `integrity.artifact_sha256` / `uncompressed_payload_digest`) and the consumer (post-load
/// verification) compute, so an internally-inconsistent manifest — one whose aggregate digest no longer
/// matches its own per-file digests — is caught instead of trusted. Files absent from `per_file_digests`
/// are skipped (a table can have no payload file); the order is the apply order, not map order.
#[must_use]
pub fn aggregate_payload_digest<S: AsRef<str>>(
    per_file_digests: &BTreeMap<String, String>,
    apply_order: &[S],
) -> String {
    let joined = apply_order
        .iter()
        .filter_map(|table| per_file_digests.get(&payload_file_name(table.as_ref())))
        .cloned()
        .collect::<Vec<_>>()
        .join("|");
    crate::canonical::digest_bytes(joined.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_digest_is_order_sensitive_and_stable() {
        let mut digests = BTreeMap::new();
        digests.insert("documents.copybin".to_owned(), "sha256:a".to_owned());
        digests.insert("chunks.copybin".to_owned(), "sha256:b".to_owned());
        let order_ab = vec!["documents".to_owned(), "chunks".to_owned()];
        let order_ba = vec!["chunks".to_owned(), "documents".to_owned()];
        assert_eq!(
            aggregate_payload_digest(&digests, &order_ab),
            aggregate_payload_digest(&digests, &order_ab),
            "deterministic"
        );
        assert_ne!(
            aggregate_payload_digest(&digests, &order_ab),
            aggregate_payload_digest(&digests, &order_ba),
            "order-sensitive"
        );
    }

    #[test]
    fn layout_paths_are_stable_and_composed() {
        let dir = Path::new("/tmp/pkg/core-1-1");
        assert_eq!(
            manifest_path(dir),
            Path::new("/tmp/pkg/core-1-1/manifest.json")
        );
        assert_eq!(payload_file_name("documents"), "documents.copybin");
        assert_eq!(
            payload_file_path(dir, "chunk_embeddings"),
            Path::new("/tmp/pkg/core-1-1/payload/chunk_embeddings.copybin")
        );
    }
}
