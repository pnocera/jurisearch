//! Corpus identity and attribution (design §4.1, §5.1; plan P0 "Corpus attribution").
//!
//! Per-corpus packaging, sequencing, and entitlement all key on a **corpus** (`core`, `inpi`, …).
//! Every replicated row resolves to exactly one corpus via [`corpus_for_source`], scoped to the
//! **owning document's** `source` (plan P0 risk note). The mapping is recorded **here**, in one
//! place, so the storage migration's SQL backfill (`documents.corpus`) and every Rust build path
//! agree; a storage test asserts the SQL `CASE` matches [`KNOWN_SOURCES`].
//!
//! Ambiguous / unknown sources **fail loudly** (`Err(AttributionError::UnknownSource)`), never
//! silently default to a corpus.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A corpus name (`core`, `inpi`, …). Open-ended by design — new corpora are added without a code
/// change to consumers — but validated to a stable, lowercase, filesystem/schema-safe token because
/// it appears in generation schema names (`jurisearch_server_<corpus>_gNNNN`, §4.1) and license
/// tokens (§11.3).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Corpus(String);

impl Corpus {
    /// Construct a corpus, validating the token shape.
    ///
    /// # Errors
    /// Returns [`AttributionError::InvalidCorpus`] if `name` is empty, too long, or contains
    /// anything other than `[a-z0-9_]` (so it is always safe to embed in a schema name).
    pub fn new(name: impl Into<String>) -> Result<Self, AttributionError> {
        let name = name.into();
        let valid = !name.is_empty()
            && name.len() <= 48
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase());
        if valid {
            Ok(Self(name))
        } else {
            Err(AttributionError::InvalidCorpus { name })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Corpus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Corpus {
    type Error = AttributionError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Corpus> for String {
    fn from(value: Corpus) -> Self {
        value.0
    }
}

/// The closed `source → corpus` attribution table for the currently ingested sources.
///
/// LEGI articles (`legi`) and DILA jurisprudence (`cass`/`capp`/`inca`/`jade`) are all **open data**
/// and belong to the `core` corpus. Restricted corpora (e.g. `inpi`/RNE) are not yet ingested; when
/// one is, it is added here and the storage backfill `CASE` is extended in lock-step (the storage
/// drift test enforces this).
pub const KNOWN_SOURCES: &[(&str, &str)] = &[
    ("legi", "core"),
    ("cass", "core"),
    ("capp", "core"),
    ("inca", "core"),
    ("jade", "core"),
];

/// Resolve the corpus that owns a replicated row, from its owning document's `source` (design
/// §4.1; plan P0). Unknown sources fail loudly so a new source cannot ship without attribution.
///
/// # Errors
/// [`AttributionError::UnknownSource`] when `source` is not in [`KNOWN_SOURCES`].
pub fn corpus_for_source(source: &str) -> Result<Corpus, AttributionError> {
    KNOWN_SOURCES
        .iter()
        .find(|(s, _)| *s == source)
        .map(|(_, corpus)| Corpus::new(*corpus))
        .transpose()?
        .ok_or_else(|| AttributionError::UnknownSource {
            source_name: source.to_owned(),
        })
}

/// Failure modes of corpus construction / attribution.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttributionError {
    /// A replicated row carried a `source` with no corpus mapping (plan P0 "ambiguous rows fail
    /// loudly"). Add it to [`KNOWN_SOURCES`] and the storage backfill `CASE`. The field is
    /// `source_name` (not `source`) so `thiserror` does not treat it as an error source.
    #[error("no corpus attribution rule for source `{source_name}` (add it to KNOWN_SOURCES)")]
    UnknownSource { source_name: String },
    /// A corpus token that cannot be embedded in a schema name.
    #[error("invalid corpus name `{name}` (expected lowercase `[a-z][a-z0-9_]*`, <= 48 chars)")]
    InvalidCorpus { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_sources_all_resolve_to_a_valid_corpus() {
        for (source, expected) in KNOWN_SOURCES {
            let corpus = corpus_for_source(source).expect("known source resolves");
            assert_eq!(corpus.as_str(), *expected);
        }
    }

    #[test]
    fn unknown_source_fails_loudly() {
        let err = corpus_for_source("inpi").unwrap_err();
        assert_eq!(
            err,
            AttributionError::UnknownSource {
                source_name: "inpi".to_owned()
            }
        );
    }

    #[test]
    fn corpus_token_is_schema_safe() {
        assert!(Corpus::new("core").is_ok());
        assert!(Corpus::new("inpi_rne").is_ok());
        assert!(Corpus::new("").is_err());
        assert!(Corpus::new("Core").is_err());
        assert!(Corpus::new("2core").is_err());
        assert!(Corpus::new("core; drop").is_err());
    }

    #[test]
    fn corpus_round_trips_through_serde() {
        let corpus = Corpus::new("core").unwrap();
        let json = serde_json::to_string(&corpus).unwrap();
        assert_eq!(json, "\"core\"");
        assert_eq!(serde_json::from_str::<Corpus>(&json).unwrap(), corpus);
        assert!(serde_json::from_str::<Corpus>("\"BAD\"").is_err());
    }
}
