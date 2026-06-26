//! Version timeline + as-of diff JSON.

use super::*;

/// Version timeline for an article (`versions`): every member of its version family ordered by
/// validity start. Each entry carries validity window + citation; the requested id is flagged.
pub fn document_versions_json(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    postgres.execute_read_sql(&format!(
        r#"
WITH {family}
SELECT jsonb_build_object(
    'id', {id},
    'count', (SELECT count(*) FROM family),
    'versions', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'document_id', document_id,
            'source_uid', source_uid,
            'citation', citation,
            'title', title,
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'is_target', (document_id = {id})
        ) ORDER BY valid_from NULLS FIRST, document_id)
        FROM family
    ), '[]'::jsonb)
)::text;
"#,
        family = version_family_cte(&id)
    ))
}

/// Compare the article versions in force on two dates (`diff`). Returns the family member valid on
/// each date (full record incl. body) and whether the version changed between them.
pub fn document_diff_json(
    postgres: &ManagedPostgres,
    document_id: &str,
    from: &str,
    to: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    let from_lit = sql_string_literal(from);
    let to_lit = sql_string_literal(to);
    postgres.execute_read_sql(&format!(
        r#"
WITH {family},
from_version AS (
    SELECT * FROM family f
    WHERE (f.valid_from IS NULL OR f.valid_from <= {from_lit})
      AND (f.valid_to IS NULL OR f.valid_to > {from_lit})
    ORDER BY f.valid_from DESC NULLS LAST LIMIT 1
),
to_version AS (
    SELECT * FROM family f
    WHERE (f.valid_from IS NULL OR f.valid_from <= {to_lit})
      AND (f.valid_to IS NULL OR f.valid_to > {to_lit})
    ORDER BY f.valid_from DESC NULLS LAST LIMIT 1
)
SELECT jsonb_build_object(
    'id', {id},
    'from', {from_lit},
    'to', {to_lit},
    'family_count', (SELECT count(*) FROM family),
    'from_version', (SELECT to_jsonb(f) FROM from_version f),
    'to_version', (SELECT to_jsonb(t) FROM to_version t),
    'changed', (
        (SELECT document_id FROM from_version) IS DISTINCT FROM (SELECT document_id FROM to_version)
    )
)::text;
"#,
        family = version_family_cte(&id)
    ))
}
