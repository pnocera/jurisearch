//! On-disk artifact layout (plan P3/P4): the agreed file names inside a package directory, so the
//! producer (`jurisearch-package-build`) and the consumer (`jurisearch-syncd`) never disagree on where
//! the signed manifest and per-file payload files live. Pure path helpers — **no I/O** (this crate is
//! a leaf; the two sides own the actual read/write).
//!
//! Layout of an artifact directory:
//! ```text
//! <artifact_dir>/
//!   manifest.json                       # serde JSON of Signed<EmbeddedManifest>
//!   payload/
//!     <table>.copybin                    # baseline: raw COPY (FORMAT binary) bytes per table
//!     <table_or_group>.<op>.jsonl        # incremental: JSONL upsert rows / delete keys / replace_sets
//! ```
//!
//! Every payload file's name is recorded in [`crate::manifest::embedded::PayloadFile::file_name`] and is
//! the key in the manifest's `integrity.per_file_digests` map, so a digest is bound to exactly one file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::event::EventKind;

/// The signed embedded-manifest file name inside an artifact directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The sub-directory holding the per-file payload files.
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

/// The baseline per-table payload file name (e.g. `documents.copybin`).
#[must_use]
pub fn baseline_file_name(table: &str) -> String {
    format!("{table}.copybin")
}

/// The incremental per-(table-or-group, op) JSONL file name (e.g. `documents.upsert.jsonl`,
/// `chunks_with_embeddings.replace_set.jsonl`).
#[must_use]
pub fn incremental_file_name(table_or_group: &str, op: EventKind) -> String {
    format!("{table_or_group}.{}.jsonl", op.as_str())
}

/// The full path of a payload file (named by its recorded `file_name`) inside `artifact_dir`.
#[must_use]
pub fn payload_file_path(artifact_dir: &Path, file_name: &str) -> PathBuf {
    payload_dir(artifact_dir).join(file_name)
}

/// The aggregate package/payload digest (plan P3/P4): `sha256` over the per-file `name=digest` pairs in
/// **file-name order** (the `BTreeMap` is already key-ordered). This is the SINGLE definition both the
/// producer (manifest `integrity.artifact_sha256` / `uncompressed_payload_digest`) and the consumer
/// (post-load verification over the files it actually read) compute, so an internally-inconsistent
/// manifest — one whose aggregate no longer matches its own per-file digests — is caught. Binding the
/// file NAME (not just the digest) means a renamed/added/removed file changes the aggregate.
#[must_use]
pub fn aggregate_payload_digest(per_file_digests: &BTreeMap<String, String>) -> String {
    let joined = per_file_digests
        .iter()
        .map(|(name, digest)| format!("{name}={digest}"))
        .collect::<Vec<_>>()
        .join("|");
    crate::canonical::digest_bytes(joined.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_digest_binds_names_and_is_stable() {
        let mut digests = BTreeMap::new();
        digests.insert("documents.copybin".to_owned(), "sha256:a".to_owned());
        digests.insert("chunks.copybin".to_owned(), "sha256:b".to_owned());
        assert_eq!(
            aggregate_payload_digest(&digests),
            aggregate_payload_digest(&digests),
            "deterministic"
        );
        // An added file (extra name) changes the aggregate.
        let mut more = digests.clone();
        more.insert("zone_units.copybin".to_owned(), "sha256:c".to_owned());
        assert_ne!(
            aggregate_payload_digest(&digests),
            aggregate_payload_digest(&more)
        );
        // A renamed file (same digest, different name) changes the aggregate.
        let mut renamed = BTreeMap::new();
        renamed.insert("documents.copybin".to_owned(), "sha256:a".to_owned());
        renamed.insert("chunkz.copybin".to_owned(), "sha256:b".to_owned());
        assert_ne!(
            aggregate_payload_digest(&digests),
            aggregate_payload_digest(&renamed)
        );
    }

    #[test]
    fn layout_paths_are_stable_and_composed() {
        let dir = Path::new("/tmp/pkg/core-1-1");
        assert_eq!(
            manifest_path(dir),
            Path::new("/tmp/pkg/core-1-1/manifest.json")
        );
        assert_eq!(baseline_file_name("documents"), "documents.copybin");
        assert_eq!(
            incremental_file_name("chunks_with_embeddings", EventKind::ReplaceSet),
            "chunks_with_embeddings.replace_set.jsonl"
        );
        assert_eq!(
            payload_file_path(dir, "chunk_embeddings.copybin"),
            Path::new("/tmp/pkg/core-1-1/payload/chunk_embeddings.copybin")
        );
    }
}
