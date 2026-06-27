//! `search` response construction (work/09 P4-4B): hybrid/bm25/dense retrieval, citation intent
//! routing (structured LEGI resolution with hybrid fallback), authority re-rank, and the response
//! envelope — all snapshot-bound and side-effect-free. The adapter (CLI or site handler) does the
//! boundary work (validation, cursor parsing, as_of/kind/authority resolution, lexical pre-tokenization,
//! the readiness gate, opening the snapshot + embedder) and calls [`build_search`]; this builder owns
//! the routing, ranking, and response shape so the CLI and site paths return byte-identical responses.

use jurisearch_core::contract::OutputFormat;
use jurisearch_core::error::ErrorObject;
use jurisearch_core::expand::expand_query;
use jurisearch_storage::authority::{
    AUTHORITY_DEFAULT_BAND, AUTHORITY_RERANK_WINDOW, authority_rerank,
};
use jurisearch_storage::query::ReadSnapshot;
use jurisearch_storage::retrieval::{
    CitationResolutionQuery, DecisionFilters, GroupBy, HybridCandidateQuery, RetrievalMode,
    RetrievalOptions, hybrid_candidates_in_snapshot, resolve_legi_citation_in_snapshot,
};
use serde_json::{Value, json};

use crate::cursor::ParsedSearchCursor;
use crate::embedder::QueryEmbedder;
use crate::errors::{dependency_unavailable, no_results, parse_storage_json, storage_error_object};
use crate::text::legi_citation_routing;

/// The shared `pagination` block (cursor note + guidance) used by both the whole-decision search and
/// the zone search, so the two surfaces stay consistent.
pub fn search_pagination_value(
    requested_top_k: u32,
    after_cursor: Option<&str>,
    returned: usize,
    cursor_supported: bool,
    next_cursor: Option<&str>,
) -> Value {
    let has_more = next_cursor.is_some();
    json!({
        "requested_top_k": requested_top_k,
        "after_cursor": after_cursor,
        "returned": returned,
        "possibly_truncated": has_more,
        "cursor_supported": cursor_supported,
        "next_cursor": next_cursor,
        "cursor_note": "Use next_cursor as --cursor on the CLI or cursor in session JSON to request the next page with the same query/filter inputs. Cursor paging walks the ranked relevance pool, not an exhaustive corpus scan.",
        "guidance": if has_more {
            Some("Use next_cursor as the next cursor value, or increase top_k (or --top-k on the CLI) to inspect a wider page.")
        } else {
            None
        }
    })
}

/// Decision metadata filters, owned (so [`SearchInput`] carries no borrowed lifetime). Borrowed into the
/// storage [`DecisionFilters`] inside [`build_search`].
#[derive(Debug, Default, Clone)]
pub struct SearchDecisionFilters {
    pub jurisdiction: Option<String>,
    pub formation: Option<String>,
    pub publication: Option<String>,
    pub decided_from: Option<String>,
    pub decided_to: Option<String>,
}

impl SearchDecisionFilters {
    fn as_storage(&self) -> DecisionFilters<'_> {
        DecisionFilters {
            jurisdiction: self.jurisdiction.as_deref(),
            formation: self.formation.as_deref(),
            publication: self.publication.as_deref(),
            decided_from: self.decided_from.as_deref(),
            decided_to: self.decided_to.as_deref(),
        }
    }
}

