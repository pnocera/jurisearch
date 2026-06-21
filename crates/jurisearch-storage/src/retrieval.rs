use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

// Dense ANN candidates are post-filtered by document validity, so fetch a
// wider pool before assigning gap-free dense ranks.
const DENSE_TEMPORAL_OVERFETCH_FACTOR: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub struct HybridCandidateQuery<'a> {
    pub query_text: &'a str,
    pub query_embedding: &'a str,
    pub embedding_fingerprint: &'a str,
    pub as_of: &'a str,
    pub lexical_limit: u32,
    pub dense_limit: u32,
    pub limit: u32,
}

pub fn hybrid_candidates_json(
    postgres: &ManagedPostgres,
    query: &HybridCandidateQuery<'_>,
) -> Result<String, StorageError> {
    let query_text = sql_string_literal(query.query_text);
    let query_embedding = sql_string_literal(query.query_embedding);
    let embedding_fingerprint = sql_string_literal(query.embedding_fingerprint);
    let as_of = sql_string_literal(query.as_of);
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
    WHERE c.body @@@ {query_text}
      AND (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
      AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
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
                'lexical_rank', lexical_rank,
                'dense_rank', dense_rank,
                'fused_score', round(fused_score::numeric, 8)
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
        lexical_limit = query.lexical_limit,
        dense_limit = query.dense_limit,
        dense_pool_limit = dense_pool_limit,
        limit = query.limit
    ))
}
