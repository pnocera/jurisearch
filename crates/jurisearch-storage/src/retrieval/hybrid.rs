//! Hybrid (BM25+dense RRF) candidate retrieval JSON.

use super::*;

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
    // Decision-metadata filters are applied alongside the kind filter in every candidate CTE.
    let filter_predicate = format!("{kind_predicate}{}", query.decision_filters.predicate());
    let ranked_ctes = ranked_candidate_ctes(query, &query_text, &as_of, &filter_predicate)?;
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
