use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

// Dense ANN candidates are post-filtered by document validity, so fetch a
// wider pool before assigning gap-free dense ranks.
const DENSE_TEMPORAL_OVERFETCH_FACTOR: u32 = 4;
const DEFAULT_CONTEXT_SIBLING_LIMIT: u32 = 50;

// Reciprocal-rank-fusion constant and per-arm weights. LEGI has many near-duplicate sibling
// articles (same parent text, different article number) whose dense embeddings are nearly
// identical, so an equal-weight dense arm dilutes the much sharper BM25 ranking on exact-citation
// queries. The weights let the dense arm act as a recall-expander/tie-breaker rather than an equal
// vote; tune via env without recompiling. The default down-weights dense to 0.3 (BM25-favored).
const RRF_K: f64 = 60.0;
const DEFAULT_RRF_LEXICAL_WEIGHT: f64 = 1.0;
// Dense down-weighted to a recall-expander/tie-breaker. France-LEGI calibration over the production
// index: equal weight (1.0) gave known-item recall@10 0.55; 0.3 lifts it to 0.60 with no temporal
// regression (0.75) and an immaterial cross-reference change. Lower still (0.15) trades temporal
// away for known-item. See reviews/2026-06-23-retrieval-fusion-*.
const DEFAULT_RRF_DENSE_WEIGHT: f64 = 0.3;

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

/// Format a finite, non-negative f64 as a plain SQL numeric literal (no locale/exponent surprises).
/// `rrf_weights` already guarantees finiteness; clamp defensively.
fn format_sql_f64(value: f64) -> String {
    let value = if value.is_finite() { value.max(0.0) } else { 0.0 };
    format!("{value:.6}")
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
    pub lexical_limit: u32,
    pub dense_limit: u32,
    pub limit: u32,
}

impl HybridCandidateQuery<'_> {
    fn effective_rrf_weights(&self) -> (f64, f64) {
        let (lexical, dense) = rrf_weights();
        (
            self.options.rrf_lexical_weight.unwrap_or(lexical),
            self.options.rrf_dense_weight.unwrap_or(dense),
        )
    }

    fn effective_probes(&self) -> u32 {
        self.options.ivfflat_probes.unwrap_or(4)
    }
}

/// An opaque pagination cursor, tagged by grouping. A chunk cursor is `<score>:<chunk_id>`; a
/// document cursor is `doc:<score>:<document_id>`. The tag lets us reject a cursor from the wrong
/// grouping instead of silently mis-paging.
#[derive(Debug, Clone, Copy)]
pub enum RetrievalCursor<'a> {
    Chunk { score: &'a str, chunk_id: &'a str },
    Document { score: &'a str, document_id: &'a str },
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
}

impl RelatedRelation {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cites" => Some(Self::Cites),
            "cited_by" => Some(Self::CitedBy),
            "temporal" => Some(Self::Temporal),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cites => "cites",
            Self::CitedBy => "cited_by",
            Self::Temporal => "temporal",
        }
    }

    fn direction(self) -> &'static str {
        match self {
            Self::Cites | Self::Temporal => "outgoing",
            Self::CitedBy => "incoming",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RelatedQuery<'a> {
    pub document_id: &'a str,
    pub rel: RelatedRelation,
    pub limit: u32,
}

