//! The per-corpus remote manifest the client polls to plan downloads (design §6.2.1).
//!
//! Lists the corpus's chain head, retention window, active baseline, and per-package
//! compatibility/size metadata so the client can **plan** (§9.4) without downloading. Signed as a
//! whole via [`crate::signed::Signed`]; each package/baseline entry additionally carries the
//! artifact's own `sha256` + [`Signature`] for the §11.1 verification chain.

use crate::compat::Version;
use crate::corpus::Corpus;
use crate::crypto::{KeyId, Signature};
use crate::package_kind::PackageKind;
use crate::sequence::PackageSequence;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The remote manifest body (design §6.2.1). Wrap in [`crate::signed::Signed`] to sign.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteManifest {
    pub manifest_version: u32,
    /// ISO-8601 build timestamp. A string (not a parsed time) so canonicalisation is trivially
    /// stable and the contract crate stays time-library-free.
    pub generated_at: String,
    pub publisher: String,
    pub corpus: Corpus,
    pub environment: String,
    pub head_sequence: PackageSequence,
    pub min_available_sequence: PackageSequence,
    pub active_baseline: BaselineRef,
    pub packages: Vec<RemotePackageEntry>,
    pub catchup_ranges: Vec<CatchupRange>,
    pub catchup_policy: CatchupPolicy,
    pub entitlement: EntitlementListing,
    pub signing: SigningInfo,
}

impl RemoteManifest {
    /// The package entry whose `to_sequence` is the chain head, if present.
    #[must_use]
    pub fn head_package(&self) -> Option<&RemotePackageEntry> {
        self.packages
            .iter()
            .find(|p| p.to_sequence == self.head_sequence)
    }

    /// Whether `client_sequence` is older than the retained window and therefore needs a baseline
    /// (the `baseline_required` half of §9.4 catch-up routing).
    #[must_use]
    pub fn requires_baseline_for(&self, client_sequence: PackageSequence) -> bool {
        client_sequence < self.min_available_sequence
    }
}

/// A pointer to the corpus's active baseline artifact (design §6.2.1 `active_baseline`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaselineRef {
    pub baseline_id: String,
    pub generation: String,
    /// `baseline` (first load) vs `rebaseline` (forward-supersession reissue) — so the planner can
    /// dispatch the right applier when it routes a client to a fresh baseline (plan P7).
    pub package_kind: PackageKind,
    pub sequence: PackageSequence,
    pub schema_version: i32,
    /// The minimum client binary version that can apply this baseline — so the planner can `Blocked`
    /// before downloading rather than discovering it at apply (plan P7, §10).
    pub minimum_client_version: Version,
    pub artifact_uri: String,
    pub compressed_size_bytes: u64,
    /// Uncompressed payload bytes — the §9.4 "cumulative uncompressed > N% of baseline" rule.
    pub uncompressed_size_bytes: u64,
    /// Estimated media-baseline LOAD seconds on the reference client — the §9.4 "expected apply time
    /// exceeds media baseline load time" rule.
    pub estimated_load_seconds: u32,
    pub sha256: String,
    pub signature: Signature,
}

/// One published package in the chain (design §6.2.1 `packages[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePackageEntry {
    pub package_id: String,
    pub from_sequence: PackageSequence,
    pub to_sequence: PackageSequence,
    pub artifact_uri: String,
    pub compressed_size_bytes: u64,
    pub uncompressed_size_bytes: u64,
    /// Estimated apply cost on the reference client profile — an input to the §9.4 size/cost
    /// decision (not chain length).
    pub estimated_apply_seconds: u32,
    /// Per-table row counts touched, for planning/observability.
    #[serde(default)]
    pub row_counts: BTreeMap<String, u64>,
    pub requires_baseline: bool,
    pub minimum_client_version: Version,
    pub schema_version: i32,
    pub embedding_fingerprint: String,
    #[serde(default)]
    pub builder_versions: BTreeMap<String, String>,
    pub sha256: String,
    pub signature: Signature,
}

/// A precomputed catch-up routing range (design §6.2.1 `catchup_ranges[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatchupRange {
    pub from_sequence: PackageSequence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_sequence: Option<PackageSequence>,
    pub mode: CatchupMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_id: Option<String>,
}

/// How a client in a given range should catch up (design §6.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatchupMode {
    /// The retained incremental chain covers this range — apply in order.
    IncrementalOk,
    /// The client is too far back; load the named baseline instead.
    RequiresBaseline,
}

/// The size/cost-driven catch-up thresholds (design §6.2.1 `catchup_policy`, §9.4).
///
/// Manifest-configured per corpus so the policy is tunable **without a client upgrade** (conception
/// §5 OCP: policy is data). The ratio is expressed in **per-mille** (integer, `330` = 0.33) rather
/// than a raw `f64`: the manifest is signed and canonicalised, and an integer keeps the wire form
/// deterministic with an explicit, bounded domain (no NaN/Inf, no float-formatting ambiguity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatchupPolicy {
    pub max_incremental_packages: u32,
    /// The cumulative-COMPRESSED-diff-to-baseline ceiling, in per-mille (e.g. `330` = 33%).
    pub max_cumulative_diff_to_baseline_permille: u32,
    /// The cumulative-UNCOMPRESSED-diff-to-baseline ceiling, in per-mille (§9.4 ~50%).
    pub max_cumulative_uncompressed_to_baseline_permille: u32,
    /// The estimated cumulative apply-cost budget in seconds — the §9.4 "estimated apply work under
    /// budget" rule (≈ 30–45 min on the reference client). Tunable per corpus without a client upgrade.
    pub max_apply_seconds_budget: u32,
}

