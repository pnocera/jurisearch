//! Retrieval query/option types (GroupBy/RetrievalMode/cursor, filters, query structs) + RRF weights.

use super::*;

// Dense ANN candidates are post-filtered by document validity, so fetch a
// wider pool before assigning gap-free dense ranks.
pub(super) const DENSE_TEMPORAL_OVERFETCH_FACTOR: u32 = 4;

pub(super) const DEFAULT_CONTEXT_SIBLING_LIMIT: u32 = 50;

// Reciprocal-rank-fusion constant and per-arm weights. LEGI has many near-duplicate sibling
// articles (same parent text, different article number) whose dense embeddings are nearly
// identical, so an equal-weight dense arm dilutes the much sharper BM25 ranking on exact-citation
// queries. The weights let the dense arm act as a recall-expander/tie-breaker rather than an equal
// vote; tune via env without recompiling. The default down-weights dense to 0.3 (BM25-favored).
pub(crate) const RRF_K: f64 = 60.0;

pub(super) const DEFAULT_RRF_LEXICAL_WEIGHT: f64 = 1.0;

// Dense down-weighted to a recall-expander/tie-breaker. France-LEGI calibration over the production
// index: equal weight (1.0) gave known-item recall@10 0.55; 0.3 lifts it to 0.60 with no temporal
// regression (0.75) and an immaterial cross-reference change. Lower still (0.15) trades temporal
// away for known-item. See reviews/2026-06-23-retrieval-fusion-*.
pub(super) const DEFAULT_RRF_DENSE_WEIGHT: f64 = 0.3;

/// `(lexical_weight, dense_weight)` for hybrid RRF, overridable via
/// `JURISEARCH_RRF_LEXICAL_WEIGHT` / `JURISEARCH_RRF_DENSE_WEIGHT` (finite, >= 0).
pub fn rrf_weights() -> (f64, f64) {
    fn weight(var: &str, default: f64) -> f64 {
        std::env::var(var)
            .ok()
            .and_then(|value| value.trim().parse::<f64>().ok())
            .filter(|weight| weight.is_finite() && *weight >= 0.0)
            .unwrap_or(default)
    }
    (
        weight("JURISEARCH_RRF_LEXICAL_WEIGHT", DEFAULT_RRF_LEXICAL_WEIGHT),
        weight("JURISEARCH_RRF_DENSE_WEIGHT", DEFAULT_RRF_DENSE_WEIGHT),
    )
}

/// Result granularity: one row per matching chunk, or one row per article (its best chunk).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Chunk,
    Document,
}

impl GroupBy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chunk => "chunk",
            Self::Document => "document",
        }
    }
}

/// Per-request retrieval tuning. `None` means "use the environment/default", so existing callers
/// are unaffected. Carried as immutable request state (NOT process env), so warm sessions and a
/// future server can serve concurrent requests with different weights/probes deterministically.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetrievalOptions {
    pub rrf_lexical_weight: Option<f64>,
    pub rrf_dense_weight: Option<f64>,
    pub ivfflat_probes: Option<u32>,
    /// Decision-only authority rerank weight (`crate::authority`). `None`/unset and `<= 0.0` are inert
    /// (the OFF path is byte-identical), so existing callers and `RetrievalOptions::default()` never
    /// trigger authority reranking. Only finite `> 0.0` enables it. No environment fallback in v1.
    pub authority_weight: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
pub struct HybridCandidateQuery<'a> {
    pub query_text: &'a str,
    pub query_embedding: Option<&'a str>,
    pub embedding_fingerprint: Option<&'a str>,
    pub retrieval_mode: RetrievalMode,
    pub group_by: GroupBy,
    pub options: RetrievalOptions,
    pub after_cursor: Option<RetrievalCursor<'a>>,
    pub as_of: &'a str,
    pub kind_filter: Option<&'a str>,
    /// Decision-metadata filters (court/formation/publication/decision-date). Empty by default.
    pub decision_filters: DecisionFilters<'a>,
    /// Gate (A2): when `true`, project `canonical_json->>'publication'` into the candidate JSON so the
    /// authority re-rank (A4) can read it. When `false` (every OFF caller), the emitted SQL and payload
    /// are byte-identical to before this field existed — no `publication` column, no JSON key.
    pub project_authority: bool,
    pub lexical_limit: u32,
    pub dense_limit: u32,
    pub limit: u32,
}

/// Optional jurisprudence-decision metadata filters applied alongside `kind_filter`. All `None` is a
/// no-op (matches the prior behaviour), so existing call sites use `DecisionFilters::default()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DecisionFilters<'a> {
    /// Court / `JURIDICTION` substring (case-insensitive).
    pub jurisdiction: Option<&'a str>,
    /// Chamber / `FORMATION` substring (case-insensitive).
    pub formation: Option<&'a str>,
    /// Publication level (`PUBLI_BULL@publie` / `PUBLI_RECUEIL`), exact (case-insensitive).
    pub publication: Option<&'a str>,
    /// Decision date lower bound (inclusive, ISO `YYYY-MM-DD`), against `valid_from`.
    pub decided_from: Option<&'a str>,
    /// Decision date upper bound (inclusive, ISO `YYYY-MM-DD`), against `valid_from`.
    pub decided_to: Option<&'a str>,
}