pub fn hybrid_candidates_json(
    postgres: &ManagedPostgres,
    query: &HybridCandidateQuery<'_>,
) -> Result<String, StorageError> {
    let query_text = sql_string_literal(query.query_text);
    let as_of = sql_string_literal(query.as_of);
    let retrieval_mode = sql_string_literal(query.retrieval_mode.as_str());
    let kind_predicate = query
        .kind_filter
        .map(|kind| format!("AND d.kind = {}", sql_string_literal(kind)))
        .unwrap_or_default();
    let ranked_ctes = ranked_candidate_ctes(query, &query_text, &as_of, &kind_predicate)?;
    let set_ivfflat_probes = if query.retrieval_mode.uses_dense() {
        format!("SET ivfflat.probes = {};\n\n", query.effective_probes())
    } else {
        String::new()
    };
    let limit = query.limit;

    let sql = match query.group_by {
        GroupBy::Chunk => {
            let cursor_predicate = cursor_predicate(query.after_cursor);
            format!(
                r#"
{set_ivfflat_probes}WITH {ranked_ctes},
limited AS (
    SELECT
        r.chunk_id, c.document_id, d.source, d.kind, d.citation, d.title, d.source_url,
        d.valid_from::text AS valid_from, d.valid_to::text AS valid_to,
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,
        r.lexical_rank, r.dense_rank, r.fused_score
    FROM ranked r
    JOIN chunks c ON c.chunk_id = r.chunk_id
    JOIN documents d ON d.document_id = c.document_id
    {cursor_predicate}
    ORDER BY round(r.fused_score::numeric, 8) DESC, r.chunk_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'query', {query_text},
    'retrieval_mode', {retrieval_mode},
    'as_of', {as_of},
    'group_by', 'chunk',
    'limit', {limit},
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'chunk_id', chunk_id,
            'document_id', document_id,
            'source', source, 'kind', kind, 'citation', citation, 'title', title,
            'source_url', source_url, 'snippet', snippet,
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'scores', jsonb_build_object('rrf', round(fused_score::numeric, 8), 'lexical_rank', lexical_rank, 'dense_rank', dense_rank),
            'cursor', concat(round(fused_score::numeric, 8)::text, ':', chunk_id)
        ) ORDER BY round(fused_score::numeric, 8) DESC, chunk_id)
        FROM limited
    ), '[]'::jsonb)
)::text;
"#
            )
        }
        GroupBy::Document => {
            // Group BEFORE paging: pick each document's best chunk, then rank documents and apply the
            // document keyset cursor over that grouped rowset (never post-page dedupe).
            let cursor_predicate = document_cursor_predicate(query.after_cursor);
            format!(
                r#"
{set_ivfflat_probes}WITH {ranked_ctes},
scored AS (
    SELECT
        r.chunk_id, c.document_id, d.source, d.kind, d.citation, d.title, d.source_url,
        d.valid_from::text AS valid_from, d.valid_to::text AS valid_to,
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,
        r.lexical_rank, r.dense_rank,
        round(r.fused_score::numeric, 8) AS cursor_score
    FROM ranked r
    JOIN chunks c ON c.chunk_id = r.chunk_id
    JOIN documents d ON d.document_id = c.document_id
),
best_document_chunks AS (
    SELECT DISTINCT ON (document_id) *
    FROM scored
    ORDER BY document_id, cursor_score DESC, chunk_id
),
limited AS (
    SELECT *
    FROM best_document_chunks
    {cursor_predicate}
    ORDER BY cursor_score DESC, document_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'query', {query_text},
    'retrieval_mode', {retrieval_mode},
    'as_of', {as_of},
    'group_by', 'document',
    'limit', {limit},
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'document_id', document_id,
            'chunk_id', chunk_id,
            'best_chunk_id', chunk_id,
            'source', source, 'kind', kind, 'citation', citation, 'title', title,
            'source_url', source_url, 'snippet', snippet,
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'scores', jsonb_build_object('rrf', cursor_score, 'lexical_rank', lexical_rank, 'dense_rank', dense_rank),
            'cursor', concat('doc:', cursor_score::text, ':', document_id)
        ) ORDER BY cursor_score DESC, document_id)
        FROM limited
    ), '[]'::jsonb)
)::text;
"#
            )
        }
    };
    postgres.execute_sql(&sql)
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

