//! Retrieval request wire types — the per-request retrieval vocabulary the wire contract carries
//! and `jurisearch-storage` interprets.
//!
//! `RetrievalMode`, `GroupBy`, and `RetrievalOptions` are **pure, immutable request state** (no SQL,
//! no lifetimes, no storage internals). They live here, in the dependency-light contract crate, so
//! there is **one** definition that both the query/thin-client side and `jurisearch-storage` depend
//! on (storage re-exports them from `jurisearch_storage::retrieval`). The storage-coupled query
//! structs (`HybridCandidateQuery`, `DecisionFilters`, cursors, the env/manifest interpretation of
//! these options, the SQL helpers) stay in `jurisearch-storage`.

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

/// Per-request retrieval tuning. `None` means "use the environment/default", so existing callers
/// are unaffected. Carried as immutable request state (NOT process env), so warm sessions and the
/// site query service can serve concurrent requests with different weights/probes deterministically.
///
/// How these overrides are interpreted against env/index-manifest defaults stays in
/// `jurisearch-storage` (`effective_rrf_weights`/`effective_probes`); only the *shape* lives here.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetrievalOptions {
    pub rrf_lexical_weight: Option<f64>,
    pub rrf_dense_weight: Option<f64>,
    pub ivfflat_probes: Option<u32>,
    /// Decision-only authority rerank weight (`jurisearch_storage::authority`). `None`/unset and
    /// `<= 0.0` are inert (the OFF path is byte-identical), so existing callers and
    /// `RetrievalOptions::default()` never trigger authority reranking. Only finite `> 0.0` enables
    /// it. No environment fallback in v1.
    pub authority_weight: Option<f64>,
}
