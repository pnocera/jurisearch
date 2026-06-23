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

#[derive(Debug, Clone, Copy)]
pub struct HybridCandidateQuery<'a> {
    pub query_text: &'a str,
    pub query_embedding: Option<&'a str>,
    pub embedding_fingerprint: Option<&'a str>,
    pub retrieval_mode: RetrievalMode,
    pub after_cursor: Option<RetrievalCursor<'a>>,
    pub as_of: &'a str,
    pub kind_filter: Option<&'a str>,
    pub lexical_limit: u32,
    pub dense_limit: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct RetrievalCursor<'a> {
    pub score: &'a str,
    pub chunk_id: &'a str,
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
    let cursor_predicate = cursor_predicate(query.after_cursor);
    let ranked_ctes = ranked_candidate_ctes(query, &query_text, &as_of, &kind_predicate)?;
    let set_ivfflat_probes = if query.retrieval_mode.uses_dense() {
        "SET ivfflat.probes = 4;\n\n"
    } else {
        ""
    };

    postgres.execute_sql(&format!(
        r#"
{set_ivfflat_probes}WITH {ranked_ctes},
limited AS (
    SELECT
        r.chunk_id,
        c.document_id,
        d.source,
        d.kind,
        d.citation,
        d.title,
        d.source_url,
        d.valid_from::text AS valid_from,
        d.valid_to::text AS valid_to,
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,
        r.lexical_rank,
        r.dense_rank,
        r.fused_score
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
                    'rrf', round(fused_score::numeric, 8),
                    'lexical_rank', lexical_rank,
                    'dense_rank', dense_rank
                ),
                'cursor', concat(round(fused_score::numeric, 8)::text, ':', chunk_id)
            )
            ORDER BY round(fused_score::numeric, 8) DESC, chunk_id
        )
        FROM limited
    ), '[]'::jsonb)
)::text;
"#,
        set_ivfflat_probes = set_ivfflat_probes,
        ranked_ctes = ranked_ctes,
        query_text = query_text,
        retrieval_mode = retrieval_mode,
        as_of = as_of,
        cursor_predicate = cursor_predicate,
        limit = query.limit
    ))
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
    let article_pattern = sql_string_literal(&like_contains(&format!(
        "article {}",
        query.article_number.trim().to_lowercase()
    )));
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
      AND lower(concat_ws(' ', d.citation, d.title)) LIKE {article_pattern} ESCAPE '\'
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

fn cursor_predicate(cursor: Option<RetrievalCursor<'_>>) -> String {
    cursor
        .map(|cursor| {
            let score = sql_string_literal(cursor.score);
            let chunk_id = sql_string_literal(cursor.chunk_id);
            format!(
                "WHERE (round(r.fused_score::numeric, 8) < {score}::numeric \
                 OR (round(r.fused_score::numeric, 8) = {score}::numeric \
                 AND r.chunk_id > {chunk_id}))"
            )
        })
        .unwrap_or_default()
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
            let (lexical_weight, dense_weight) = rrf_weights();
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
