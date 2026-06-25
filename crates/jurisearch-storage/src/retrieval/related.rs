//! Depth-1 graph neighbours JSON.

use super::*;

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
        // Statute → interpreting jurisprudence: decisions whose official CITATION edges resolve to
        // this article's source_uid. Same indexed CITATION/cible path as cited_by, restricted to
        // decision neighbours. Never asserts jurisprudence constante — just ranked candidates.
        RelatedRelation::InterpretedBy => format!(
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
      AND fd.kind = 'decision'
      AND fd.document_id <> {id}
    ORDER BY fd.valid_from DESC NULLS LAST, fd.source_uid
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
