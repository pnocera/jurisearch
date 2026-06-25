//! search command: hybrid/bm25/dense retrieval, citation routing, pagination cursors.

use crate::*;

pub(crate) fn emit_search(req: SearchRequest) -> anyhow::Result<()> {
    match search_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

/// The shared `pagination` block (cursor note + guidance) used by both the whole-decision search and
/// the zone search, so the two surfaces stay consistent.
pub(crate) fn search_pagination_value(
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

pub(crate) fn search_payload(req: SearchRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths (previously duplicated in
    // dispatch::run and session::session_search_payload). The one-shot strings are canonical.
    if req.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("search query must not be empty"));
    }
    if req.top_k == 0 {
        return Err(ErrorObject::bad_input("search --top-k must be at least 1"));
    }
    validate_retrieval_options(&req.retrieval_options())?;
    // Explicit opt-in: --zone routes to the parallel official-zone subsystem (Cassation-only), which
    // bypasses the chunk readiness gate and uses its own zone index. Absent --zone, behaviour is
    // byte-identical to the whole-decision search below.
    if let Some(zone) = req.zone {
        return zone_search_payload(req, zone);
    }
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
        .map(|cursor| parse_search_cursor(cursor, req.group_by))
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
    let index_dir = require_existing_index_dir(req.index_dir.as_deref())?;
    let kind: LegalKind = req.kind.into();

    let postgres = open_index(index_dir.as_path())?;
    search_with_postgres(
        &postgres,
        &req,
        retrieval_mode,
        output_format,
        after_cursor.as_ref(),
        &query_text,
        kind,
        true,
        None,
    )
}

/// A citation-shaped query parsed for structured resolution: an `Article <n>` reference plus the
/// as-of date that pins the version (from an `en vigueur au <date>` suffix, else the caller default).
pub(crate) struct LegiCitationRouting {
    /// The citation text with any `en vigueur au <date>` suffix stripped, used for the resolver's
    /// exact-citation-match ranking (so a temporal query still matches the stored citation).
    pub(crate) citation_query: String,
    pub(crate) article_number: String,
    pub(crate) code_hint: Option<String>,
    pub(crate) as_of: String,
}

/// Classify a query for intent routing. Returns `Some` when the query is a citation-shaped LEGI
/// lookup (contains an `Article <n>` reference, optionally with an `en vigueur au <date>` temporal
/// suffix) — those route to structured citation resolution. `None` means a conceptual query that
/// goes to hybrid semantic search. This classification is production-visible (the shared search
/// path), so the gate measures the same routing users hit.
pub(crate) fn legi_citation_routing(
    query: &str,
    default_as_of: &str,
) -> Option<LegiCitationRouting> {
    const EN_VIGUEUR: &str = " en vigueur au ";
    let (article_part, as_of) = match find_ascii_ci(query, EN_VIGUEUR) {
        Some(idx) => {
            let after = query[idx + EN_VIGUEUR.len()..].trim();
            let date = after.split_whitespace().next().unwrap_or(after);
            let as_of = if is_iso_date(date) {
                date.to_owned()
            } else {
                default_as_of.to_owned()
            };
            (query[..idx].trim(), as_of)
        }
        None => (query.trim(), default_as_of.to_owned()),
    };
    const ARTICLE: &str = "article ";
    let pos = rfind_ascii_ci(article_part, ARTICLE)?;
    let article_number = article_part[pos + ARTICLE.len()..].trim();
    if article_number.is_empty() {
        return None;
    }
    let code_hint = article_part[..pos].trim();
    Some(LegiCitationRouting {
        citation_query: article_part.to_owned(),
        article_number: article_number.to_owned(),
        code_hint: (!code_hint.is_empty()).then(|| code_hint.to_owned()),
        as_of,
    })
}

pub(crate) fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, &byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// Run one search against an already-open index. Split out from `search_payload` so an
/// eval/batch path can `open_index` once and run many queries under a single Postgres lifecycle
/// (avoiding per-query cold starts). Query/kind validation and index opening stay in
/// `search_payload` to preserve error precedence (an unsearchable query reports `bad_input`
/// before any index check).
///
/// Intent routing: a citation-shaped query (`Article <n>`, optionally `en vigueur au <date>`) in
/// Hybrid mode resolves structurally (exact article + as-of validity window); conceptual queries
/// and explicit bm25/dense modes use hybrid search. A `routing` object records the audit trail.
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
    if verify_readiness {
        let readiness_gate = if retrieval_mode.uses_dense() {
            QueryReadinessGate::Search
        } else {
            QueryReadinessGate::SearchLexical
        };
        ensure_query_readiness(postgres, readiness_gate)?;
    }
    let execution = SearchExecution::new(
        postgres,
        req,
        retrieval_mode,
        query_text,
        after_cursor,
        kind,
        embedder,
    );
    let routed = execution.run_structured_citation_or_fallback()?;
    execution.apply_search_response_envelope(routed, output_format)
}

