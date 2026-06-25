//! Retrieval query/SQL surface. This module root keeps the shared runtime imports and the
//! re-exports; per-response SQL emitters live in submodules that pull the shared scope via
//! `use super::*`. The `pub(crate)` re-exports expose the shared query types and SQL helpers
//! to `crate::zone_retrieval` (which builds the parallel zone index on the same primitives).

use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

mod citation;
mod context;
mod fetch;
mod hybrid;
mod related;
mod sql;
mod stats;
mod types;
mod versions;

pub use citation::resolve_legi_citation_json;
pub use context::context_documents_json;
pub use fetch::fetch_documents_json;
pub use hybrid::hybrid_candidates_json;
pub use related::related_neighbours_json;
pub use stats::{corpus_source_coverage_json, corpus_stats_json, inspect_document_json};
pub use types::{
    CitationResolutionQuery, ContextDocumentsQuery, DecisionFilters, FetchDocumentsQuery, GroupBy,
    HybridCandidateQuery, RelatedQuery, RelatedRelation, RetrievalCursor, RetrievalMode,
    RetrievalOptions, rrf_weights,
};
pub use versions::{document_diff_json, document_versions_json};

// Shared query types + SQL helpers used by `crate::zone_retrieval` and across the submodules.
pub(crate) use sql::*;
pub(crate) use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    // T1.3: the helpers extracted to pub(crate) for the zone retrieval path must behave identically to
    // the prior inline logic (the isolation invariant — no change to default retrieval).
    #[test]
    fn effective_probes_defaults_to_four_else_override() {
        assert_eq!(effective_probes(&RetrievalOptions::default()), 4);
        assert_eq!(
            effective_probes(&RetrievalOptions {
                ivfflat_probes: Some(13),
                ..RetrievalOptions::default()
            }),
            13
        );
    }

    #[test]
    fn effective_rrf_weights_uses_defaults_else_override() {
        // SAFETY: tests in this module are the only readers of these env vars here; clear to read the
        // compiled defaults deterministically.
        unsafe {
            std::env::remove_var("JURISEARCH_RRF_LEXICAL_WEIGHT");
            std::env::remove_var("JURISEARCH_RRF_DENSE_WEIGHT");
        }
        assert_eq!(
            effective_rrf_weights(&RetrievalOptions::default()),
            (DEFAULT_RRF_LEXICAL_WEIGHT, DEFAULT_RRF_DENSE_WEIGHT)
        );
        assert_eq!(
            effective_rrf_weights(&RetrievalOptions {
                rrf_lexical_weight: Some(2.0),
                rrf_dense_weight: Some(0.5),
                ..RetrievalOptions::default()
            }),
            (2.0, 0.5)
        );
    }

    #[test]
    fn format_sql_f64_is_plain_and_clamped() {
        assert_eq!(format_sql_f64(RRF_K), "60.000000");
        assert_eq!(format_sql_f64(0.3), "0.300000");
        assert_eq!(format_sql_f64(-1.0), "0.000000");
        assert_eq!(format_sql_f64(f64::INFINITY), "0.000000");
    }
}