impl CatchupPolicy {
    /// The ceiling as a fraction (e.g. `330` → `0.33`) for the planner's size comparison (§9.4).
    #[must_use]
    pub fn max_cumulative_diff_to_baseline_ratio(&self) -> f64 {
        f64::from(self.max_cumulative_diff_to_baseline_permille) / 1000.0
    }
}

/// The corpus's entitlement listing (design §6.2.1 `entitlement`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntitlementListing {
    pub corpus: Corpus,
    pub tier: EntitlementTier,
    pub license_epoch: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
}

/// Open vs subscription tiering (design §6.2.1, §11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntitlementTier {
    /// No subscription requirement — packages apply given the bytes.
    Open,
    /// Requires a locally installed license token covering this corpus (§11.3).
    Subscription,
}

/// The signing key advertised for this manifest's artifacts (design §6.2.1 `signing`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningInfo {
    pub key_id: KeyId,
    pub algorithm: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KeyEpoch, KeyId};

    fn sig() -> Signature {
        Signature {
            algorithm: "stub".to_owned(),
            key_id: KeyId("k".to_owned()),
            key_epoch: KeyEpoch(0),
            signature_hex: String::new(),
        }
    }

    fn sample() -> RemoteManifest {
        RemoteManifest {
            manifest_version: 1,
            generated_at: "2026-06-26T00:00:00Z".to_owned(),
            publisher: "jurisearch".to_owned(),
            corpus: Corpus::new("core").unwrap(),
            environment: "production".to_owned(),
            head_sequence: PackageSequence::new(1088),
            min_available_sequence: PackageSequence::new(970),
            active_baseline: BaselineRef {
                baseline_id: "core-2026-06-25-g000124".to_owned(),
                generation: "core_g000124".to_owned(),
                package_kind: PackageKind::Baseline,
                sequence: PackageSequence::new(1040),
                schema_version: 18,
                minimum_client_version: Version::new(0, 1, 0),
                artifact_uri: "media://core-baseline".to_owned(),
                compressed_size_bytes: 0,
                uncompressed_size_bytes: 0,
                estimated_load_seconds: 0,
                sha256: "sha256:00".to_owned(),
                signature: sig(),
            },
            packages: vec![RemotePackageEntry {
                package_id: "core-1041-1042".to_owned(),
                from_sequence: PackageSequence::new(1041),
                to_sequence: PackageSequence::new(1042),
                artifact_uri: "https://host/core-1041-1042".to_owned(),
                compressed_size_bytes: 0,
                uncompressed_size_bytes: 0,
                estimated_apply_seconds: 0,
                row_counts: BTreeMap::from([("documents".to_owned(), 10)]),
                requires_baseline: false,
                minimum_client_version: Version::new(0, 1, 0),
                schema_version: 18,
                embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
                builder_versions: BTreeMap::new(),
                sha256: "sha256:01".to_owned(),
                signature: sig(),
            }],
            catchup_ranges: vec![
                CatchupRange {
                    from_sequence: PackageSequence::new(1000),
                    to_sequence: Some(PackageSequence::new(1088)),
                    mode: CatchupMode::IncrementalOk,
                    baseline_id: None,
                },
                CatchupRange {
                    from_sequence: PackageSequence::new(800),
                    to_sequence: None,
                    mode: CatchupMode::RequiresBaseline,
                    baseline_id: Some("core-2026-06-25-g000124".to_owned()),
                },
            ],
            catchup_policy: CatchupPolicy {
                max_incremental_packages: 120,
                max_cumulative_diff_to_baseline_permille: 330,
                max_cumulative_uncompressed_to_baseline_permille: 500,
                max_apply_seconds_budget: 2700,
            },
            entitlement: EntitlementListing {
                corpus: Corpus::new("core").unwrap(),
                tier: EntitlementTier::Open,
                license_epoch: 3,
                audience: None,
            },
            signing: SigningInfo {
                key_id: KeyId("k".to_owned()),
                algorithm: "stub".to_owned(),
            },
        }
    }

    #[test]
    fn remote_manifest_round_trips_the_design_example() {
        let manifest = sample();
        let json = serde_json::to_string(&manifest).unwrap();
        let restored: RemoteManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, manifest);
    }

    #[test]
    fn head_package_and_baseline_routing() {
        let manifest = sample();
        assert_eq!(
            manifest.head_package().map(|p| p.package_id.as_str()),
            None,
            "head 1088 has no listed package in this sample"
        );
        assert!(manifest.requires_baseline_for(PackageSequence::new(800)));
        assert!(!manifest.requires_baseline_for(PackageSequence::new(1041)));
    }
}
