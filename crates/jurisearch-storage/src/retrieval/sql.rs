//! Shared SQL builders/predicates: ranked-candidate CTEs, cursor/version predicates, literals.

use super::*;

/// Format a finite, non-negative f64 as a plain SQL numeric literal (no locale/exponent surprises).
/// `rrf_weights` already guarantees finiteness; clamp defensively.
pub(crate) fn format_sql_f64(value: f64) -> String {
    let value = if value.is_finite() { value.max(0.0) } else { 0.0 };
    format!("{value:.6}")
}

/// Build a `%value%` LIKE pattern, escaping the LIKE metacharacters `\ % _` (escape char `\`).
pub(super) fn like_contains(value: &str) -> String {
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
pub(super) fn cursor_predicate(cursor: Option<RetrievalCursor<'_>>) -> String {
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
/// Shared with the zone retrieval path, which groups by `document_id` over `cursor_score` too.
pub(crate) fn document_cursor_predicate(cursor: Option<RetrievalCursor<'_>>) -> String {
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

pub(super) fn ranked_candidate_ctes(
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

pub(super) const CITATION_FILTER: &str =
    r#"'[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb"#;

pub(super) const VERSION_FILTER: &str =
    r#"'[{"key":"debut"},{"key":"fin"},{"key":"num"},{"key":"etat"}]'::jsonb"#;

/// The version family of an article: the target plus its LIEN_ART version-edge neighbours, with a
/// shared CTE body so `versions` and `diff` resolve the same set. `$id` is interpolated by the caller.
pub(super) fn version_family_cte(id: &str) -> String {
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
