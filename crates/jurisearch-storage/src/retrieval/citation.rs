//! Structured LEGI citation resolution JSON.

use super::*;

/// Resolve a citation-shaped query structurally: LEGI articles whose `citation`/`title` contain the
/// article reference (and optional code hint) and are valid at `as_of`. Returns the SAME JSON shape
/// as [`hybrid_candidates_json`] (a `candidates` array keyed on `document_id`), so callers can route
/// to either backend transparently. The as-of validity filter yields one version per matched
/// article, and exact-citation matches are ranked first.
pub fn resolve_legi_citation_in_snapshot(
    snapshot: &mut dyn ReadSnapshot,
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

    snapshot.read_text(&format!(
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
