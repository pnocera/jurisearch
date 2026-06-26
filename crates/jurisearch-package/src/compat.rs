//! Compatibility stamps and the version gate's comparison logic (design §6.2.2, §10).
//!
//! Each package carries, as first-class fields, the stamps the service compares before applying:
//! schema version, embedding fingerprint, and builder versions (§10). A package also carries a
//! `minimum_client_version`; the gate applies the package only if the client clears that minimum,
//! else warn-and-reject `client_too_old` (the existing `SchemaVersionAhead` shape, C2, drives
//! `schema_ahead`).

use crate::reject::{RejectCode, RejectError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The compatibility stamp set a package and a corpus carry (design §10). A mismatch on any stamp is
/// a precondition failure with the matching reject code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityStamps {
    /// The storage `schema_version` the package was built against / the corpus is at.
    pub schema_version: i32,
    /// The embedding fingerprint (`bge-m3:1024:cls:normalize=true`), a fingerprint bump forces a
    /// rebaseline (§6.1).
    pub embedding_fingerprint: String,
    /// Per-kind builder versions (`chunk_builder_version`, `zone_unit_builder_version`, …). Sorted
    /// (`BTreeMap`) so canonicalisation is deterministic regardless of insertion order.
    pub builder_versions: BTreeMap<String, String>,
}

