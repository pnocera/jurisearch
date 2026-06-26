//! The per-package embedded manifest — self-sufficient, travels inside the artifact (design §6.2.2).
//!
//! A client must never have to trust only the remote listing once it holds an artifact (ISP §7), so
//! every field needed to *verify* and *apply* a package is embedded here and signed (wrap in
//! [`crate::signed::Signed`]). Organised into the §6.2.2 field groups.

use crate::compat::Version;
use crate::corpus::Corpus;
use crate::event::ReplaceSetGroup;
use crate::package_kind::PackageKind;
use crate::sequence::PackageSequence;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The complete embedded manifest body (design §6.2.2). Wrap in [`crate::signed::Signed`] to sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddedManifest {
    pub identity: Identity,
    pub compatibility: Compatibility,
    pub entitlement: Entitlement,
    pub integrity: Integrity,
    pub apply: ApplyContract,
    pub payload: PayloadLayout,
}

/// Identity & ordering (design §6.2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub package_format_version: u32,
    pub package_id: String,
    pub corpus: Corpus,
    pub package_kind: PackageKind,
    pub from_sequence: PackageSequence,
    pub to_sequence: PackageSequence,
    /// Chain link to the previous package (absent for the first baseline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_package_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_package_sha256: Option<String>,
    pub baseline_id: String,
    pub generation: String,
    pub created_at: String,
    pub builder_run_id: String,
}

/// Compatibility gates (design §6.2.2). The default logical path leaves `postgres_major_min/max`
/// absent (advisory only) — they appear **only** for a physical-format variant (§9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compatibility {
    pub minimum_client_version: Version,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_client_version: Option<Version>,
    pub schema_version: i32,
    /// Digest over the bundled schema-migration SQL (so the client knows the DDL it carries).
    pub schema_migration_bundle_digest: String,
    pub requires_extensions: Vec<ExtensionRequirement>,
    pub embedding_fingerprint: String,
    pub embedding_model: String,
    pub embedding_dimension: u32,
    pub embedding_normalize: bool,
    pub builder_versions: BTreeMap<String, String>,
    /// Present only for the constrained physical-format variant (§9.3); absent on the default path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_major_min: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgres_major_max: Option<u32>,
}

/// One required extension and the version constraint, if known (design §6.2.2 `requires_extensions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRequirement {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
}

/// Entitlement (design §6.2.2). Verified independently of the remote listing (§11.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlement {
    pub entitlement_corpus: Corpus,
    pub tier: String,
    pub license_epoch: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// So the client can explain "not subscribed to corpus X" rather than a generic integrity error.
    pub entitlement_policy_digest: String,
}

/// Integrity & signing (design §6.2.2). The manifest's own signature lives in the
/// [`crate::signed::Signed`] wrapper; these are the artifact/payload digests the §11.1 chain checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Integrity {
    pub artifact_sha256: String,
    pub uncompressed_payload_digest: String,
    /// One digest per payload file (table/change file), checked before applying that file (§11.1).
    pub per_file_digests: BTreeMap<String, String>,
    pub canonicalisation_algorithm: String,
    pub signature_algorithm: String,
    /// Reserved for a future supply-chain transparency log (§6.2.2 "optional transparency-log index").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transparency_log_index: Option<u64>,
}

/// Apply contract (design §6.2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyContract {
    /// The sequence the client must be at for this package to apply (`from_sequence - 1`, §7.3).
    pub expected_client_from_sequence: PackageSequence,
    pub result_sequence: PackageSequence,
    pub requires_empty_generation: bool,
    pub schema_ops_digest: String,
    /// Counts by table × op kind (observability + a cheap shape check).
    pub operations: Vec<OperationCount>,
    pub replace_scopes: Vec<ReplaceScopeCount>,
    pub preconditions: Preconditions,
    pub postconditions: Postconditions,
    pub index_build: IndexBuildContract,
    /// `package_id` + digest — a re-applied committed package is skipped via the cursor (§7.3).
    pub idempotency_key: String,
    pub rollback_policy: RollbackPolicy,
}

/// A `(table, op, count)` summary entry (design §6.2.2 `operations`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationCount {
    pub table: String,
    pub op: crate::event::EventKind,
    pub count: u64,
}

