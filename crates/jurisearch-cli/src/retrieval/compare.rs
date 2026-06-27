//! compare command: aligned bm25/dense/hybrid retriever comparison.

use jurisearch_query::{CompareInput, build_compare};
use jurisearch_storage::query::QueryStore;

use crate::*;

/// Run the same query through bm25/dense/hybrid (document grouping) and return aligned per-mode top-k
/// plus the pooled union with per-mode ranks and pairwise overlap. Single-page (no cursor): cross-mode
/// pagination has no honest shared meaning. The adapter validates + tokenizes + opens the store and
/// embedder; the side-effect-free `build_compare` runs the three arms over ONE read snapshot.
pub(crate) fn compare_payload(req: CompareRequest) -> Result<Value, ErrorObject> {
    let input = resolve_compare_input(&req)?;
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Search)?;
    let embedder = PreparedQueryEmbedder::from_env()?;
    let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
    build_compare(&input, &mut *snapshot, &embedder)
}

/// Resolve a `compare` request into the builder's [`CompareInput`]: validate the query + top_k,
/// pre-tokenize the lexical text, resolve as-of + the kind filter/label. Boundary only (no index, no
/// embedder). Shared by the CLI adapter and the site compare handler.
pub(crate) fn resolve_compare_input(req: &CompareRequest) -> Result<CompareInput, ErrorObject> {
    if req.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("compare requires a query"));
    }
    if req.top_k == 0 {
        return Err(ErrorObject::bad_input("compare --top-k must be at least 1"));
    }
    let kind: LegalKind = req.kind.into();
    let query_text = parade_query_text(&req.query).ok_or_else(|| {
        ErrorObject::bad_input("compare query must contain at least one searchable token")
    })?;
    let as_of = req.as_of.clone().unwrap_or_else(today_utc);
    let (kind_filter, kind_label) = match kind {
        LegalKind::Code => (Some("article"), "code"),
        LegalKind::Decision => (Some("decision"), "decision"),
        LegalKind::All => (None, "all"),
    };
    Ok(CompareInput {
        query: req.query.clone(),
        query_text,
        top_k: req.top_k,
        kind_filter,
        kind_label,
        as_of,
    })
}

pub(crate) fn emit_compare(req: CompareRequest) -> anyhow::Result<()> {
    match compare_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}
