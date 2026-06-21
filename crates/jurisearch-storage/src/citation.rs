use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

#[derive(Debug, Clone, Copy)]
pub struct CitationLookupQuery<'a> {
    pub lookup: CitationLookup<'a>,
    pub limit: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum CitationLookup<'a> {
    DocumentId {
        document_id: &'a str,
        source_uid: Option<&'a str>,
    },
    ArticleSourceUid(&'a str),
    TextSourceUid(&'a str),
    SectionSourceUid(&'a str),
    Nor(&'a str),
    FreeTextArticle {
        article_number: &'a str,
        code_hint: Option<&'a str>,
    },
}

pub fn citation_lookup_json(
    postgres: &ManagedPostgres,
    query: &CitationLookupQuery<'_>,
) -> Result<String, StorageError> {
    let limit = query.limit.max(1);
    let union_sql = match query.lookup {
        CitationLookup::DocumentId {
            document_id,
            source_uid,
        } => document_id_lookup_sql(document_id, source_uid),
        CitationLookup::ArticleSourceUid(source_uid) => document_lookup_sql(
            &format!("d.source_uid = {}", sql_string_literal(source_uid)),
            "TRUE",
        ),
        CitationLookup::TextSourceUid(source_uid) => metadata_lookup_sql(
            &format!(
                "m.source_uid = {} AND m.root_kind IN ('TEXTE_VERSION', 'TEXTELR')",
                sql_string_literal(source_uid)
            ),
            "TRUE",
        ),
        CitationLookup::SectionSourceUid(source_uid) => metadata_lookup_sql(
            &format!(
                "m.source_uid = {} AND m.root_kind = 'SECTION_TA'",
                sql_string_literal(source_uid)
            ),
            "TRUE",
        ),
        CitationLookup::Nor(nor) => metadata_lookup_sql(
            &format!(
                "upper(m.canonical_json->>'nor') = {} AND m.root_kind = 'TEXTELR'",
                sql_string_literal(&nor.to_ascii_uppercase())
            ),
            "TRUE",
        ),
        CitationLookup::FreeTextArticle {
            article_number,
            code_hint,
        } => free_text_article_lookup_sql(article_number, code_hint),
    };

    postgres.execute_sql(&format!(
        r#"
WITH matches AS (
{union_sql}
)
SELECT jsonb_build_object(
    'matches', COALESCE((
        SELECT jsonb_agg(
            jsonb_build_object(
                'target_type', target_type,
                'document_id', document_id,
                'metadata_key', metadata_key,
                'source', source,
                'kind', kind,
                'source_uid', source_uid,
                'version_group', version_group,
                'root_kind', root_kind,
                'parent_source_uid', parent_source_uid,
                'citation', citation,
                'title', title,
                'nor', nor,
                'validity', jsonb_build_object(
                    'from', valid_from,
                    'to', valid_to,
                    'to_raw', valid_to_raw,
                    'to_exclusive', true
                ),
                'source_url', source_url,
                'source_payload_hash', source_payload_hash,
                'exact_identifier_match', exact_identifier_match
            )
            ORDER BY exact_identifier_match DESC, valid_from DESC NULLS LAST, target_type, source_uid, document_id, metadata_key
        )
        FROM (
            SELECT *
            FROM matches
            ORDER BY exact_identifier_match DESC, valid_from DESC NULLS LAST, target_type, source_uid, document_id, metadata_key
            LIMIT {limit}
        ) limited
    ), '[]'::jsonb)
)::text;
"#,
        union_sql = union_sql,
        limit = limit
    ))
}

fn document_id_lookup_sql(document_id: &str, source_uid: Option<&str>) -> String {
    let document_id_literal = sql_string_literal(document_id);
    let source_predicate = source_uid
        .map(|source_uid| format!(" OR d.source_uid = {}", sql_string_literal(source_uid)))
        .unwrap_or_default();
    document_lookup_sql(
        &format!("d.document_id = {document_id_literal}{source_predicate}"),
        &format!("d.document_id = {document_id_literal}"),
    )
}

fn document_lookup_sql(predicate: &str, exact_expr: &str) -> String {
    format!(
        r#"
    SELECT
        'document'::text AS target_type,
        d.document_id,
        NULL::text AS metadata_key,
        d.source,
        d.kind,
        d.source_uid,
        d.version_group,
        NULL::text AS root_kind,
        NULL::text AS parent_source_uid,
        d.citation,
        d.title,
        NULL::text AS nor,
        d.valid_from::text AS valid_from,
        d.valid_to::text AS valid_to,
        d.valid_to_raw,
        d.source_url,
        d.source_payload_hash,
        ({exact_expr}) AS exact_identifier_match
    FROM documents d
    WHERE {predicate}
"#
    )
}

fn metadata_lookup_sql(predicate: &str, exact_expr: &str) -> String {
    format!(
        r#"
    SELECT
        'metadata_root'::text AS target_type,
        NULL::text AS document_id,
        m.metadata_key,
        'legi'::text AS source,
        lower(m.root_kind)::text AS kind,
        m.source_uid,
        NULL::text AS version_group,
        m.root_kind,
        m.parent_source_uid,
        NULL::text AS citation,
        m.title,
        m.canonical_json->>'nor' AS nor,
        m.valid_from::text AS valid_from,
        m.valid_to::text AS valid_to,
        m.valid_to_raw,
        NULL::text AS source_url,
        m.source_payload_hash,
        ({exact_expr}) AS exact_identifier_match
    FROM legi_metadata_roots m
    WHERE {predicate}
"#
    )
}

fn free_text_article_lookup_sql(article_number: &str, code_hint: Option<&str>) -> String {
    let article_pattern = sql_string_literal(&format!("%article {}%", article_number));
    let mut predicate = format!(
        "d.kind = 'article' \
         AND lower(concat_ws(' ', d.citation, d.title)) LIKE {article_pattern}"
    );
    if let Some(code_hint) = code_hint {
        let code_pattern = sql_string_literal(&format!("%{}%", code_hint));
        predicate.push_str(&format!(
            " AND lower(concat_ws(' ', d.citation, d.title)) LIKE {code_pattern}"
        ));
    }
    document_lookup_sql(&predicate, "FALSE")
}
