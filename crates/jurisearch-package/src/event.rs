//! The three mandatory event kinds and the `replace_set` payload contract (design §5.2, §5.3).
//!
//! A package is a **diff**, so all three event kinds are required *fields of the format*, not
//! options (INV-1). A purely additive `upsert` stream both misses validity-window closes and
//! orphans derived rows.

use serde::{Deserialize, Serialize};

/// The semantic operation a change carries (design §5.2; the outbox `op` CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// New rows **and** in-place updates of existing base rows (a closing `valid_to`, a source
    /// correction — C3). Idempotent via deterministic PK + `source_payload_hash`.
    Upsert,
    /// Rare base-document removal (pseudonymisation/redaction, mis-ingest). Low-volume; **not** the
    /// representation for routine derived rebuilds.
    Delete,
    /// The scoped rebuild of a derived set (design §5.3).
    ReplaceSet,
}

impl EventKind {
    /// The wire token, matching the outbox `op` CHECK in `package_change_log` (design §5.1).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EventKind::Upsert => "upsert",
            EventKind::Delete => "delete",
            EventKind::ReplaceSet => "replace_set",
        }
    }

    /// All three kinds, in apply-relevant order (base mutations, then set rebuilds).
    #[must_use]
    pub const fn all() -> [EventKind; 3] {
        [EventKind::Upsert, EventKind::Delete, EventKind::ReplaceSet]
    }
}

/// The scope a change is attributed to (the outbox `scope_kind`/`scope_key`, design §5.1).
///
/// Derived rebuilds are always **document-scoped** (§5.3); base mutations may be scoped to a single
/// document or a logical article.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    /// A specific document/decision (`document_id`).
    Document,
    /// A logical article tracked over time (`source_uid`/`version_group`).
    LogicalArticle,
}

impl ScopeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ScopeKind::Document => "document",
            ScopeKind::LogicalArticle => "logical_article",
        }
    }
}

/// The derived table-group a [`EventKind::ReplaceSet`] rebuilds (design §5.3 scope rules).
///
/// The distinction between [`ReplaceSetGroup::ChunksWithEmbeddings`] and
/// [`ReplaceSetGroup::ChunkEmbeddings`] is the subtlest §5.3 correctness point: a chunk
/// *membership/partition/body* change **must** use `chunks_with_embeddings` (chunks are
/// BM25-indexed replicated rows and the live LEGI projection does not delete dropped chunks), while
/// a pure embedding-payload correction on an unchanged chunk row set may use the narrower
/// `chunk_embeddings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceSetGroup {
    /// `zone_units` (+ cascaded `zone_unit_embeddings`), scope = one `document_id`.
    ZoneUnits,
    /// `chunks` (+ cascaded `chunk_embeddings`), scope = one `document_id`. Required whenever chunk
    /// membership/partitioning/body changes so stale BM25-visible chunk rows cannot survive.
    ChunksWithEmbeddings,
    /// `chunk_embeddings` only — allowed **only** when the chunk row set is unchanged and just an
    /// embedding payload/fingerprint is corrected.
    ChunkEmbeddings,
    /// The per-document `decision_zones` Judilibre-overlay cache row (a singleton-per-document derived
    /// set), scope = one `document_id`. Emitted by the zone-enrichment writer; replaced as a whole so a
    /// re-resolution cannot leave a stale overlay (plan P4).
    DecisionZones,
}

impl ReplaceSetGroup {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReplaceSetGroup::ZoneUnits => "zone_units",
            ReplaceSetGroup::ChunksWithEmbeddings => "chunks_with_embeddings",
            ReplaceSetGroup::ChunkEmbeddings => "chunk_embeddings",
            ReplaceSetGroup::DecisionZones => "decision_zones",
        }
    }

    /// Whether applying this group may leave stale BM25-visible rows if expressed as the narrower
    /// embeddings-only replacement (the §5.3 stale-chunk hazard). Only [`ChunkEmbeddings`] is safe
    /// to express narrowly, and only when the chunk row set is unchanged.
    ///
    /// [`ChunkEmbeddings`]: ReplaceSetGroup::ChunkEmbeddings
    #[must_use]
    pub const fn rebuilds_bm25_rows(self) -> bool {
        matches!(self, ReplaceSetGroup::ChunksWithEmbeddings)
    }
}

/// The §5.3 `replace_set` payload contract (one scope's complete derived set).
///
/// Carries the scope key, **the rebuilt row bodies** (`rows`), the stable PKs, the builder/fingerprint
/// stamps that make the set reproducible, and a deterministic `set_digest` over the ordered
/// `(pk, row_hash)` pairs so the applier can compare the post-apply set against the producer's. The
/// applier deletes the scope's current set, inserts these `rows` (in dependency order — units before
/// their embeddings), then verifies the result against `set_digest` (§5.3).
///
/// Not `Eq` because `rows` holds `serde_json::Value` bodies (which carry `f64` numbers); `PartialEq`
/// is sufficient for round-trip tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplaceSet {
    /// Always `"replace_set"` on the wire; present so a payload is self-describing.
    #[serde(rename = "op")]
    pub op: ReplaceSetOp,
    pub table_group: ReplaceSetGroup,
    /// The scope this set replaces — a single `document_id`, optionally `(corpus, document_id)` for
    /// multi-corpus DBs (design §5.3 "Multi-corpus DBs").
    pub scope: ReplaceSetScope,
    /// The authoritative rebuilt row bodies, keyed by table name (`zone_units`,
    /// `zone_unit_embeddings`, or `chunks`/`chunk_embeddings`). A `BTreeMap` so serialization is
    /// key-ordered and canonicalisation is stable. This is the §5.3 `rows` object the applier
    /// delete-then-inserts; without it the package would carry only a digest descriptor and the
    /// applier would have nothing to insert.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub rows: std::collections::BTreeMap<String, Vec<serde_json::Value>>,
    /// Stable PKs of the rows in this set (`zone_unit_id`/`chunk_id`), kept so the digest is
    /// order-stable and the applier can validate membership.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub row_pks: Vec<String>,
    pub builder_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_text_hash: Option<String>,
    pub embedding_fingerprint: String,
    /// Deterministic hash over the ordered `(pk, row_hash)` pairs (the postcondition proof, §5.3).
    pub set_digest: String,
}

