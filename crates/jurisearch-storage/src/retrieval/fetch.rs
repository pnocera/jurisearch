//! Exact version-pinned document fetch JSON.

use super::*;

pub fn fetch_documents_in_snapshot(
    snapshot: &mut dyn ReadSnapshot,
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

    snapshot.read_text(&format!(
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
