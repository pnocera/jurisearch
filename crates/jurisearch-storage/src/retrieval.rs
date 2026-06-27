//! Retrieval query/SQL surface. This module root keeps the shared runtime imports and the
//! re-exports; per-response SQL emitters live in submodules that pull the shared scope via
//! `use super::*`. The `pub(crate)` re-exports expose the shared query types and SQL helpers
//! to `crate::zone_retrieval` (which builds the parallel zone index on the same primitives).

use crate::query::ReadSnapshot;
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

// work/09 P3B — the snapshot-bound retrieval cores (one request = one read snapshot). The response
// builders (`jurisearch-query`) call these directly with a shared [`ReadSnapshot`]. The legacy
// `*_json(&ManagedPostgres, …)` wrappers below open a one-shot snapshot and delegate, so deferred
// callers (the eval harness, local diagnostics, integration tests) keep their existing call shape.
pub use citation::resolve_legi_citation_in_snapshot;
pub use context::context_documents_in_snapshot;
pub use fetch::fetch_documents_in_snapshot;
pub use hybrid::hybrid_candidates_in_snapshot;
pub use related::related_neighbours_in_snapshot;
pub use stats::{corpus_source_coverage_json, corpus_stats_json, inspect_document_json};
pub use types::{
    CitationResolutionQuery, ContextDocumentsQuery, DecisionFilters, FetchDocumentsQuery, GroupBy,
    HybridCandidateQuery, RelatedQuery, RelatedRelation, RetrievalCursor, RetrievalMode,
    RetrievalOptions, rrf_weights,
};
pub use versions::{document_diff_json, document_versions_json};

use crate::query::QueryStore;

/// Legacy one-shot wrapper: open a read snapshot and run [`fetch_documents_in_snapshot`]. Prefer the
/// snapshot-bound core (one shared snapshot per request) on the query path; this exists for deferred
/// callers that hold a [`ManagedPostgres`] and read a single response.
pub fn fetch_documents_json(
    postgres: &ManagedPostgres,
    query: &FetchDocumentsQuery<'_>,
) -> Result<String, StorageError> {
    let mut snapshot = postgres.begin_snapshot()?;
    fetch_documents_in_snapshot(&mut *snapshot, query)
}

/// Legacy one-shot wrapper over [`context_documents_in_snapshot`] (see [`fetch_documents_json`]).
pub fn context_documents_json(
    postgres: &ManagedPostgres,
    query: &ContextDocumentsQuery<'_>,
) -> Result<String, StorageError> {
    let mut snapshot = postgres.begin_snapshot()?;
    context_documents_in_snapshot(&mut *snapshot, query)
}

/// Legacy one-shot wrapper over [`related_neighbours_in_snapshot`] (see [`fetch_documents_json`]).
pub fn related_neighbours_json(
    postgres: &ManagedPostgres,
    query: &RelatedQuery<'_>,
) -> Result<String, StorageError> {
    let mut snapshot = postgres.begin_snapshot()?;
    related_neighbours_in_snapshot(&mut *snapshot, query)
}

/// Legacy one-shot wrapper over [`resolve_legi_citation_in_snapshot`] (see [`fetch_documents_json`]).
pub fn resolve_legi_citation_json(
    postgres: &ManagedPostgres,
    query: &CitationResolutionQuery<'_>,
) -> Result<String, StorageError> {
    let mut snapshot = postgres.begin_snapshot()?;
    resolve_legi_citation_in_snapshot(&mut *snapshot, query)
}

/// Legacy one-shot wrapper over [`hybrid_candidates_in_snapshot`] (see [`fetch_documents_json`]).
pub fn hybrid_candidates_json(
    postgres: &ManagedPostgres,
    query: &HybridCandidateQuery<'_>,
) -> Result<String, StorageError> {
    let mut snapshot = postgres.begin_snapshot()?;
    hybrid_candidates_in_snapshot(&mut *snapshot, query)
}

// Shared query types + SQL helpers used by `crate::zone_retrieval` and across the submodules.
pub(crate) use sql::*;
pub(crate) use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    // T1.3: the helpers extracted to pub(crate) for the zone retrieval path must behave identically to
    // the prior inline logic (the isolation invariant — no change to default retrieval).
    #[test]
    fn effective_probes_prefers_override_then_stored_then_fixed_default() {
        // No override, no stored recommendation → the conservative fixed fallback.
        assert_eq!(
            effective_probes(&RetrievalOptions::default(), None),
            DEFAULT_IVFFLAT_PROBES
        );
        // No override, but a manifest recommendation → the corpus-sized stored value.
        assert_eq!(effective_probes(&RetrievalOptions::default(), Some(47)), 47);
        // An explicit `--probes` override always wins over the stored recommendation.
        assert_eq!(
            effective_probes(
                &RetrievalOptions {
                    ivfflat_probes: Some(13),
                    ..RetrievalOptions::default()
                },
                Some(47)
            ),
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