/// A `replace_set` scope count, optionally with a scope digest (design §6.2.2 `replace_scopes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceScopeCount {
    pub table_group: ReplaceSetGroup,
    pub scope_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes_digest: Option<String>,
}

/// Preconditions checked before apply (design §6.2.2, §7.3 step 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preconditions {
    pub schema_version: i32,
    pub embedding_fingerprint: String,
    pub builder_versions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_baseline_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_generation: Option<String>,
}

/// Postconditions checked before the cursor advances (design §6.2.2, §11.1 step 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Postconditions {
    /// Expected row counts per table after apply.
    pub row_counts: BTreeMap<String, u64>,
    /// Deterministic per-table/set digests (the §5.4 QA backstop, used as a postcondition).
    pub table_digests: BTreeMap<String, String>,
}

/// The index-build contract (design §6.2.2 `index_build`, §9.3). Declares which §7.3 indexing case
/// applies; the default for an ordinary incremental is "no finalize required."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexBuildContract {
    /// BM25 indexes to build (baseline/rebaseline, or an incremental adding a new index).
    #[serde(default)]
    pub bm25_indexes: Vec<String>,
    /// IVFFlat indexes to finalize, with the target `lists` (baseline/rebaseline).
    #[serde(default)]
    pub ivfflat_finalize: Vec<IvfflatFinalize>,
    /// `true` only for ordinary incrementals: row-level maintenance suffices, no finalize (§7.3).
    pub row_level_maintenance_only: bool,
    /// Default: a corpus is **not** advertised query-ready until indexes are built (§7.1, INV-6).
    pub queryable_before_finalize: bool,
}

/// An IVFFlat finalize directive (design §6.2.2 `lists`/`probes` defaults, §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IvfflatFinalize {
    pub index: String,
    pub lists: u32,
    pub probes: u32,
}

/// Rollback policy (design §6.2.2 `rollback_policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackPolicy {
    /// Incrementals: the whole apply is one transaction, so a failure rolls back atomically.
    TransactionRollback,
    /// Baselines/rebaselines: keep the previous generation until the new one validates (§7.4).
    KeepPreviousGenerationUntilValidated,
}

/// The payload layout and dependency apply order (design §6.2.2 "Payload layout").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadLayout {
    pub files: Vec<PayloadFile>,
    /// The dependency apply order: base before dependents; derived `replace_set` after base;
    /// embeddings after chunks/zone units; **`official_api_responses` before the citation tables**
    /// that FK its `response_id` (the §5.2 surrogate-key exception); index finalize last.
    pub apply_order: Vec<String>,
}

impl PayloadLayout {
    /// Validate the §5.2/§6.2.2 ordering invariant: if both `official_api_responses` and a citation
    /// table that FKs it are present, the responses file is ordered first. Returns the offending
    /// pair on violation.
    #[must_use]
    pub fn citation_order_holds(&self) -> bool {
        let pos = |name: &str| self.apply_order.iter().position(|f| f == name);
        let responses = pos("official_api_responses");
        if let Some(responses) = responses {
            for fk_table in [
                "decision_legislation_citations",
                "legislation_citation_resolutions",
            ] {
                if let Some(fk) = pos(fk_table)
                    && fk < responses
                {
                    return false;
                }
            }
        }
        true
    }
}

/// One file in the payload (design §6.2.2 per-file list).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadFile {
    pub table: String,
    /// The explicit, ordered column list moved by this file (plan P3 D2) — generated columns excluded.
    /// Producer and consumer COPY exactly these columns in this order, so a producer/consumer column
    /// drift is caught instead of silently corrupting a binary COPY.
    pub columns: Vec<String>,
    pub op: crate::event::EventKind,
    pub format: PayloadFormat,
    pub compression: Compression,
    pub row_count: u64,
    pub digest: String,
}

/// On-the-wire per-file encoding (design §6.2.2, §15.2 — chosen by measurement).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PayloadFormat {
    CopyBinary,
    Jsonl,
    Parquet,
}

