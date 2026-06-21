use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

// Dense ANN candidates are post-filtered by document validity, so fetch a
// wider pool before assigning gap-free dense ranks.
const DENSE_TEMPORAL_OVERFETCH_FACTOR: u32 = 4;
const DEFAULT_CONTEXT_SIBLING_LIMIT: u32 = 50;

#[derive(Debug, Clone, Copy)]
pub struct HybridCandidateQuery<'a> {
    pub query_text: &'a str,
    pub query_embedding: &'a str,
    pub embedding_fingerprint: &'a str,
    pub as_of: &'a str,
    pub kind_filter: Option<&'a str>,
    pub lexical_limit: u32,
    pub dense_limit: u32,
    pub limit: u32,
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
    let query_embedding = sql_string_literal(query.query_embedding);
    let embedding_fingerprint = sql_string_literal(query.embedding_fingerprint);
    let as_of = sql_string_literal(query.as_of);
    let kind_predicate = query
        .kind_filter
        .map(|kind| format!("AND d.kind = {}", sql_string_literal(kind)))
        .unwrap_or_default();
    let dense_pool_limit = query
        .dense_limit
        .saturating_mul(DENSE_TEMPORAL_OVERFETCH_FACTOR)
        .max(query.dense_limit);

    postgres.execute_sql(&format!(
        r#"
SET ivfflat.probes = 4;

WITH lexical AS (
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
            CASE WHEN f.lexical_rank IS NULL THEN 0.0 ELSE 1.0 / (60.0 + f.lexical_rank) END
            + CASE WHEN f.dense_rank IS NULL THEN 0.0 ELSE 1.0 / (60.0 + f.dense_rank) END
        ) AS fused_score
    FROM fused f
),
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
    ORDER BY r.fused_score DESC, r.chunk_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'query', {query_text},
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
            ORDER BY fused_score DESC, chunk_id
        )
        FROM limited
    ), '[]'::jsonb)
)::text;
"#,
        query_text = query_text,
        query_embedding = query_embedding,
        embedding_fingerprint = embedding_fingerprint,
        as_of = as_of,
        kind_predicate = kind_predicate,
        lexical_limit = query.lexical_limit,
        dense_limit = query.dense_limit,
        dense_pool_limit = dense_pool_limit,
        limit = query.limit
    ))
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