/// Resolve a citation-shaped query structurally: LEGI articles whose `citation`/`title` contain the
/// article reference (and optional code hint) and are valid at `as_of`. Returns the SAME JSON shape
/// as [`hybrid_candidates_json`] (a `candidates` array keyed on `document_id`), so callers can route
/// to either backend transparently. The as-of validity filter yields one version per matched
/// article, and exact-citation matches are ranked first.
pub fn resolve_legi_citation_json(
    postgres: &ManagedPostgres,
    query: &CitationResolutionQuery<'_>,
) -> Result<String, StorageError> {
    let query_literal = sql_string_literal(query.query);
    let as_of = sql_string_literal(query.as_of);
    // Match the article number EXACTLY against the article's own title ("Article <n>"), not a
    // `%article <n>%` substring — otherwise "Article 33" also matches "Article 330"/"33-1" and a
    // newer prefix sibling could outrank the intended article.
    let article_title = sql_string_literal(&format!(
        "article {}",
        query.article_number.trim().to_lowercase()
    ));
    let code_hint_predicate = match query.code_hint {
        Some(hint) if !hint.trim().is_empty() => format!(
            "AND lower(concat_ws(' ', d.citation, d.title)) LIKE {} ESCAPE '\\'",
            sql_string_literal(&like_contains(&hint.trim().to_lowercase()))
        ),
        _ => String::new(),
    };
    let kind_predicate = query
        .kind_filter
        .map(|kind| format!("AND d.kind = {}", sql_string_literal(kind)))
        .unwrap_or_default();
    let limit = query.limit.max(1);

    postgres.execute_sql(&format!(
        r#"
WITH resolved AS (
    SELECT
        d.document_id,
        d.source,
        d.kind,
        d.citation,
        d.title,
        d.source_url,
        d.valid_from::text AS valid_from,
        d.valid_to::text AS valid_to,
        (lower(btrim(d.citation)) = lower(btrim({query_literal}))) AS exact_citation_match,
        d.valid_from AS sort_valid_from
    FROM documents d
    WHERE d.source = 'legi'
      AND d.kind = 'article'
      AND lower(btrim(d.title)) = {article_title}
      {code_hint_predicate}
      {kind_predicate}
      AND (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
      AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
    ORDER BY exact_citation_match DESC, d.valid_from DESC NULLS LAST, d.document_id
    LIMIT {limit}
),
with_chunk AS (
    SELECT
        r.*,
        ch.chunk_id,
        ch.snippet
    FROM resolved r
    LEFT JOIN LATERAL (
        SELECT chunk_id, left(regexp_replace(body, '\s+', ' ', 'g'), 280) AS snippet
        FROM chunks
        WHERE document_id = r.document_id
        ORDER BY chunk_index
        LIMIT 1
    ) ch ON true
)
SELECT jsonb_build_object(
    'query', {query_literal},
    'retrieval_mode', 'structured_citation',
    'as_of', {as_of},
    'limit', {limit},
    'candidates', COALESCE((
        SELECT jsonb_agg(
            jsonb_build_object(
                'chunk_id', chunk_id,
                'document_id', document_id,
                'source', source,
                'kind', kind,
                'citation', citation,
                'title', title,
                'source_url', source_url,
                'snippet', snippet,
                'validity', jsonb_build_object(
                    'from', valid_from,
                    'to', valid_to,
                    'to_exclusive', true
                ),
                'scores', jsonb_build_object(
                    'exact_citation_match', exact_citation_match
                ),
                'cursor', NULL
            )
            ORDER BY exact_citation_match DESC, sort_valid_from DESC NULLS LAST, document_id
        )
        FROM with_chunk
    ), '[]'::jsonb)
)::text;
"#
    ))
}

/// Build a `%value%` LIKE pattern, escaping the LIKE metacharacters `\ % _` (escape char `\`).
fn like_contains(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('%');
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('%');
    escaped
}

/// Keyset cursor predicate for chunk grouping (over the `ranked` rows). Tie-break on `chunk_id`.
fn cursor_predicate(cursor: Option<RetrievalCursor<'_>>) -> String {
    match cursor {
        Some(RetrievalCursor::Chunk { score, chunk_id }) => {
            let score = sql_string_literal(score);
            let chunk_id = sql_string_literal(chunk_id);
            format!(
                "WHERE (round(r.fused_score::numeric, 8) < {score}::numeric \
                 OR (round(r.fused_score::numeric, 8) = {score}::numeric \
                 AND r.chunk_id > {chunk_id}))"
            )
        }
        // A document cursor never reaches the chunk-grouped query (the CLI rejects the mismatch).
        Some(RetrievalCursor::Document { .. }) | None => String::new(),
    }
}

/// Keyset cursor predicate for document grouping (over `best_document_chunks`). Tie-break on
/// `document_id`. Uses the same rounded `cursor_score` as the ordering so ties never duplicate/skip.
fn document_cursor_predicate(cursor: Option<RetrievalCursor<'_>>) -> String {
    match cursor {
        Some(RetrievalCursor::Document { score, document_id }) => {
            let score = sql_string_literal(score);
            let document_id = sql_string_literal(document_id);
            format!(
                "WHERE (cursor_score < {score}::numeric \
                 OR (cursor_score = {score}::numeric AND document_id > {document_id}))"
            )
        }
        Some(RetrievalCursor::Chunk { .. }) | None => String::new(),
    }
}

fn ranked_candidate_ctes(
    query: &HybridCandidateQuery<'_>,
    query_text: &str,
    as_of: &str,
    kind_predicate: &str,
) -> Result<String, StorageError> {
    match query.retrieval_mode {
        RetrievalMode::Hybrid => {
            let (query_embedding, embedding_fingerprint) = dense_query_inputs(query)?;
            let query_embedding = sql_string_literal(query_embedding);
            let embedding_fingerprint = sql_string_literal(embedding_fingerprint);
            let dense_pool_limit = dense_pool_limit(query.dense_limit);
            let (lexical_weight, dense_weight) = query.effective_rrf_weights();
            Ok(format!(
                r#"
lexical AS (
    SELECT
        c.chunk_id,
        row_number() OVER (ORDER BY paradedb.score(c.chunk_id) DESC, c.chunk_id) AS lexical_rank
    FROM chunks c
    JOIN documents d ON d.document_id = c.document_id
    WHERE c.contextualized_body @@@ {query_text}
      AND (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
      AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
      {kind_predicate}
    ORDER BY paradedb.score(c.chunk_id) DESC, c.chunk_id
    LIMIT {lexical_limit}
),
dense_pool AS (
    SELECT
        scored.chunk_id,
        row_number() OVER (ORDER BY scored.distance) AS dense_rank
    FROM (
        SELECT
            ce.chunk_id,
            ce.embedding <-> {query_embedding}::vector AS distance
        FROM chunk_embeddings ce
        WHERE ce.embedding_fingerprint = {embedding_fingerprint}
    ) scored
    ORDER BY scored.distance
    LIMIT {dense_pool_limit}
),
dense AS (
    SELECT
        dp.chunk_id,
        row_number() OVER (ORDER BY dp.dense_rank, dp.chunk_id) AS dense_rank
    FROM dense_pool dp
    JOIN chunks c ON c.chunk_id = dp.chunk_id
    JOIN documents d ON d.document_id = c.document_id
    WHERE (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
      AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
      {kind_predicate}
    ORDER BY dp.dense_rank, dp.chunk_id
    LIMIT {dense_limit}
),
fused AS (
    SELECT
        chunk_id,
        min(lexical_rank) AS lexical_rank,
        min(dense_rank) AS dense_rank
    FROM (
        SELECT chunk_id, lexical_rank, NULL::bigint AS dense_rank FROM lexical
        UNION ALL
        SELECT chunk_id, NULL::bigint AS lexical_rank, dense_rank FROM dense
    ) ranks
    GROUP BY chunk_id
),
ranked AS (
    SELECT
        f.chunk_id,
        f.lexical_rank,
        f.dense_rank,
        (
            CASE WHEN f.lexical_rank IS NULL THEN 0.0 ELSE {lexical_weight} / ({rrf_k} + f.lexical_rank) END
            + CASE WHEN f.dense_rank IS NULL THEN 0.0 ELSE {dense_weight} / ({rrf_k} + f.dense_rank) END
        ) AS fused_score
    FROM fused f
)"#,
                query_text = query_text,
                as_of = as_of,
                kind_predicate = kind_predicate,
                lexical_limit = query.lexical_limit,
                query_embedding = query_embedding,
                embedding_fingerprint = embedding_fingerprint,
                dense_pool_limit = dense_pool_limit,
                dense_limit = query.dense_limit,
                rrf_k = format_sql_f64(RRF_K),
                lexical_weight = format_sql_f64(lexical_weight),
                dense_weight = format_sql_f64(dense_weight),
            ))
        }
        RetrievalMode::Bm25 => Ok(format!(
            r#"
lexical AS (
    SELECT
        c.chunk_id,
        row_number() OVER (ORDER BY paradedb.score(c.chunk_id) DESC, c.chunk_id) AS lexical_rank
    FROM chunks c
    JOIN documents d ON d.document_id = c.document_id
    WHERE c.contextualized_body @@@ {query_text}
      AND (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
      AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
      {kind_predicate}
    ORDER BY paradedb.score(c.chunk_id) DESC, c.chunk_id
    LIMIT {lexical_limit}
),
ranked AS (
    SELECT
        l.chunk_id,
        l.lexical_rank,
        NULL::bigint AS dense_rank,
        1.0 / (60.0 + l.lexical_rank) AS fused_score
    FROM lexical l
)"#,
            query_text = query_text,
            as_of = as_of,
            kind_predicate = kind_predicate,
            lexical_limit = query.lexical_limit,
        )),
        RetrievalMode::Dense => {
            let (query_embedding, embedding_fingerprint) = dense_query_inputs(query)?;
            let query_embedding = sql_string_literal(query_embedding);
            let embedding_fingerprint = sql_string_literal(embedding_fingerprint);
            let dense_pool_limit = dense_pool_limit(query.dense_limit);
            Ok(format!(
                r#"
dense_pool AS (
    SELECT
        scored.chunk_id,
        row_number() OVER (ORDER BY scored.distance) AS dense_rank
    FROM (
        SELECT
            ce.chunk_id,
            ce.embedding <-> {query_embedding}::vector AS distance
        FROM chunk_embeddings ce
        WHERE ce.embedding_fingerprint = {embedding_fingerprint}
    ) scored
    ORDER BY scored.distance
    LIMIT {dense_pool_limit}
),
dense AS (
    SELECT
        dp.chunk_id,
        row_number() OVER (ORDER BY dp.dense_rank, dp.chunk_id) AS dense_rank
    FROM dense_pool dp
    JOIN chunks c ON c.chunk_id = dp.chunk_id
    JOIN documents d ON d.document_id = c.document_id
    WHERE (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
      AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
      {kind_predicate}
    ORDER BY dp.dense_rank, dp.chunk_id
    LIMIT {dense_limit}
),
ranked AS (
    SELECT
        d.chunk_id,
        NULL::bigint AS lexical_rank,
        d.dense_rank,
        1.0 / (60.0 + d.dense_rank) AS fused_score
    FROM dense d
)"#,
                query_embedding = query_embedding,
                embedding_fingerprint = embedding_fingerprint,
                dense_pool_limit = dense_pool_limit,
                as_of = as_of,
                kind_predicate = kind_predicate,
                dense_limit = query.dense_limit,
            ))
        }
    }
}

fn dense_query_inputs<'a>(
    query: &'a HybridCandidateQuery<'a>,
) -> Result<(&'a str, &'a str), StorageError> {
    let query_embedding = query
        .query_embedding
        .ok_or_else(|| StorageError::Retrieval {
            message: format!(
                "{} retrieval requires a query embedding",
                query.retrieval_mode.as_str()
            ),
        })?;
    let embedding_fingerprint =
        query
            .embedding_fingerprint
            .ok_or_else(|| StorageError::Retrieval {
                message: format!(
                    "{} retrieval requires an embedding fingerprint",
                    query.retrieval_mode.as_str()
                ),
            })?;
    Ok((query_embedding, embedding_fingerprint))
}

fn dense_pool_limit(dense_limit: u32) -> u32 {
    dense_limit
        .saturating_mul(DENSE_TEMPORAL_OVERFETCH_FACTOR)
        .max(dense_limit)
}

pub fn fetch_documents_json(
    postgres: &ManagedPostgres,
    query: &FetchDocumentsQuery<'_>,
) -> Result<String, StorageError> {
    if query.document_ids.is_empty() {
        return Ok(r#"{"documents":[]}"#.to_owned());
    }

    let requested_values = query
        .document_ids
        .iter()
        .enumerate()
        .map(|(ordinal, document_id)| format!("({}, {})", sql_string_literal(document_id), ordinal))
        .collect::<Vec<_>>()
        .join(", ");

    postgres.execute_sql(&format!(
        r#"
WITH requested(document_id, ordinal) AS (
    VALUES {requested_values}
),
matched AS (
    SELECT
        r.ordinal,
        d.document_id,
        d.source,
        d.kind,
        d.source_uid,
        d.version_group,
        d.citation,
        d.title,
        d.body,
        d.valid_from::text AS valid_from,
        d.valid_to::text AS valid_to,
        d.valid_to_raw,
        d.source_url,
        d.source_payload_hash
    FROM requested r
    JOIN documents d ON d.document_id = r.document_id
)
SELECT jsonb_build_object(
    'documents', COALESCE((
        SELECT jsonb_agg(
            jsonb_build_object(
                'document_id', m.document_id,
                'source', m.source,
                'kind', m.kind,
                'source_uid', m.source_uid,
                'version_group', m.version_group,
                'citation', m.citation,
                'title', m.title,
                'body', m.body,
                'validity', jsonb_build_object(
                    'from', m.valid_from,
                    'to', m.valid_to,
                    'to_raw', m.valid_to_raw,
                    'to_exclusive', true
                ),
                'source_url', m.source_url,
                'source_payload_hash', m.source_payload_hash,
                'chunks', COALESCE((
                    SELECT jsonb_agg(
                        jsonb_build_object(
                            'chunk_id', c.chunk_id,
                            'chunk_index', c.chunk_index,
                            'chunk_kind', c.chunk_kind,
                            'body', c.body,
                            'contextualized_body', c.contextualized_body,
                            'chunking', c.chunking,
                            'boundary', c.boundary,
                            'hierarchy_path', c.hierarchy_path,
                            'source_fields', c.source_fields,
                            'source_payload_hash', c.source_payload_hash,
                            'chunk_builder_version', c.chunk_builder_version,
                            'embedding_fingerprint', c.embedding_fingerprint
                        )
                        ORDER BY c.chunk_index
                    )
                    FROM chunks c
                    WHERE c.document_id = m.document_id
                ), '[]'::jsonb)
            )
            ORDER BY m.ordinal
        )
        FROM matched m
    ), '[]'::jsonb)
)::text;
"#,
        requested_values = requested_values
    ))
}

pub fn context_documents_json(
    postgres: &ManagedPostgres,
    query: &ContextDocumentsQuery<'_>,
) -> Result<String, StorageError> {
    let document_id = sql_string_literal(query.document_id);
    let as_of_expression = query
        .as_of
        .map(|as_of| format!("{}::date", sql_string_literal(as_of)))
        .unwrap_or_else(|| "NULL::date".to_owned());
    let requested_as_of = query
        .as_of
        .map(sql_string_literal)
        .unwrap_or_else(|| "NULL".to_owned());
    let include_siblings = if query.include_siblings {
        "true"
    } else {
        "false"
    };
    let sibling_limit = DEFAULT_CONTEXT_SIBLING_LIMIT;

    postgres.execute_sql(&format!(
        r#"
WITH target_raw AS (
    SELECT
        d.document_id,
        d.source,
        d.kind,
        d.source_uid,
        d.version_group,
        d.citation,
        d.title,
        d.valid_from,
        d.valid_to,
        d.valid_to_raw,
        d.source_url,
        d.hierarchy_path
    FROM documents d
    WHERE d.document_id = {document_id}
),
target AS (
    SELECT
        t.*,
        CASE
            WHEN jsonb_typeof(t.hierarchy_path) = 'array'
                THEN t.hierarchy_path
            ELSE '[]'::jsonb
        END AS context_hierarchy_path,
        COALESCE({as_of_expression}, t.valid_from, CURRENT_DATE) AS effective_as_of
    FROM target_raw t
),
visible_target AS (
    SELECT *
    FROM target t
    WHERE {as_of_expression} IS NULL
       OR ((t.valid_from IS NULL OR t.valid_from <= t.effective_as_of)
       AND  (t.valid_to IS NULL OR t.valid_to > t.effective_as_of))
),
sibling_candidates AS (
    SELECT
        d.document_id,
        d.source,
        d.kind,
        d.source_uid,
        d.version_group,
        d.citation,
        d.title,
        d.valid_from::text AS valid_from,
        d.valid_to::text AS valid_to,
        d.valid_to_raw,
        d.source_url,
        d.hierarchy_path
    FROM visible_target t
    JOIN documents d
      ON d.source = t.source
     AND d.kind = t.kind
     AND md5(d.hierarchy_path::text) = md5(t.context_hierarchy_path::text)
     AND d.hierarchy_path = t.context_hierarchy_path
     AND d.document_id <> t.document_id
    WHERE {include_siblings}
      AND jsonb_typeof(t.context_hierarchy_path) = 'array'
      AND jsonb_array_length(t.context_hierarchy_path) > 0
      AND (d.valid_from IS NULL OR d.valid_from <= t.effective_as_of)
      AND (d.valid_to IS NULL OR d.valid_to > t.effective_as_of)
),
limited_siblings AS (
    SELECT *
    FROM sibling_candidates
    ORDER BY title, document_id
    LIMIT {sibling_limit}
)
SELECT jsonb_build_object(
    'id', {document_id},
    'as_of', (SELECT effective_as_of::text FROM target),
    'requested_as_of', {requested_as_of},
    'target', (
        SELECT jsonb_build_object(
            'document_id', t.document_id,
            'source', t.source,
            'kind', t.kind,
            'source_uid', t.source_uid,
            'version_group', t.version_group,
            'citation', t.citation,
            'title', t.title,
            'validity', jsonb_build_object(
                'from', t.valid_from::text,
                'to', t.valid_to::text,
                'to_raw', t.valid_to_raw,
                'to_exclusive', true
            ),
            'source_url', t.source_url,
            'hierarchy_path', t.context_hierarchy_path
        )
        FROM visible_target t
    ),
    'ancestry', COALESCE((
        SELECT jsonb_agg(
            jsonb_build_object(
                'depth', path.ordinality - 1,
                'title', path.value
            )
            ORDER BY path.ordinality
        )
        FROM visible_target t,
             jsonb_array_elements_text(t.context_hierarchy_path) WITH ORDINALITY AS path(value, ordinality)
    ), '[]'::jsonb),
    'siblings', COALESCE((
        SELECT jsonb_agg(
            jsonb_build_object(
                'document_id', s.document_id,
                'source', s.source,
                'kind', s.kind,
                'source_uid', s.source_uid,
                'version_group', s.version_group,
                'citation', s.citation,
                'title', s.title,
                'validity', jsonb_build_object(
                    'from', s.valid_from,
                    'to', s.valid_to,
                    'to_raw', s.valid_to_raw,
                    'to_exclusive', true
                ),
                'source_url', s.source_url,
                'hierarchy_path', s.hierarchy_path
            )
            ORDER BY s.title, s.document_id
        )
        FROM limited_siblings s
    ), '[]'::jsonb),
    'sibling_count', (SELECT count(*) FROM sibling_candidates),
    'sibling_limit', {sibling_limit},
    'sibling_truncated', (SELECT count(*) > {sibling_limit} FROM sibling_candidates)
)::text;
"#,
        document_id = document_id,
        as_of_expression = as_of_expression,
        requested_as_of = requested_as_of,
        include_siblings = include_siblings,
        sibling_limit = sibling_limit
    ))
}

/// Corpus/graph/embedding counts for `stats` — replaces ad-hoc psql for introspection. The counts
/// are exact (sequential scans over large tables), so this is an introspection command, not a hot path.
pub fn corpus_stats_json(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT jsonb_build_object(
    'documents', (SELECT count(*) FROM documents),
    'documents_by_kind', COALESCE((SELECT jsonb_object_agg(kind, c) FROM (SELECT kind, count(*) c FROM documents GROUP BY kind) t), '{}'::jsonb),
    'documents_by_source', COALESCE((SELECT jsonb_object_agg(source, c) FROM (SELECT source, count(*) c FROM documents GROUP BY source) t), '{}'::jsonb),
    'chunks', (SELECT count(*) FROM chunks),
    'chunk_embeddings', (SELECT count(*) FROM chunk_embeddings),
    'graph_edges', (SELECT count(*) FROM graph_edges),
    'graph_edges_by_kind', COALESCE((SELECT jsonb_object_agg(edge_kind, c) FROM (SELECT edge_kind, count(*) c FROM graph_edges GROUP BY edge_kind) t), '{}'::jsonb),
    'graph_edges_by_source', COALESCE((SELECT jsonb_object_agg(edge_source, c) FROM (SELECT edge_source, count(*) c FROM graph_edges GROUP BY edge_source) t), '{}'::jsonb)
)::text;
"#,
    )
}

/// Per-source corpus coverage + freshness for `status`, read cheaply from each source's latest
/// completed ingest run manifest (no live full-corpus counts). Surfaces honest zone/chunking
/// provenance so judicial (cass/capp/inca) vs administrative (jade) jurisprudence coverage and
/// freshness are visible alongside legi. Sources without a completed run simply do not appear.
pub fn corpus_source_coverage_json(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT COALESCE((
    SELECT jsonb_object_agg(source, summary)
    FROM (
        SELECT DISTINCT ON (source)
            source,
            jsonb_build_object(
                'latest_completed_run', run_id,
                'completed_at', completed_at,
                'dataset', manifest->>'dataset',
                'source_version', manifest->>'source_version',
                'zone_accurate', COALESCE(manifest->'zone_accurate', 'null'::jsonb),
                'chunking_provenance', manifest->>'chunking_provenance',
                'freshness', COALESCE(manifest->'freshness', 'null'::jsonb),
                -- Per-run insert counts from the latest run (a replay/sync may legitimately be 0);
                -- cumulative live corpus counts are exposed by the `stats` command.
                'last_run_coverage', COALESCE(manifest->'coverage', '{}'::jsonb)
            ) AS summary
        FROM ingest_run
        WHERE status = 'completed'
        ORDER BY source, completed_at DESC NULLS LAST, run_id DESC
    ) latest
), '{}'::jsonb)::text;
"#,
    )
}

/// Raw canonical record for one document (`inspect`): the full `documents` row (incl. canonical_json),
/// its chunk count, and outgoing edge count. Returns `{"document": null, ...}` when the id is unknown.
pub fn inspect_document_json(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    postgres.execute_sql(&format!(
        r#"
SELECT jsonb_build_object(
    'document', (SELECT to_jsonb(d) FROM documents d WHERE d.document_id = {id}),
    'chunk_count', (SELECT count(*) FROM chunks WHERE document_id = {id}),
    'outgoing_edges', (SELECT count(*) FROM graph_edges WHERE from_document_id = {id})
)::text;
"#
    ))
}

const CITATION_FILTER: &str =
    r#"'[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb"#;
const VERSION_FILTER: &str =
    r#"'[{"key":"debut"},{"key":"fin"},{"key":"num"},{"key":"etat"}]'::jsonb"#;

/// The version family of an article: the target plus its LIEN_ART version-edge neighbours, with a
/// shared CTE body so `versions` and `diff` resolve the same set. `$id` is interpolated by the caller.
fn version_family_cte(id: &str) -> String {
    // No `is_target` column here: LIEN_ART edges include a self-link, so the target also appears in
    // the edge branch. Keeping the columns identical lets UNION dedupe to one row per document;
    // consumers derive `is_target` as `document_id = id`.
    format!(
        r#"family AS (
    SELECT d.document_id, d.source_uid, d.citation, d.title,
           d.valid_from::text AS valid_from, d.valid_to::text AS valid_to, d.body
    FROM documents d
    WHERE d.document_id = {id}
    UNION
    SELECT td.document_id, td.source_uid, td.citation, td.title,
           td.valid_from::text AS valid_from, td.valid_to::text AS valid_to, td.body
    FROM graph_edges e
    JOIN documents td
      ON td.source = 'legi' AND td.kind = 'article'
     AND td.source_uid = e.payload->>'to_source_uid'
    WHERE e.edge_source = 'publisher'
      AND e.from_document_id = {id}
      AND e.payload->>'source_tag' = 'LIEN_ART'
      AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
      AND e.payload->'attributes' @> {VERSION_FILTER}
)"#
    )
}

/// Version timeline for an article (`versions`): every member of its version family ordered by
/// validity start. Each entry carries validity window + citation; the requested id is flagged.
pub fn document_versions_json(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    postgres.execute_sql(&format!(
        r#"
WITH {family}
SELECT jsonb_build_object(
    'id', {id},
    'count', (SELECT count(*) FROM family),
    'versions', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'document_id', document_id,
            'source_uid', source_uid,
            'citation', citation,
            'title', title,
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'is_target', (document_id = {id})
        ) ORDER BY valid_from NULLS FIRST, document_id)
        FROM family
    ), '[]'::jsonb)
)::text;
"#,
        family = version_family_cte(&id)
    ))
}