/// Per-file compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compression {
    None,
    Zstd,
    Gzip,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;

    fn minimal() -> EmbeddedManifest {
        EmbeddedManifest {
            identity: Identity {
                package_format_version: 1,
                package_id: "core-1041-1042".to_owned(),
                corpus: Corpus::new("core").unwrap(),
                package_kind: PackageKind::Incremental,
                from_sequence: PackageSequence::new(1041),
                to_sequence: PackageSequence::new(1042),
                previous_package_id: Some("core-1040-1040".to_owned()),
                previous_package_sha256: Some("sha256:aa".to_owned()),
                baseline_id: "core-2026-06-25-g000124".to_owned(),
                generation: "core_g000124".to_owned(),
                created_at: "2026-06-26T00:00:00Z".to_owned(),
                builder_run_id: "run-1".to_owned(),
            },
            compatibility: Compatibility {
                minimum_client_version: Version::new(0, 1, 0),
                maximum_client_version: None,
                schema_version: 18,
                schema_migration_bundle_digest: "sha256:bb".to_owned(),
                requires_extensions: vec![
                    ExtensionRequirement {
                        name: "vector".to_owned(),
                        minimum_version: None,
                    },
                    ExtensionRequirement {
                        name: "pg_search".to_owned(),
                        minimum_version: None,
                    },
                ],
                embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
                embedding_model: "bge-m3".to_owned(),
                embedding_dimension: 1024,
                embedding_normalize: true,
                builder_versions: BTreeMap::new(),
                postgres_major_min: None,
                postgres_major_max: None,
            },
            entitlement: Entitlement {
                entitlement_corpus: Corpus::new("core").unwrap(),
                tier: "open".to_owned(),
                license_epoch: 3,
                audience: None,
                entitlement_policy_digest: "sha256:cc".to_owned(),
            },
            integrity: Integrity {
                artifact_sha256: "sha256:dd".to_owned(),
                uncompressed_payload_digest: "sha256:ee".to_owned(),
                per_file_digests: BTreeMap::from([(
                    "documents".to_owned(),
                    "sha256:ff".to_owned(),
                )]),
                canonicalisation_algorithm: "jcs-sorted-json".to_owned(),
                signature_algorithm: "stub".to_owned(),
                transparency_log_index: None,
            },
            apply: ApplyContract {
                expected_client_from_sequence: PackageSequence::new(1040),
                result_sequence: PackageSequence::new(1042),
                requires_empty_generation: false,
                schema_ops_digest: "sha256:00".to_owned(),
                operations: vec![OperationCount {
                    table: "documents".to_owned(),
                    op: EventKind::Upsert,
                    count: 10,
                }],
                replace_scopes: vec![],
                preconditions: Preconditions {
                    schema_version: 18,
                    embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
                    builder_versions: BTreeMap::new(),
                    active_baseline_id: Some("core-2026-06-25-g000124".to_owned()),
                    active_generation: Some("core_g000124".to_owned()),
                },
                postconditions: Postconditions {
                    row_counts: BTreeMap::from([("documents".to_owned(), 10)]),
                    table_digests: BTreeMap::new(),
                },
                index_build: IndexBuildContract {
                    bm25_indexes: vec![],
                    ivfflat_finalize: vec![],
                    row_level_maintenance_only: true,
                    queryable_before_finalize: false,
                },
                idempotency_key: "core-1041-1042:sha256:dd".to_owned(),
                rollback_policy: RollbackPolicy::TransactionRollback,
            },
            payload: PayloadLayout {
                files: vec![PayloadFile {
                    table: "documents".to_owned(),
                    columns: vec!["document_id".to_owned(), "body".to_owned()],
                    op: EventKind::Upsert,
                    format: PayloadFormat::Jsonl,
                    compression: Compression::Zstd,
                    row_count: 10,
                    digest: "sha256:ff".to_owned(),
                }],
                apply_order: vec![
                    "documents".to_owned(),
                    "chunks".to_owned(),
                    "chunk_embeddings".to_owned(),
                    "official_api_responses".to_owned(),
                    "decision_legislation_citations".to_owned(),
                ],
            },
        }
    }

    #[test]
    fn embedded_manifest_round_trips() {
        let manifest = minimal();
        let json = serde_json::to_string(&manifest).unwrap();
        let restored: EmbeddedManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, manifest);
    }

    #[test]
    fn citation_apply_order_invariant() {
        let manifest = minimal();
        assert!(manifest.payload.citation_order_holds());

        let mut broken = minimal();
        broken.payload.apply_order = vec![
            "decision_legislation_citations".to_owned(),
            "official_api_responses".to_owned(),
        ];
        assert!(
            !broken.payload.citation_order_holds(),
            "citations before official_api_responses must violate the §5.2 ordering"
        );
    }
}