/// The deterministic `set_digest` over a replace-set's rows (plan P4 §5.3): `sha256` over the canonical
/// JSON of the per-table row map. The SINGLE definition both the producer (over the rows it ships) and
/// the consumer (over the generation rows it reads back post-apply) compute, so the applier can prove
/// the scope materialised to exactly the producer's set. The caller orders each table's rows by PK
/// before calling, so the digest is stable. `serde_json::Value` maps are key-ordered, so this is
/// canonical for fixed row order.
#[must_use]
pub fn set_digest_over_rows(
    rows: &std::collections::BTreeMap<String, Vec<serde_json::Value>>,
) -> String {
    let json = serde_json::to_string(rows).unwrap_or_default();
    crate::canonical::digest_bytes(json.as_bytes())
}

/// Marker for the `op` discriminant inside a [`ReplaceSet`] payload (always `replace_set`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceSetOp {
    ReplaceSet,
}

/// The scope a [`ReplaceSet`] replaces (design §5.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceSetScope {
    pub document_id: String,
    /// Present only for multi-corpus DBs, where the scope key is `(corpus, document_id)`. Typed as a
    /// validated [`Corpus`] so this package-facing field cannot carry an unvalidated token.
    ///
    /// [`Corpus`]: crate::corpus::Corpus
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus: Option<crate::corpus::Corpus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_wire_tokens_match_outbox_check() {
        assert_eq!(EventKind::Upsert.as_str(), "upsert");
        assert_eq!(EventKind::Delete.as_str(), "delete");
        assert_eq!(EventKind::ReplaceSet.as_str(), "replace_set");
        for kind in EventKind::all() {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
    }

    #[test]
    fn only_chunks_with_embeddings_rebuilds_bm25_rows() {
        assert!(ReplaceSetGroup::ChunksWithEmbeddings.rebuilds_bm25_rows());
        assert!(!ReplaceSetGroup::ChunkEmbeddings.rebuilds_bm25_rows());
        assert!(!ReplaceSetGroup::ZoneUnits.rebuilds_bm25_rows());
    }

    #[test]
    fn replace_set_round_trips_the_design_example_including_row_bodies() {
        // The §5.3 zone-units example, with the `rows` object the applier delete-then-inserts.
        let payload = ReplaceSet {
            op: ReplaceSetOp::ReplaceSet,
            table_group: ReplaceSetGroup::ZoneUnits,
            scope: ReplaceSetScope {
                document_id: "cass:JURITEXT000000000001".to_owned(),
                corpus: None,
            },
            rows: std::collections::BTreeMap::from([
                (
                    "zone_units".to_owned(),
                    vec![serde_json::json!({"zone_unit_id": "zu-1", "body": "…"})],
                ),
                (
                    "zone_unit_embeddings".to_owned(),
                    vec![serde_json::json!({"zone_unit_id": "zu-1", "embedding": [0.1, 0.2]})],
                ),
            ]),
            row_pks: vec!["zu-1".to_owned(), "zu-2".to_owned()],
            builder_version: "zone-unit-builder-vN".to_owned(),
            source_text_hash: Some("sha256:abc".to_owned()),
            embedding_fingerprint: "bge-m3:1024:cls:normalize=true".to_owned(),
            set_digest: "sha256:deadbeef".to_owned(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["op"], "replace_set");
        assert_eq!(json["table_group"], "zone_units");
        // The row bodies survive the round trip (the BLOCKER-1 regression guard).
        assert_eq!(json["rows"]["zone_units"][0]["zone_unit_id"], "zu-1");
        let restored: ReplaceSet = serde_json::from_value(json).unwrap();
        assert_eq!(restored, payload);
        assert_eq!(restored.rows["zone_unit_embeddings"].len(), 1);
    }

    #[test]
    fn replace_set_scope_corpus_is_validated() {
        let json = serde_json::json!({
            "op": "replace_set",
            "table_group": "zone_units",
            "scope": {"document_id": "cass:X", "corpus": "core"},
            "builder_version": "v1",
            "embedding_fingerprint": "fp",
            "set_digest": "sha256:0"
        });
        let restored: ReplaceSet = serde_json::from_value(json).unwrap();
        assert_eq!(restored.scope.corpus.unwrap().as_str(), "core");

        // An invalid corpus token is rejected on deserialize (validation flows through `Corpus`).
        let bad = serde_json::json!({
            "op": "replace_set",
            "table_group": "zone_units",
            "scope": {"document_id": "cass:X", "corpus": "BAD CORPUS"},
            "builder_version": "v1",
            "embedding_fingerprint": "fp",
            "set_digest": "sha256:0"
        });
        assert!(serde_json::from_value::<ReplaceSet>(bad).is_err());
    }
}