/// `search` input: everything the adapter validated/resolved at the boundary, in contract/storage
/// vocabulary. The builder never re-reads CLI defaults, env, or the index_dir.
pub struct SearchInput {
    /// The raw query (echoed; the source for embedding, expansion, and citation routing).
    pub query: String,
    /// The pre-tokenized BM25 lexical text (the adapter's `parade_query_text`).
    pub query_text: String,
    pub retrieval_mode: RetrievalMode,
    pub group_by: GroupBy,
    pub output_format: OutputFormat,
    pub top_k: u32,
    /// The raw cursor string the caller passed (echoed in pagination/diagnostics).
    pub cursor_input: Option<String>,
    /// The parsed cursor (adapter-side boundary validation), if any.
    pub after_cursor: Option<ParsedSearchCursor>,
    /// The resolved as-of date (request value or today).
    pub as_of: String,
    /// `Some("article")` / `Some("decision")` / `None`, from the resolved kind.
    pub kind_filter: Option<&'static str>,
    pub options: RetrievalOptions,
    pub decision_filters: SearchDecisionFilters,
    /// The pre-resolved effective authority weight (`Some(w>0)` enables the decision-only re-rank).
    pub authority_weight: Option<f64>,
}

/// Build the `search` response body: route a citation-shaped query to structured LEGI resolution (with
/// hybrid fallback) or conceptual/explicit-mode queries to hybrid retrieval, apply the authority
/// re-rank, and shape the response envelope (expansion/format/pagination/routing/diagnostics). Dense
/// retrieval pulls the embedding from `embedder`; a lexical-only request passes `None` (and never
/// embeds).
pub fn build_search(
    input: &SearchInput,
    snapshot: &mut dyn ReadSnapshot,
    embedder: Option<&dyn QueryEmbedder>,
) -> Result<Value, ErrorObject> {
    let mut execution = SearchExecution::new(input, snapshot, embedder);
    let routed = execution.run_structured_citation_or_fallback()?;
    execution.apply_search_response_envelope(routed)
}

/// The candidate-execution context for one search: borrowed inputs plus request-derived limits/filters.
/// Bundling them lets routing, candidate execution, and response shaping be small `&self`/`&mut self`
/// methods instead of one long function.
struct SearchExecution<'a> {
    input: &'a SearchInput,
    snapshot: &'a mut dyn ReadSnapshot,
    embedder: Option<&'a dyn QueryEmbedder>,
    kind_filter: Option<&'static str>,
    lexical_limit: u32,
    dense_limit: u32,
    query_limit: u32,
    /// The per-grouping clamped window factor used when authority is ON (`1` when OFF).
    window_factor: u32,
}

/// The routed candidate set plus its intent-routing audit labels, before response-envelope shaping.
struct RoutedSearch {
    response: Value,
    query_type: &'static str,
    chosen_backend: &'static str,
    fallback_path: &'static str,
}

impl<'a> SearchExecution<'a> {
    fn new(
        input: &'a SearchInput,
        snapshot: &'a mut dyn ReadSnapshot,
        embedder: Option<&'a dyn QueryEmbedder>,
    ) -> Self {
        let kind_filter = input.kind_filter;
        // Document grouping collapses many chunks per article, so overfetch a deeper pool to still
        // yield up to top_k UNIQUE documents (reported smaller only when the pool is exhausted).
        let pool_multiplier = match input.group_by {
            GroupBy::Document => 20,
            GroupBy::Chunk => 4,
        };
        let lexical_limit = input.top_k.saturating_mul(pool_multiplier);
        let dense_limit = input.top_k.saturating_mul(pool_multiplier);
        // Authority (A4): when ON, widen the fetched window to `top_k * W_eff` so the re-rank has a
        // deeper same-relevance pool. `W_eff` is clamped to the arm pool multiplier so the window can
        // never outrun the candidate set feeding RRF. OFF keeps today's exact `top_k + 1`.
        let window_factor = if input.authority_weight.is_some() {
            AUTHORITY_RERANK_WINDOW.min(pool_multiplier)
        } else {
            1
        };
        let query_limit = if input.authority_weight.is_some() {
            input.top_k.saturating_mul(window_factor).saturating_add(1)
        } else {
            input.top_k.saturating_add(1)
        };
        Self {
            input,
            snapshot,
            embedder,
            kind_filter,
            lexical_limit,
            dense_limit,
            query_limit,
            window_factor,
        }
    }

