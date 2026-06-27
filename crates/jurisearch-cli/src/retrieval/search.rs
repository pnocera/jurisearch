//! search command: hybrid/bm25/dense retrieval, citation routing, pagination cursors. The
//! response-construction engine (intent routing, structured-citation resolution, authority re-rank, the
//! response envelope) moved to `jurisearch_query::build_search` (work/09 P4-4B). This is the thin CLI
//! adapter: boundary validation, zone routing, authority preconditions, cursor parsing, lexical
//! pre-tokenization, the readiness gate, the index open, and the (lazy) query embedder. The
//! boundary→`SearchInput` resolution ([`resolve_search_input`]) is shared with the site search handler.

use std::cell::OnceCell;

use jurisearch_query::{
    QueryEmbedder, QueryEmbedding, SearchDecisionFilters, SearchInput, build_search,
};
use jurisearch_storage::query::QueryStore;

use crate::*;

// The pure search helpers now live in `jurisearch-query`; re-export the ones the zone path and eval
// runners use so their `crate::{…}` references keep resolving.
pub(crate) use jurisearch_query::{
    ParsedSearchCursor, parse_search_cursor, search_pagination_value,
};
// `is_iso_date` / `legi_citation_routing` are exercised by the CLI routing unit tests (via `crate::*`);
// the production routing that used them now lives in `build_search`, so they are test-only here.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use jurisearch_query::{is_iso_date, legi_citation_routing};

pub(crate) fn emit_search(req: SearchRequest) -> anyhow::Result<()> {
    match search_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn search_payload(req: SearchRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths (and the site handler), run BEFORE
    // zone routing so a zone search is validated identically.
    validate_search_common(&req)?;
    // Explicit opt-in: --zone routes to the parallel official-zone subsystem (Cassation-only), which
    // bypasses the chunk readiness gate and uses its own zone index. Absent --zone, behaviour is
    // byte-identical to the whole-decision search below.
    if let Some(zone) = req.zone {
        return zone_search_payload(req, zone);
    }
    let input = resolve_search_input(&req)?;
    let index_dir = require_existing_index_dir(req.index_dir.as_deref())?;
    let postgres = open_index(index_dir.as_path())?;
    run_search_with_input(&postgres, &input, true, None)
}

/// The search validation common to the zone and whole-decision paths (and the site handler): a
/// non-empty query, `top_k >= 1`, and well-formed retrieval-tuning options. Runs before zone routing.
pub(crate) fn validate_search_common(req: &SearchRequest) -> Result<(), ErrorObject> {
    if req.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("search query must not be empty"));
    }
    if req.top_k == 0 {
        return Err(ErrorObject::bad_input("search --top-k must be at least 1"));
    }
    validate_retrieval_options(&req.retrieval_options())?;
    Ok(())
}

/// Resolve a NON-ZONE search request into the side-effect-free builder's [`SearchInput`]: authority
/// preconditions, mode/format, cursor parsing (boundary validation), lexical pre-tokenization with the
/// "at least one searchable token" precedence, and the kind/as-of/filter resolution. Assumes
/// [`validate_search_common`] already ran. Shared by the CLI one-shot path and the site search handler,
/// so both apply byte-identical boundary rules; neither opens an index here.
pub(crate) fn resolve_search_input(req: &SearchRequest) -> Result<SearchInput, ErrorObject> {
    // Authority routing (non-zone main path only — the zone path implies decisions and gates itself).
    // `0.0`/unset is inert (`effective_authority_weight` is None), so these rejections never fire OFF.
    if effective_authority_weight(&req.retrieval_options()).is_some() {
        if !matches!(req.kind, CliKind::Decision) {
            return Err(ErrorObject::bad_input(
                "--authority-weight re-ranks jurisprudence only; rerun with --kind decision (or use --zone)",
            ));
        }
        if req.cursor.is_some() {
            return Err(ErrorObject::bad_input(
                "--authority-weight is first-page-only and cannot be combined with --cursor; omit the cursor to get the authority-ranked first page",
            ));
        }
    }
    let retrieval_mode: RetrievalMode = req.mode.into();
    let output_format: OutputFormat = req.format.into();
    let after_cursor = req
        .cursor
        .as_deref()
        .map(|cursor| parse_search_cursor(cursor, req.group_by.into()))
        .transpose()?;
    let normalized_query_text = parade_query_text(&req.query);
    let query_text = if retrieval_mode.uses_lexical() {
        normalized_query_text.ok_or_else(|| {
            ErrorObject::bad_input("search query must contain at least one searchable token")
        })?
    } else if normalized_query_text.is_none() {
        return Err(ErrorObject::bad_input(
            "search query must contain at least one searchable token",
        ));
    } else {
        req.query.trim().to_owned()
    };
    let kind: LegalKind = req.kind.into();
    Ok(make_search_input(
        req,
        retrieval_mode,
        output_format,
        after_cursor,
        query_text,
        kind,
    ))
}