/// The candidate-execution context for one search against an already-open index: the borrowed
/// inputs plus the request-derived limits/filters. Bundling them lets the routing, candidate
/// execution, and response-shaping steps be small `&self` methods instead of one long function,
/// without threading a dozen locals through each. Postgres stays concrete (no retriever trait).
struct SearchExecution<'a> {
    postgres: &'a ManagedPostgres,
    req: &'a SearchRequest,
    retrieval_mode: RetrievalMode,
    query_text: &'a str,
    after_cursor: Option<&'a ParsedSearchCursor>,
    embedder: Option<&'a PreparedQueryEmbedder>,
    as_of: String,
    kind_filter: Option<&'static str>,
    group_by: GroupBy,
    lexical_limit: u32,
    dense_limit: u32,
    query_limit: u32,
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
        postgres: &'a ManagedPostgres,
        req: &'a SearchRequest,
        retrieval_mode: RetrievalMode,
        query_text: &'a str,
        after_cursor: Option<&'a ParsedSearchCursor>,
        kind: LegalKind,
        embedder: Option<&'a PreparedQueryEmbedder>,
    ) -> Self {
        let as_of = req.as_of.clone().unwrap_or_else(today_utc);
        let kind_filter = match kind {
            LegalKind::Code => Some("article"),
            LegalKind::Decision => Some("decision"),
            LegalKind::All => None,
        };
        // Document grouping collapses many chunks per article, so overfetch a deeper pool to still
        // yield up to top_k UNIQUE documents (reported smaller only when the pool is exhausted).
        let group_by: GroupBy = req.group_by.into();
        let pool_multiplier = match group_by {
            GroupBy::Document => 20,
            GroupBy::Chunk => 4,
        };
        let lexical_limit = req.top_k.saturating_mul(pool_multiplier);
        let dense_limit = req.top_k.saturating_mul(pool_multiplier);
        let query_limit = req.top_k.saturating_add(1);
        Self {
            postgres,
            req,
            retrieval_mode,
            query_text,
            after_cursor,
            embedder,
            as_of,
            kind_filter,
            group_by,
            lexical_limit,
            dense_limit,
            query_limit,
        }
    }

    /// Hybrid retrieval (embedding + BM25/dense/RRF). Run for conceptual queries, explicit bm25/dense
    /// modes, or as the fallback when structured citation resolution finds nothing.
    fn run_hybrid_candidates(&self) -> Result<Value, ErrorObject> {
        let (query_embedding, embedding_fingerprint) = if self.retrieval_mode.uses_dense() {
            let (literal, fingerprint) = match self.embedder {
                Some(prepared) => prepared.embed(self.req.query.as_str())?,
                None => PreparedQueryEmbedder::from_env()?.embed(self.req.query.as_str())?,
            };
            (Some(literal), Some(fingerprint))
        } else {
            (None, None)
        };
        let response = hybrid_candidates_json(
            self.postgres,
            &HybridCandidateQuery {
                query_text: self.query_text,
                query_embedding: query_embedding.as_deref(),
                embedding_fingerprint: embedding_fingerprint.as_deref(),
                retrieval_mode: self.retrieval_mode,
                group_by: self.group_by,
                options: self.req.retrieval_options(),
                after_cursor: self
                    .after_cursor
                    .map(ParsedSearchCursor::as_retrieval_cursor),
                as_of: self.as_of.as_str(),
                kind_filter: self.kind_filter,
                project_authority: false,
                decision_filters: self.req.decision_filters(),
                lexical_limit: self.lexical_limit,
                dense_limit: self.dense_limit,
                limit: self.query_limit,
            },
        )
        .map_err(storage_error_object)?;
        serde_json::from_str::<Value>(&response)
            .map_err(|error| dependency_unavailable(error.to_string()))
    }

    /// Intent routing. A citation-shaped query (`Article <n>`) in Hybrid mode resolves structurally;
    /// a structured miss falls back to hybrid so a malformed citation still returns results. Conceptual
    /// queries and explicit bm25/dense modes go straight to hybrid.
    fn run_structured_citation_or_fallback(&self) -> Result<RoutedSearch, ErrorObject> {
        let citation_intent = legi_citation_routing(&self.req.query, self.as_of.as_str());
        let query_type = if citation_intent.is_some() {
            "citation"
        } else {
            "semantic"
        };
        let (response, chosen_backend, fallback_path) = match citation_intent {
            Some(parsed) if matches!(self.retrieval_mode, RetrievalMode::Hybrid) => {
                let structured = resolve_legi_citation_json(
                    self.postgres,
                    &CitationResolutionQuery {
                        query: parsed.citation_query.as_str(),
                        article_number: parsed.article_number.as_str(),
                        code_hint: parsed.code_hint.as_deref(),
                        as_of: parsed.as_of.as_str(),
                        kind_filter: self.kind_filter,
                        // Structured results have no pagination cursor; request exactly top_k so the
                        // response never reports a phantom truncation it cannot page past.
                        limit: self.req.top_k,
                    },
                )
                .map_err(storage_error_object)?;
                let structured: Value = serde_json::from_str(&structured)
                    .map_err(|error| dependency_unavailable(error.to_string()))?;
                let count = structured["candidates"].as_array().map_or(0, Vec::len);
                if count > 0 {
                    (structured, "structured_citation", "none")
                } else {
                    (
                        self.run_hybrid_candidates()?,
                        self.retrieval_mode.as_str(),
                        "hybrid_fallback",
                    )
                }
            }
            _ => (
                self.run_hybrid_candidates()?,
                self.retrieval_mode.as_str(),
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
    fn apply_search_response_envelope(
        &self,
        routed: RoutedSearch,
        output_format: OutputFormat,
    ) -> Result<Value, ErrorObject> {
        let RoutedSearch {
            mut response,
            query_type,
            chosen_backend,
            fallback_path,
        } = routed;
        let routed_candidate_count = response["candidates"].as_array().map_or(0, Vec::len);
        let expansion = expand_query(&self.req.query);
        response["format"] = json!(output_format.as_str());
        response["limit"] = json!(self.req.top_k);
        response["expansion_seed_version"] = json!(expansion.seed_version);
        response["expanded_terms"] = json!(expansion.expanded_terms);
        let mut next_cursor = None;
        let top_k = self.req.top_k as usize;
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
        // cursor, so cursor paging does not apply to them.
        let cursor_supported = chosen_backend != "structured_citation";
        response["pagination"] = search_pagination_value(
            self.req.top_k,
            self.req.cursor.as_deref(),
            returned,
            cursor_supported,
            next_cursor.as_deref(),
        );
        // Intent-routing audit: prove the resolver was used because the input is structurally
        // resolvable (query_type=citation, backend=structured_citation), not because the evaluator
        // knew the answer. Recorded on every search, structured and hybrid alike.
        response["routing"] = json!({
            "query_type": query_type,
            "chosen_backend": chosen_backend,
            "candidate_count": routed_candidate_count,
            "fallback_path": fallback_path,
        });
        if matches!(output_format, OutputFormat::Detailed) {
            response["diagnostics"] = json!({
                "query_input": self.req.query.clone(),
                "lexical_query_text": if self.retrieval_mode.uses_lexical() {
                    Some(self.query_text)
                } else {
                    None
                },
                "retrieval": {
                    "mode": self.retrieval_mode.as_str(),
                    "uses_lexical": self.retrieval_mode.uses_lexical(),
                    "uses_dense": self.retrieval_mode.uses_dense(),
                    "lexical_limit": self.lexical_limit,
                    "dense_limit": self.dense_limit,
                    "query_limit": self.query_limit,
                    "kind_filter": self.kind_filter,
                    "after_cursor": self.req.cursor.as_deref(),
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

#[derive(Debug)]
pub(crate) enum ParsedSearchCursor {
    Chunk { score: String, chunk_id: String },
    Document { score: String, document_id: String },
}

impl ParsedSearchCursor {
    pub(crate) fn as_retrieval_cursor(&self) -> RetrievalCursor<'_> {
        match self {
            Self::Chunk { score, chunk_id } => RetrievalCursor::Chunk { score, chunk_id },
            Self::Document { score, document_id } => {
                RetrievalCursor::Document { score, document_id }
            }
        }
    }
}

pub(crate) fn validate_cursor_score(score: &str, tail: &str) -> Result<(), ErrorObject> {
    let parsed = score.parse::<f64>().map_err(|_| {
        ErrorObject::bad_input(
            "search --cursor must start with a numeric score followed by ':' and an id",
        )
    })?;
    if !parsed.is_finite() || parsed < 0.0 || tail.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "search --cursor must be a finite non-negative score followed by ':' and an id",
        ));
    }
    Ok(())
}

/// Parse the opaque cursor, tagged by grouping. A `doc:`-prefixed cursor is a document cursor; an
/// unprefixed `<score>:<chunk_id>` is a chunk cursor. A cursor from the other grouping is rejected
/// rather than silently mis-paging.
pub(crate) fn parse_search_cursor(
    cursor: &str,
    group_by: CliGroupBy,
) -> Result<ParsedSearchCursor, ErrorObject> {
    if let Some(rest) = cursor.strip_prefix("doc:") {
        if group_by != CliGroupBy::Document {
            return Err(ErrorObject::bad_input(
                "this is a document cursor; rerun with --group-by document",
            ));
        }
        let (score, document_id) = rest.split_once(':').ok_or_else(|| {
            ErrorObject::bad_input("malformed document cursor (expected doc:<score>:<document_id>)")
        })?;
        validate_cursor_score(score, document_id)?;
        Ok(ParsedSearchCursor::Document {
            score: score.to_owned(),
            document_id: document_id.to_owned(),
        })
    } else {
        if group_by != CliGroupBy::Chunk {
            return Err(ErrorObject::bad_input(
                "this is a chunk cursor; rerun with --group-by chunk (the default)",
            ));
        }
        let (score, chunk_id) = cursor.split_once(':').ok_or_else(|| {
            ErrorObject::bad_input(
                "search --cursor must use the cursor value returned by a previous search candidate",
            )
        })?;
        validate_cursor_score(score, chunk_id)?;
        Ok(ParsedSearchCursor::Chunk {
            score: score.to_owned(),
            chunk_id: chunk_id.to_owned(),
        })
    }
}