    /// Hybrid retrieval (embedding + BM25/dense/RRF). Run for conceptual queries, explicit bm25/dense
    /// modes, or as the fallback when structured citation resolution finds nothing.
    fn run_hybrid_candidates(&mut self) -> Result<Value, ErrorObject> {
        let (query_embedding, embedding_fingerprint) = if self.input.retrieval_mode.uses_dense() {
            let embedder = self.embedder.ok_or_else(|| {
                dependency_unavailable(
                    "dense retrieval requires a query embedder but none was provided",
                )
            })?;
            let embedding = embedder.embed(self.input.query.as_str())?;
            (Some(embedding.literal), Some(embedding.fingerprint))
        } else {
            (None, None)
        };
        let response = hybrid_candidates_in_snapshot(
            self.snapshot,
            &HybridCandidateQuery {
                query_text: self.input.query_text.as_str(),
                query_embedding: query_embedding.as_deref(),
                embedding_fingerprint: embedding_fingerprint.as_deref(),
                retrieval_mode: self.input.retrieval_mode,
                group_by: self.input.group_by,
                options: self.input.options,
                after_cursor: self
                    .input
                    .after_cursor
                    .as_ref()
                    .map(ParsedSearchCursor::as_retrieval_cursor),
                as_of: self.input.as_of.as_str(),
                kind_filter: self.kind_filter,
                project_authority: self.input.authority_weight.is_some(),
                decision_filters: self.input.decision_filters.as_storage(),
                lexical_limit: self.lexical_limit,
                dense_limit: self.dense_limit,
                limit: self.query_limit,
            },
        )
        .map_err(storage_error_object)?;
        parse_storage_json(&response)
    }

    /// Intent routing. A citation-shaped query (`Article <n>`) in Hybrid mode resolves structurally;
    /// a structured miss falls back to hybrid so a malformed citation still returns results. Conceptual
    /// queries and explicit bm25/dense modes go straight to hybrid.
    fn run_structured_citation_or_fallback(&mut self) -> Result<RoutedSearch, ErrorObject> {
        let citation_intent =
            legi_citation_routing(self.input.query.as_str(), self.input.as_of.as_str());
        let query_type = if citation_intent.is_some() {
            "citation"
        } else {
            "semantic"
        };
        let (response, chosen_backend, fallback_path) = match citation_intent {
            Some(parsed) if matches!(self.input.retrieval_mode, RetrievalMode::Hybrid) => {
                let structured = resolve_legi_citation_in_snapshot(
                    self.snapshot,
                    &CitationResolutionQuery {
                        query: parsed.citation_query.as_str(),
                        article_number: parsed.article_number.as_str(),
                        code_hint: parsed.code_hint.as_deref(),
                        as_of: parsed.as_of.as_str(),
                        kind_filter: self.kind_filter,
                        // Structured results have no pagination cursor; request exactly top_k so the
                        // response never reports a phantom truncation it cannot page past.
                        limit: self.input.top_k,
                    },
                )
                .map_err(storage_error_object)?;
                let structured: Value = parse_storage_json(&structured)?;
                let count = structured["candidates"].as_array().map_or(0, Vec::len);
                if count > 0 {
                    (structured, "structured_citation", "none")
                } else {
                    (
                        self.run_hybrid_candidates()?,
                        self.input.retrieval_mode.as_str(),
                        "hybrid_fallback",
                    )
                }
            }
            _ => (
                self.run_hybrid_candidates()?,
                self.input.retrieval_mode.as_str(),
                "none",
            ),
        };
        Ok(RoutedSearch {
            response,
            query_type,
            chosen_backend,
            fallback_path,
        })
    }