impl DecisionFilters<'_> {
    fn is_empty(&self) -> bool {
        self.jurisdiction.is_none()
            && self.formation.is_none()
            && self.publication.is_none()
            && self.decided_from.is_none()
            && self.decided_to.is_none()
    }

    /// Build the SQL predicate fragment (each clause prefixed with `AND`) against `documents d`.
    /// All values pass through `sql_string_literal`, so this is injection-safe.
    ///
    /// Any non-empty filter implies `d.kind = 'decision'`: these are decision-metadata filters, so
    /// they must never silently re-interpret a statute search (e.g. a decision-date bound must not
    /// filter LEGI articles by their version start). Court/formation/publication already self-scope
    /// via the JSON metadata only decisions carry; the kind guard makes the date bounds consistent.
    pub(crate) fn predicate(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut predicate = String::from(" AND d.kind = 'decision'");
        if let Some(jurisdiction) = self.jurisdiction {
            predicate.push_str(&format!(
                " AND d.canonical_json->>'jurisdiction' ILIKE {}",
                sql_string_literal(&format!("%{jurisdiction}%"))
            ));
        }
        if let Some(formation) = self.formation {
            predicate.push_str(&format!(
                " AND d.canonical_json->>'formation' ILIKE {}",
                sql_string_literal(&format!("%{formation}%"))
            ));
        }
        if let Some(publication) = self.publication {
            predicate.push_str(&format!(
                " AND lower(d.canonical_json->>'publication') = lower({})",
                sql_string_literal(publication)
            ));
        }
        if let Some(decided_from) = self.decided_from {
            predicate.push_str(&format!(
                " AND d.valid_from >= {}::date",
                sql_string_literal(decided_from)
            ));
        }
        if let Some(decided_to) = self.decided_to {
            predicate.push_str(&format!(
                " AND d.valid_from <= {}::date",
                sql_string_literal(decided_to)
            ));
        }
        predicate
    }
}

/// Effective `(lexical_weight, dense_weight)` for a request: per-request override else env/default.
/// Shared by `hybrid_candidates_json` and the parallel zone retrieval path (`zone_retrieval.rs`).
pub(crate) fn effective_rrf_weights(options: &RetrievalOptions) -> (f64, f64) {
    let (lexical, dense) = rrf_weights();
    (
        options.rrf_lexical_weight.unwrap_or(lexical),
        options.rrf_dense_weight.unwrap_or(dense),
    )
}

/// Effective ivfflat probes for a request: per-request override else the default 4. Shared with the
/// zone retrieval path.
pub(crate) fn effective_probes(options: &RetrievalOptions) -> u32 {
    options.ivfflat_probes.unwrap_or(4)
}

impl HybridCandidateQuery<'_> {
    pub(super) fn effective_rrf_weights(&self) -> (f64, f64) {
        effective_rrf_weights(&self.options)
    }

    pub(super) fn effective_probes(&self) -> u32 {
        effective_probes(&self.options)
    }
}

/// An opaque pagination cursor, tagged by grouping. A chunk cursor is `<score>:<chunk_id>`; a
/// document cursor is `doc:<score>:<document_id>`. The tag lets us reject a cursor from the wrong
/// grouping instead of silently mis-paging.
#[derive(Debug, Clone, Copy)]
pub enum RetrievalCursor<'a> {
    Chunk {
        score: &'a str,
        chunk_id: &'a str,
    },
    Document {
        score: &'a str,
        document_id: &'a str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrievalMode {
    Hybrid,
    Bm25,
    Dense,
}

impl RetrievalMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::Bm25 => "bm25",
            Self::Dense => "dense",
        }
    }

    pub fn uses_lexical(self) -> bool {
        matches!(self, Self::Hybrid | Self::Bm25)
    }

    pub fn uses_dense(self) -> bool {
        matches!(self, Self::Hybrid | Self::Dense)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FetchDocumentsQuery<'a> {
    pub document_ids: &'a [&'a str],
}

#[derive(Debug, Clone, Copy)]
pub struct ContextDocumentsQuery<'a> {
    pub document_id: &'a str,
    pub as_of: Option<&'a str>,
    pub include_siblings: bool,
}

/// A typed graph relation served by `related`. All are depth-1 publisher edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelatedRelation {
    /// Outgoing official citations (this article cites …).
    Cites,
    /// Incoming official citations (… cites this article).
    CitedBy,
    /// Version-family members (LIEN_ART version-list edges).
    Temporal,
    /// Decisions that officially cite this article (statute → interpreting jurisprudence). Like
    /// `cited_by` but restricted to `kind = 'decision'` neighbours. (`cites` from a decision seed
    /// already returns the articles it applies — the inverse direction.)
    InterpretedBy,
}

impl RelatedRelation {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cites" => Some(Self::Cites),
            "cited_by" => Some(Self::CitedBy),
            "temporal" => Some(Self::Temporal),
            "interpreted_by" => Some(Self::InterpretedBy),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cites => "cites",
            Self::CitedBy => "cited_by",
            Self::Temporal => "temporal",
            Self::InterpretedBy => "interpreted_by",
        }
    }

    pub(super) fn direction(self) -> &'static str {
        match self {
            Self::Cites | Self::Temporal => "outgoing",
            Self::CitedBy | Self::InterpretedBy => "incoming",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RelatedQuery<'a> {
    pub document_id: &'a str,
    pub rel: RelatedRelation,
    pub limit: u32,
}

/// Inputs for structured citation resolution: an article reference parsed out of a citation-shaped
/// query, plus the as-of date that pins the version.
#[derive(Debug, Clone, Copy)]
pub struct CitationResolutionQuery<'a> {
    /// Echoed back in the response `query` field.
    pub query: &'a str,
    /// The article number/identifier (e.g. `33`, `R242-40`), matched as `%article <n>%`.
    pub article_number: &'a str,
    /// Optional disambiguating text (the parent text/code title), matched as `%<hint>%`.
    pub code_hint: Option<&'a str>,
    /// Validity anchor: only articles valid at this date are returned (one version per article).
    pub as_of: &'a str,
    pub kind_filter: Option<&'a str>,
    pub limit: u32,
}
