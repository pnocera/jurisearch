//! Hybrid (BM25+dense RRF) candidate retrieval JSON.

use super::*;

/// Fail-closed embedding-fingerprint preflight (work/09 P3A): before any dense/hybrid retrieval,
/// require that the query's embedding fingerprint matches the **active generation's** fingerprint
/// (`corpus_state.embedding_fingerprint`). Today a mismatch is merely a SQL filter that returns zero
/// dense rows, so hybrid silently degrades to lexical and explicit-dense returns a false no-results;
/// this turns that into a clear error BEFORE retrieval. Single-corpus in 3A: no active corpus keeps the
/// legacy `public` path, and more than one active corpus is deferred to 3C (multi-corpus fan-out).
fn ensure_embedding_fingerprint_compatible(
    postgres: &ManagedPostgres,
    query: &HybridCandidateQuery<'_>,
) -> Result<(), StorageError> {
    if !query.retrieval_mode.uses_dense() {
        return Ok(());
    }
    let mut client = postgres.client()?;
    let rows = client
        .query(
            "SELECT embedding_fingerprint FROM jurisearch_control.corpus_state ORDER BY corpus;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    match rows.as_slice() {
        // No active corpus: the `public` producer/local working set — keep the legacy path (the
        // active-generation preflight is a site/client concern).
        [] => Ok(()),
        [row] => {
            let active: String = row.get(0);
            if query.embedding_fingerprint == Some(active.as_str()) {
                Ok(())
            } else {
                Err(StorageError::Retrieval {
                    message: format!(
                        "embedding_fingerprint_mismatch: query fingerprint {:?} does not match the \
                         active generation fingerprint `{active}`; refusing dense/hybrid retrieval \
                         (it would silently degrade to lexical or return false no-results)",
                        query.embedding_fingerprint
                    ),
                })
            }
        }
        _ => Err(StorageError::Retrieval {
            message: "multi-corpus embedding-fingerprint preflight is deferred to work/09 3C \
                      (multi-corpus fan-out)"
                .to_owned(),
        }),
    }
}

pub fn hybrid_candidates_json(
    postgres: &ManagedPostgres,
    query: &HybridCandidateQuery<'_>,
) -> Result<String, StorageError> {
    ensure_embedding_fingerprint_compatible(postgres, query)?;
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
        let stored_probes = manifest_default_probes(postgres, "embedding")?;
        let probes = effective_probes(&query.options, stored_probes);
        format!("SET ivfflat.probes = {probes};\n\n")
    } else {
        String::new()
    };
    let limit = query.limit;

    // A2 gate: project `publication` ONLY when the authority re-rank needs it. Both fragments are the
    // empty string when OFF, so the emitted SQL is byte-identical to before this field existed (the
    // OFF-path invariant). Shared by the chunk and document branches so the two stay in lockstep.
    let publication_select = if query.project_authority {
        "\n        d.canonical_json->>'publication' AS publication,"
    } else {
        ""
    };
    let publication_json = if query.project_authority {
        "\n            'publication', publication,"
    } else {
        ""
    };

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
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,{publication_select}
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
            'source_url', source_url, 'snippet', snippet,{publication_json}
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
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,{publication_select}
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
            'source_url', source_url, 'snippet', snippet,{publication_json}
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
    postgres.execute_read_sql(&sql)
}