    /// Shape the routed candidate set into the final `SearchResponse`: expansion/format/limit
    /// decoration, top_k truncation + next-cursor, pagination block, the intent-routing audit, and
    /// (detailed only) diagnostics. Maps an empty candidate set to `no_results`.
    fn apply_search_response_envelope(&self, routed: RoutedSearch) -> Result<Value, ErrorObject> {
        let RoutedSearch {
            mut response,
            query_type,
            chosen_backend,
            fallback_path,
        } = routed;
        let routed_candidate_count = response["candidates"].as_array().map_or(0, Vec::len);
        let expansion = expand_query(self.input.query.as_str());
        response["format"] = json!(self.input.output_format.as_str());
        response["limit"] = json!(self.input.top_k);
        response["expansion_seed_version"] = json!(expansion.seed_version);
        response["expanded_terms"] = json!(expansion.expanded_terms);
        // Authority re-rank (A4): reorder the widened window by within-order publication authority
        // BEFORE truncation, so the most authoritative of the near-tied top results surfaces. Only on
        // the hybrid candidate path — structured citation results are an exact set with no ranking band.
        let mut authority_applied = false;
        if chosen_backend != "structured_citation"
            && let Some(weight) = self.input.authority_weight
            && let Some(candidates) = response["candidates"].as_array_mut()
        {
            authority_rerank(candidates, weight, AUTHORITY_DEFAULT_BAND);
            authority_applied = true;
        }
        let mut next_cursor = None;
        let top_k = self.input.top_k as usize;
        if let Some(candidates) = response["candidates"].as_array_mut()
            && candidates.len() > top_k
        {
            candidates.truncate(top_k);
            // Storage always projects a cursor; keep next_cursor tied to the last displayed row.
            next_cursor = candidates
                .last()
                .and_then(|candidate| candidate["cursor"].as_str().map(str::to_owned));
        }
        let returned = response["candidates"].as_array().map_or(0, Vec::len);
        // Structured citation results are an exact, fully-returned resolution set with no ranking
        // cursor. Authority re-rank is first-page-only in v1: it reorders rows away from SQL keyset
        // order, so no single (score,id) cursor can represent the page — disable paging for it too.
        let cursor_supported = chosen_backend != "structured_citation" && !authority_applied;
        if authority_applied {
            next_cursor = None;
        }
        response["pagination"] = search_pagination_value(
            self.input.top_k,
            self.input.cursor_input.as_deref(),
            returned,
            cursor_supported,
            next_cursor.as_deref(),
        );
        if authority_applied {
            response["pagination"]["cursor_note"] = json!(
                "Authority re-rank is first-page-only in v1: cursor paging is disabled for this \
                 response. Increase --top-k (or top_k in session JSON) to inspect a wider \
                 authority-ranked window."
            );
            response["authority"] = json!({
                "enabled": true,
                "weight": self.input.authority_weight,
                "window_factor": self.window_factor,
                "paging": "first_page_only",
            });
        }
        // Intent-routing audit: prove the resolver was used because the input is structurally
        // resolvable (query_type=citation, backend=structured_citation), not because the evaluator
        // knew the answer. Recorded on every search, structured and hybrid alike.
        response["routing"] = json!({
            "query_type": query_type,
            "chosen_backend": chosen_backend,
            "candidate_count": routed_candidate_count,
            "fallback_path": fallback_path,
        });
        if matches!(self.input.output_format, OutputFormat::Detailed) {
            response["diagnostics"] = json!({
                "query_input": self.input.query.clone(),
                "lexical_query_text": if self.input.retrieval_mode.uses_lexical() {
                    Some(self.input.query_text.as_str())
                } else {
                    None
                },
                "retrieval": {
                    "mode": self.input.retrieval_mode.as_str(),
                    "uses_lexical": self.input.retrieval_mode.uses_lexical(),
                    "uses_dense": self.input.retrieval_mode.uses_dense(),
                    "lexical_limit": self.lexical_limit,
                    "dense_limit": self.dense_limit,
                    "query_limit": self.query_limit,
                    "kind_filter": self.kind_filter,
                    "after_cursor": self.input.cursor_input.as_deref(),
                }
            });
        }
        if response["candidates"]
            .as_array()
            .is_some_and(|candidates| candidates.is_empty())
        {
            Err(no_results("search returned no candidates"))
        } else {
            Ok(response)
        }
    }
}