/// Compare the article versions in force on two dates (`diff`). Returns the family member valid on
/// each date (full record incl. body) and whether the version changed between them.
pub fn document_diff_json(
    postgres: &ManagedPostgres,
    document_id: &str,
    from: &str,
    to: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    let from_lit = sql_string_literal(from);
    let to_lit = sql_string_literal(to);
    postgres.execute_sql(&format!(
        r#"
WITH {family},
from_version AS (
    SELECT * FROM family f
    WHERE (f.valid_from IS NULL OR f.valid_from <= {from_lit})
      AND (f.valid_to IS NULL OR f.valid_to > {from_lit})
    ORDER BY f.valid_from DESC NULLS LAST LIMIT 1
),
to_version AS (
    SELECT * FROM family f
    WHERE (f.valid_from IS NULL OR f.valid_from <= {to_lit})
      AND (f.valid_to IS NULL OR f.valid_to > {to_lit})
    ORDER BY f.valid_from DESC NULLS LAST LIMIT 1
)
SELECT jsonb_build_object(
    'id', {id},
    'from', {from_lit},
    'to', {to_lit},
    'family_count', (SELECT count(*) FROM family),
    'from_version', (SELECT to_jsonb(f) FROM from_version f),
    'to_version', (SELECT to_jsonb(t) FROM to_version t),
    'changed', (
        (SELECT document_id FROM from_version) IS DISTINCT FROM (SELECT document_id FROM to_version)
    )
)::text;
"#,
        family = version_family_cte(&id)
    ))
}

