//! The two identities and the `official_api_responses` surrogate-key exception (design §8.1, §5.2).
//!
//! These helpers re-state the canonical-id construction the ingest pipeline already uses
//! (`jurisearch-ingest` `legi/canonical.rs`, `juri/types.rs`) so the contract crate stays a pure
//! leaf (no dependency on the heavy ingest crate). The construction is identical and tested against
//! the documented format; a future refactor may have ingest depend on these helpers instead.

use serde::{Deserialize, Serialize};

/// A specific, immutable historical version, identified by `document_id` (design §8.1).
///
/// Because supersession **retains** old version rows, a reference pinned to a specific-version
/// `document_id` keeps resolving across updates *and* across a re-baseline (INV-4). This is the
/// identity to use for evidence/citations ("this exact text I saw", §8.3).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocumentVersionId(String);

impl DocumentVersionId {
    /// A LEGI article version: `legi:<source_uid>@<valid_from>` (C4, `legi/canonical.rs`).
    #[must_use]
    pub fn legi(source_uid: &str, valid_from: &str) -> Self {
        Self(format!("legi:{source_uid}@{valid_from}"))
    }

    /// A jurisprudence decision: `<source>:<source_uid>` (C4, `juri/types.rs`). Decisions are not
    /// temporal versions, so the document id is usually both the specific and the logical identity
    /// (§8.3).
    #[must_use]
    pub fn decision(source: &str, source_uid: &str) -> Self {
        Self(format!("{source}:{source_uid}"))
    }

    /// Wrap an already-constructed `document_id` (e.g. read back from a package or the DB).
    #[must_use]
    pub fn from_raw(document_id: impl Into<String>) -> Self {
        Self(document_id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DocumentVersionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// "The article, as-of date D" — a logical article tracked over time (design §8.1).
///
/// `document_id` is **not** a stable identifier for the logical article (each version is a new row
/// with a new `document_id`). The logical identity is `source_uid`/`version_group`; the resolver
/// selects the version whose validity window contains `as_of_date` (§8.3). Use this to "track this
/// article over time" / "the article applicable at date D".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LogicalArticleId {
    pub source: String,
    pub source_uid: String,
    /// The version-group key when the source provides one (LEGI). `None` falls back to `source_uid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_group: Option<String>,
    /// The as-of date the reference resolves against (ISO `YYYY-MM-DD`).
    pub as_of_date: String,
}

impl LogicalArticleId {
    #[must_use]
    pub fn new(
        source: impl Into<String>,
        source_uid: impl Into<String>,
        version_group: Option<String>,
        as_of_date: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            source_uid: source_uid.into(),
            version_group,
            as_of_date: as_of_date.into(),
        }
    }

    /// The grouping key the resolver keys on: `version_group` if present, else `source_uid` (§8.1).
    #[must_use]
    pub fn group_key(&self) -> &str {
        self.version_group.as_deref().unwrap_or(&self.source_uid)
    }
}

/// The `official_api_responses` surrogate-key rule (design §5.2 "Identity for non-`document_id`…").
///
/// `official_api_responses` uses a **server-assigned `response_id bigserial`** that the v17 citation
/// tables FK into, with "highest `response_id` per decision = latest archived response". The
/// contract: the producer's `response_id` is the **immutable replicated key** — packages carry it
/// verbatim and the client **inserts it explicitly** (overriding the local `bigserial`), never
/// minting a local id. Apply order lands `official_api_responses` **before** the dependent citation
/// tables (§6.2.2). This type makes that rule explicit and non-defaultable in build/apply code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResponseId(i64);

impl ResponseId {
    #[must_use]
    pub const fn new(producer_response_id: i64) -> Self {
        Self(producer_response_id)
    }

    /// The replicated key, carried verbatim and inserted explicitly on the client.
    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legi_document_id_matches_canonical_format() {
        // Mirrors `legi/canonical.rs`: document_id == format!("legi:{source_uid}@{valid_from}").
        let id = DocumentVersionId::legi("LEGIARTI000006419292", "2020-01-01");
        assert_eq!(id.as_str(), "legi:LEGIARTI000006419292@2020-01-01");
    }

    #[test]
    fn decision_document_id_matches_canonical_format() {
        // Mirrors `juri/types.rs`: document_id == format!("{source}:{source_uid}").
        let id = DocumentVersionId::decision("cass", "JURITEXT000000000001");
        assert_eq!(id.as_str(), "cass:JURITEXT000000000001");
    }

    #[test]
    fn logical_article_group_key_prefers_version_group() {
        let with_group = LogicalArticleId::new(
            "legi",
            "LEGIARTI000006419292",
            Some("GROUP-1".to_owned()),
            "2020-01-01",
        );
        assert_eq!(with_group.group_key(), "GROUP-1");
        let without = LogicalArticleId::new("legi", "LEGIARTI000006419292", None, "2020-01-01");
        assert_eq!(without.group_key(), "LEGIARTI000006419292");
    }

    #[test]
    fn response_id_is_carried_verbatim() {
        let id = ResponseId::new(42);
        assert_eq!(id.get(), 42);
        // Transparent on the wire so it inserts as a plain integer.
        assert_eq!(serde_json::to_string(&id).unwrap(), "42");
        assert_eq!(serde_json::from_str::<ResponseId>("42").unwrap(), id);
    }
}
