//! Structural context (ancestry/siblings) JSON.

use super::*;

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

    postgres.execute_read_sql(&format!(
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
