//! The closed vocabulary of warn-and-reject codes (design §6.3).
//!
//! Every warn-and-reject outcome carries exactly one of these codes so the service can explain
//! itself and a conformance suite can assert the right code for each failure (INV-9). The enum is
//! **closed**: it is exhaustive against §6.3 and adding a wire reason without adding a variant is a
//! compile error at every match site.

use serde::{Deserialize, Serialize};

/// A machine-readable reject code (design §6.3). The `serde` token is the wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    /// The client binary is older than the package's `minimum_client_version` (§10).
    ClientTooOld,
    /// The local DB's `schema_migrations` max exceeds the binary (the existing `SchemaVersionAhead`
    /// shape, C2) — the binary must be upgraded before it can apply.
    SchemaAhead,
    /// No locally installed license token covers the package's `entitlement_corpus`/`tier` (§11.3).
    MissingEntitlement,
    /// `corpus_state.sequence != expected_client_from_sequence` — a missing or out-of-order package
    /// (§7.3). Never skipped; caught up by applying the missing packages in order.
    SequenceGap,
    /// The package targets a generation/baseline the corpus is no longer on (§7.4 switch check).
    WrongGeneration,
    /// The package's `embedding_fingerprint` does not match the corpus's current fingerprint.
    EmbeddingFingerprintMismatch,
    /// A `builder_versions` stamp does not match what the corpus currently carries.
    BuilderVersionMismatch,
    /// A signature (remote manifest, embedded manifest) failed verification (§11.1).
    SignatureInvalid,
    /// An artifact / per-file / postcondition digest did not match (§11.1).
    DigestMismatch,
    /// A required extension (`vector`, `pg_search`) is absent or incompatible (`requires_extensions`,
    /// §6.2.2).
    ExtensionMissing,
    /// The client is past `min_available_sequence` (or the chain crosses a reissue) and must load a
    /// fresh baseline instead of an incremental chain (§9.4).
    BaselineRequired,
}

impl RejectCode {
    /// The wire token (matches the `serde` rename and the §6.3 vocabulary).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RejectCode::ClientTooOld => "client_too_old",
            RejectCode::SchemaAhead => "schema_ahead",
            RejectCode::MissingEntitlement => "missing_entitlement",
            RejectCode::SequenceGap => "sequence_gap",
            RejectCode::WrongGeneration => "wrong_generation",
            RejectCode::EmbeddingFingerprintMismatch => "embedding_fingerprint_mismatch",
            RejectCode::BuilderVersionMismatch => "builder_version_mismatch",
            RejectCode::SignatureInvalid => "signature_invalid",
            RejectCode::DigestMismatch => "digest_mismatch",
            RejectCode::ExtensionMissing => "extension_missing",
            RejectCode::BaselineRequired => "baseline_required",
        }
    }

    /// A one-line description of what triggers this code (design §6.3 / §11.1). Used by the doc test
    /// below to assert every code has a recorded trigger, and by the service to explain rejections.
    #[must_use]
    pub const fn trigger(self) -> &'static str {
        match self {
            RejectCode::ClientTooOld => {
                "package minimum_client_version is newer than the client binary (§10)"
            }
            RejectCode::SchemaAhead => "local schema_migrations max exceeds the binary (C2)",
            RejectCode::MissingEntitlement => {
                "no local license token covers the package's corpus/tier (§11.3)"
            }
            RejectCode::SequenceGap => {
                "corpus_state.sequence != expected_client_from_sequence (§7.3)"
            }
            RejectCode::WrongGeneration => {
                "the corpus is no longer on the generation/baseline the package targets (§7.4)"
            }
            RejectCode::EmbeddingFingerprintMismatch => {
                "package embedding_fingerprint differs from the corpus's current fingerprint"
            }
            RejectCode::BuilderVersionMismatch => {
                "a builder_versions stamp differs from what the corpus carries"
            }
            RejectCode::SignatureInvalid => "a manifest signature failed verification (§11.1)",
            RejectCode::DigestMismatch => {
                "an artifact / per-file / postcondition digest did not match (§11.1)"
            }
            RejectCode::ExtensionMissing => {
                "a requires_extensions entry is absent or incompatible (§6.2.2)"
            }
            RejectCode::BaselineRequired => {
                "client is past min_available_sequence or the chain crosses a reissue (§9.4)"
            }
        }
    }

    /// Every reject code, in §6.3 order. Used to assert exhaustiveness in tests and conformance.
    #[must_use]
    pub const fn all() -> [RejectCode; 11] {
        [
            RejectCode::ClientTooOld,
            RejectCode::SchemaAhead,
            RejectCode::MissingEntitlement,
            RejectCode::SequenceGap,
            RejectCode::WrongGeneration,
            RejectCode::EmbeddingFingerprintMismatch,
            RejectCode::BuilderVersionMismatch,
            RejectCode::SignatureInvalid,
            RejectCode::DigestMismatch,
            RejectCode::ExtensionMissing,
            RejectCode::BaselineRequired,
        ]
    }
}

impl std::fmt::Display for RejectCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A warn-and-reject error: a code plus a human-readable detail, carrying **no** cursor movement
/// (INV-9). This is the error every apply/verify precondition returns on failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct RejectError {
    pub code: RejectCode,
    pub detail: String,
}

impl RejectError {
    pub fn new(code: RejectCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §6.3 exhaustiveness: the closed vocabulary, verbatim, in order. If §6.3 ever grows a code,
    /// this list and [`RejectCode::all`] must grow with it.
    const SECTION_6_3_VOCABULARY: &[&str] = &[
        "client_too_old",
        "schema_ahead",
        "missing_entitlement",
        "sequence_gap",
        "wrong_generation",
        "embedding_fingerprint_mismatch",
        "builder_version_mismatch",
        "signature_invalid",
        "digest_mismatch",
        "extension_missing",
        "baseline_required",
    ];

    #[test]
    fn enum_is_exhaustive_against_section_6_3() {
        let tokens: Vec<&str> = RejectCode::all().iter().map(|c| c.as_str()).collect();
        assert_eq!(tokens, SECTION_6_3_VOCABULARY);
    }

    /// Plan P0 acceptance: "a doc test maps each to its trigger."
    #[test]
    fn every_code_has_a_recorded_trigger() {
        for code in RejectCode::all() {
            assert!(
                !code.trigger().is_empty(),
                "{code} must document its trigger"
            );
        }
    }

    #[test]
    fn reject_codes_round_trip_on_the_wire() {
        for code in RejectCode::all() {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            assert_eq!(serde_json::from_str::<RejectCode>(&json).unwrap(), code);
        }
    }
}