/// Depth-1 publisher-edge graph traversal for `related`. Resolves typed neighbours of an exact,
/// version-pinned `document_id`:
/// - `cites` / `temporal`: outgoing edges (`from_document_id = id`), target resolved by
///   `payload->>'to_source_uid'` → `documents.source_uid` — served by `graph_edges_from_idx`.
/// - `cited_by`: incoming citations keyed on the seed's `source_uid` — served by the partial
///   expression index `graph_edges_publisher_citation_to_source_uid_idx` (migration 10).
pub fn related_neighbours_json(
    postgres: &ManagedPostgres,
    query: &RelatedQuery<'_>,
) -> Result<String, StorageError> {
    let id = sql_string_literal(query.document_id);
    let limit = query.limit.max(1);
    let rel = query.rel.as_str();
    let direction = query.rel.direction();

    // Each branch produces a `resolved` CTE with identical output columns so the final SELECT is shared.
    let resolved_cte = match query.rel {
        RelatedRelation::Cites => format!(
            r#"resolved AS (
    SELECT e.edge_id, e.edge_kind, e.edge_source, e.payload,
           td.document_id, td.source_uid, td.citation, td.title,
           td.valid_from::text AS valid_from, td.valid_to::text AS valid_to,
           td.valid_to_raw, td.source_url
    FROM graph_edges e
    JOIN documents td
      ON td.source = 'legi' AND td.kind = 'article'
     AND td.source_uid = e.payload->>'to_source_uid'
    WHERE e.edge_source = 'publisher'
      AND e.from_document_id = {id}
      AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
      AND e.payload->'attributes' @> {CITATION_FILTER}
      AND td.document_id <> {id}
    ORDER BY td.source_uid
    LIMIT {limit}
)"#
        ),
        RelatedRelation::CitedBy => format!(
            r#"seed AS (SELECT source_uid FROM documents WHERE document_id = {id}),
resolved AS (
    SELECT e.edge_id, e.edge_kind, e.edge_source, e.payload,
           fd.document_id, fd.source_uid, fd.citation, fd.title,
           fd.valid_from::text AS valid_from, fd.valid_to::text AS valid_to,
           fd.valid_to_raw, fd.source_url
    FROM graph_edges e
    JOIN seed s ON e.payload->>'to_source_uid' = s.source_uid
    JOIN documents fd ON fd.document_id = e.from_document_id
    WHERE e.edge_source = 'publisher'
      AND e.payload->'attributes' @> {CITATION_FILTER}
      AND fd.document_id <> {id}
    ORDER BY fd.source_uid
    LIMIT {limit}
)"#
        ),
        RelatedRelation::Temporal => format!(
            r#"resolved AS (
    SELECT e.edge_id, e.edge_kind, e.edge_source, e.payload,
           td.document_id, td.source_uid, td.citation, td.title,
           td.valid_from::text AS valid_from, td.valid_to::text AS valid_to,
           td.valid_to_raw, td.source_url
    FROM graph_edges e
    JOIN documents td
      ON td.source = 'legi' AND td.kind = 'article'
     AND td.source_uid = e.payload->>'to_source_uid'
    WHERE e.edge_source = 'publisher'
      AND e.from_document_id = {id}
      AND e.payload->>'source_tag' = 'LIEN_ART'
      AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
      AND e.payload->'attributes' @> {VERSION_FILTER}
      AND td.document_id <> {id}
    ORDER BY td.valid_from, td.source_uid
    LIMIT {limit}
)"#
        ),
    };

    postgres.execute_sql(&format!(
        r#"
WITH {resolved_cte}
SELECT jsonb_build_object(
    'id', {id},
    'rel', '{rel}',
    'depth', 1,
    'returned', (SELECT count(*) FROM resolved),
    'neighbours', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'rel', '{rel}',
            'direction', '{direction}',
            'depth', 1,
            'document', jsonb_build_object(
                'document_id', r.document_id,
                'source_uid', r.source_uid,
                'citation', r.citation,
                'title', r.title,
                'validity', jsonb_build_object(
                    'from', r.valid_from, 'to', r.valid_to,
                    'to_raw', r.valid_to_raw, 'to_exclusive', true
                ),
                'source_url', r.source_url
            ),
            'edge', jsonb_build_object(
                'edge_id', r.edge_id,
                'edge_kind', r.edge_kind,
                'edge_source', r.edge_source,
                'source_tag', r.payload->>'source_tag',
                'attributes', r.payload->'attributes'
            ),
            'authority', jsonb_build_object(
                'score', 1.0, 'label', 'publisher', 'confidence', 'high',
                'reasons', jsonb_build_array('publisher_edge', 'target_resolved_by_source_uid')
            )
        ) ORDER BY r.document_id)
        FROM resolved r
    ), '[]'::jsonb),
    'pagination', jsonb_build_object(
        'limit', {limit},
        'possibly_truncated', (SELECT count(*) FROM resolved) >= {limit}
    )
)::text;
"#
    ))
}