impl CompatibilityStamps {
    /// Check **only** this package's embedding fingerprint and builder versions against the corpus's
    /// current stamps (`self` = package, `current` = corpus). Returns the first mismatch as a
    /// [`RejectError`] (§7.3 step 3), or `Ok` if they match.
    ///
    /// This is deliberately **not** the whole compatibility gate — its name says so. Schema is gated
    /// separately: a higher package schema requires its bundled migration (handled by the applier),
    /// and a DB ahead of the binary is the distinct `schema_ahead` check ([`schema_gate`], C2, §10).
    /// Keeping the two apart prevents a caller from treating this fingerprint/builder check as the
    /// full gate and silently accepting a schema mismatch.
    ///
    /// # Errors
    /// [`RejectCode::EmbeddingFingerprintMismatch`] or [`RejectCode::BuilderVersionMismatch`].
    pub fn check_fingerprint_and_builders(
        &self,
        current: &CompatibilityStamps,
    ) -> Result<(), RejectError> {
        if self.embedding_fingerprint != current.embedding_fingerprint {
            return Err(RejectError::new(
                RejectCode::EmbeddingFingerprintMismatch,
                format!(
                    "package fingerprint `{}` != corpus fingerprint `{}`",
                    self.embedding_fingerprint, current.embedding_fingerprint
                ),
            ));
        }
        for (builder, version) in &self.builder_versions {
            match current.builder_versions.get(builder) {
                Some(current_version) if current_version == version => {}
                Some(current_version) => {
                    return Err(RejectError::new(
                        RejectCode::BuilderVersionMismatch,
                        format!(
                            "builder `{builder}` package version `{version}` != corpus version `{current_version}`"
                        ),
                    ));
                }
                None => {
                    return Err(RejectError::new(
                        RejectCode::BuilderVersionMismatch,
                        format!("builder `{builder}` is absent on the corpus"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The outcome of the schema half of the version gate (design §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaGate {
    /// Package schema == the corpus/client schema: apply normally, no migration needed.
    Equal,
    /// Package schema is ahead of the corpus by additive migration(s) the client binary understands:
    /// apply the package's bundled schema migration **before** the row apply, then proceed (§6.1,
    /// §10 "additive → rides in a normal package").
    AdditiveAhead,
}

/// The schema dimension of the §10 version gate. Compares a package's `schema_version` and the
/// corpus's current `schema_version` against the **client binary's** known `schema_version`
/// (`CURRENT_SCHEMA_VERSION`, C2). This is distinct from [`CompatibilityStamps::check_fingerprint_and_builders`]
/// (fingerprint/builder only) — together they form the precondition gate.
///
/// Cases:
/// * `corpus_schema > client_binary_schema` → the DB is ahead of the binary → `schema_ahead`
///   (the existing `SchemaVersionAhead` shape, C2): the binary must be upgraded first.
/// * `package_schema > client_binary_schema` → the package needs DDL the binary does not know →
///   `client_too_old` (a raised `minimum_client_version`, §10).
/// * `package_schema > corpus_schema` (both ≤ binary) → [`SchemaGate::AdditiveAhead`]: the package's
///   bundled additive migration runs before the row apply.
/// * `package_schema == corpus_schema` → [`SchemaGate::Equal`].
/// * `package_schema < corpus_schema` → `baseline_required`: the package predates a schema the corpus
///   has already advanced past (a stale/reordered package); it cannot be applied as an incremental.
///
/// # Errors
/// [`RejectCode::SchemaAhead`], [`RejectCode::ClientTooOld`], or [`RejectCode::BaselineRequired`].
pub fn schema_gate(
    package_schema: i32,
    corpus_schema: i32,
    client_binary_schema: i32,
) -> Result<SchemaGate, RejectError> {
    if corpus_schema > client_binary_schema {
        return Err(RejectError::new(
            RejectCode::SchemaAhead,
            format!(
                "corpus schema_version {corpus_schema} is ahead of the binary {client_binary_schema}; upgrade the client"
            ),
        ));
    }
    if package_schema > client_binary_schema {
        return Err(RejectError::new(
            RejectCode::ClientTooOld,
            format!(
                "package schema_version {package_schema} needs DDL the binary {client_binary_schema} does not have"
            ),
        ));
    }
    match package_schema.cmp(&corpus_schema) {
        std::cmp::Ordering::Equal => Ok(SchemaGate::Equal),
        std::cmp::Ordering::Greater => Ok(SchemaGate::AdditiveAhead),
        std::cmp::Ordering::Less => Err(RejectError::new(
            RejectCode::BaselineRequired,
            format!(
                "package schema_version {package_schema} predates the corpus schema {corpus_schema}; a baseline is required"
            ),
        )),
    }
}

/// A dotted `major.minor.patch` client/package version (design §10). Compared numerically (not
/// lexically) so `0.10.0 > 0.9.0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse a `major.minor.patch` string.
    ///
    /// # Errors
    /// [`VersionParseError`] if the string is not exactly three dot-separated unsigned integers.
    pub fn parse(text: &str) -> Result<Self, VersionParseError> {
        let mut parts = text.split('.');
        let mut next = |field: &'static str| -> Result<u32, VersionParseError> {
            parts
                .next()
                .ok_or(VersionParseError::Missing { field })?
                .parse()
                .map_err(|_| VersionParseError::NotNumeric {
                    field,
                    text: text.to_owned(),
                })
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patch")?;
        if parts.next().is_some() {
            return Err(VersionParseError::TooManyComponents {
                text: text.to_owned(),
            });
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    /// The §10 version gate: this client version clears `minimum`. Equivalently, the client is **not**
    /// `client_too_old`.
    #[must_use]
    pub fn satisfies_minimum(self, minimum: Version) -> bool {
        self >= minimum
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl TryFrom<String> for Version {
    type Error = VersionParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<Version> for String {
    fn from(value: Version) -> Self {
        value.to_string()
    }
}

/// Failure modes of [`Version::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionParseError {
    #[error("version is missing its {field} component")]
    Missing { field: &'static str },
    #[error("version {field} component of `{text}` is not numeric")]
    NotNumeric { field: &'static str, text: String },
    #[error("version `{text}` has more than three components")]
    TooManyComponents { text: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamps(fp: &str, builders: &[(&str, &str)]) -> CompatibilityStamps {
        CompatibilityStamps {
            schema_version: 18,
            embedding_fingerprint: fp.to_owned(),
            builder_versions: builders
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn version_compares_numerically_not_lexically() {
        assert!(Version::parse("0.10.0").unwrap() > Version::parse("0.9.0").unwrap());
        assert!(Version::new(1, 0, 0).satisfies_minimum(Version::new(0, 9, 9)));
        assert!(!Version::new(0, 8, 0).satisfies_minimum(Version::new(0, 9, 0)));
    }

    #[test]
    fn version_parse_rejects_malformed() {
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("1.x.3").is_err());
    }

    #[test]
    fn matching_stamps_pass() {
        let package = stamps("bge-m3:1024", &[("chunk_builder_version", "c1")]);
        let current = stamps("bge-m3:1024", &[("chunk_builder_version", "c1")]);
        assert!(package.check_fingerprint_and_builders(&current).is_ok());
    }

    #[test]
    fn fingerprint_mismatch_is_rejected() {
        let package = stamps("bge-m3:1024", &[]);
        let current = stamps("bge-m3:512", &[]);
        let err = package
            .check_fingerprint_and_builders(&current)
            .unwrap_err();
        assert_eq!(err.code, RejectCode::EmbeddingFingerprintMismatch);
    }

    #[test]
    fn builder_mismatch_is_rejected() {
        let package = stamps("bge-m3:1024", &[("chunk_builder_version", "c2")]);
        let current = stamps("bge-m3:1024", &[("chunk_builder_version", "c1")]);
        let err = package
            .check_fingerprint_and_builders(&current)
            .unwrap_err();
        assert_eq!(err.code, RejectCode::BuilderVersionMismatch);
    }

    #[test]
    fn schema_gate_models_every_case() {
        // Equal: package == corpus == binary.
        assert_eq!(schema_gate(18, 18, 18).unwrap(), SchemaGate::Equal);
        // Additive ahead: package one step ahead of corpus, both <= binary.
        assert_eq!(schema_gate(18, 17, 18).unwrap(), SchemaGate::AdditiveAhead);
        // DB ahead of binary -> schema_ahead.
        assert_eq!(
            schema_gate(18, 19, 18).unwrap_err().code,
            RejectCode::SchemaAhead
        );
        // Package needs DDL the binary lacks -> client_too_old.
        assert_eq!(
            schema_gate(19, 18, 18).unwrap_err().code,
            RejectCode::ClientTooOld
        );
        // Package predates the corpus schema -> baseline_required.
        assert_eq!(
            schema_gate(17, 18, 18).unwrap_err().code,
            RejectCode::BaselineRequired
        );
    }

    #[test]
    fn builder_versions_canonicalise_regardless_of_insertion_order() {
        let a = stamps("fp", &[("b", "2"), ("a", "1")]);
        let b = stamps("fp", &[("a", "1"), ("b", "2")]);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }
}