/// Assemble the [`SearchInput`] from a request and its already-resolved retrieval parameters. Pure (no
/// I/O); shared by [`resolve_search_input`] and the eval `search_with_postgres` entry.
fn make_search_input(
    req: &SearchRequest,
    retrieval_mode: RetrievalMode,
    output_format: OutputFormat,
    after_cursor: Option<ParsedSearchCursor>,
    query_text: String,
    kind: LegalKind,
) -> SearchInput {
    let kind_filter = match kind {
        LegalKind::Code => Some("article"),
        LegalKind::Decision => Some("decision"),
        LegalKind::All => None,
    };
    SearchInput {
        query: req.query.clone(),
        query_text,
        retrieval_mode,
        group_by: req.group_by.into(),
        output_format,
        top_k: req.top_k,
        cursor_input: req.cursor.clone(),
        after_cursor,
        as_of: req.as_of.clone().unwrap_or_else(today_utc),
        kind_filter,
        options: req.retrieval_options(),
        decision_filters: SearchDecisionFilters {
            jurisdiction: req.court.clone(),
            formation: req.formation.clone(),
            publication: req.publication.clone(),
            decided_from: req.decided_from.clone(),
            decided_to: req.decided_to.clone(),
        },
        authority_weight: effective_authority_weight(&req.retrieval_options()),
    }
}

/// A `jurisearch_query::QueryEmbedder` that builds the heavy `PreparedQueryEmbedder` LAZILY (or reuses a
/// batch caller's). `build_search` only calls `embed` on the hybrid candidate path, NOT for a
/// structured-citation hit or a bm25 request — so wrapping the embedder this way preserves the prior
/// behaviour where a citation-routed query (even in Hybrid mode) never constructs the embedder or probes
/// the embedding endpoint. A `None` reused embedder builds one from the environment on first `embed`.
struct LazyQueryEmbedder<'a> {
    reused: Option<&'a PreparedQueryEmbedder>,
    built: OnceCell<PreparedQueryEmbedder>,
}

impl<'a> LazyQueryEmbedder<'a> {
    fn new(reused: Option<&'a PreparedQueryEmbedder>) -> Self {
        Self {
            reused,
            built: OnceCell::new(),
        }
    }
}

impl QueryEmbedder for LazyQueryEmbedder<'_> {
    fn embed(&self, text: &str) -> Result<QueryEmbedding, ErrorObject> {
        if let Some(embedder) = self.reused {
            return QueryEmbedder::embed(embedder, text);
        }
        if self.built.get().is_none() {
            let built = PreparedQueryEmbedder::from_env()?;
            // Single-threaded one-shot path; a redundant set (never happens here) would be ignored.
            let _ = self.built.set(built);
        }
        QueryEmbedder::embed(self.built.get().expect("just built"), text)
    }
}

/// Run a prepared search against an already-open managed index: the readiness gate (a pre-snapshot
/// precondition, keyed off the request's retrieval mode), one read snapshot, and the side-effect-free
/// builder with a lazily-constructed query embedder. Shared by the CLI one-shot path and the eval
/// `search_with_postgres` entry. (The site service has its own snapshot-bound readiness gate + store.)
fn run_search_with_input(
    postgres: &ManagedPostgres,
    input: &SearchInput,
    verify_readiness: bool,
    embedder: Option<&PreparedQueryEmbedder>,
) -> Result<Value, ErrorObject> {
    if verify_readiness {
        let readiness_gate = if input.retrieval_mode.uses_dense() {
            QueryReadinessGate::Search
        } else {
            QueryReadinessGate::SearchLexical
        };
        ensure_query_readiness(postgres, readiness_gate)?;
    }
    // work/09 P3B: the readiness gate is an adapter-side precondition (above, pre-snapshot); the request
    // then runs all reads — structured-citation resolution AND the hybrid fallback — through ONE read
    // snapshot, so a generation swap mid-request can never split the two reads across topologies.
    let lazy = LazyQueryEmbedder::new(embedder);
    let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
    build_search(input, &mut *snapshot, Some(&lazy as &dyn QueryEmbedder))
}

/// Run one search against an already-open index (the eval/batch entry). Split from `search_payload` so
/// a batch path can `open_index` once and run many queries under a single Postgres lifecycle. Query/kind
/// validation and index opening stay in `search_payload` (preserving error precedence). Builds the
/// `SearchInput` from the caller's explicit parameters, then defers to [`run_search_with_input`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn search_with_postgres(
    postgres: &ManagedPostgres,
    req: &SearchRequest,
    retrieval_mode: RetrievalMode,
    output_format: OutputFormat,
    after_cursor: Option<&ParsedSearchCursor>,
    query_text: &str,
    kind: LegalKind,
    // Whether to run the (relatively expensive) query-readiness coverage check. One-shot callers
    // pass `true`; a batch caller that already verified readiness once can pass `false` to avoid
    // re-counting coverage on every query.
    verify_readiness: bool,
    // A reused query embedder for batch callers. `None` builds a fresh one inline (one-shot path).
    embedder: Option<&PreparedQueryEmbedder>,
) -> Result<Value, ErrorObject> {
    let input = make_search_input(
        req,
        retrieval_mode,
        output_format,
        after_cursor.cloned(),
        query_text.to_owned(),
        kind,
    );
    run_search_with_input(postgres, &input, verify_readiness, embedder)
}
