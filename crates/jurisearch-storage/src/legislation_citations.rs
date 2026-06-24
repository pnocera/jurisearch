//! Legislation-citation enrichment storage (migration v17).
//!
//! Citations are extracted from the archived Judilibre `/decision` responses (`official_api_responses`)
//! into per-decision OCCURRENCES (`decision_legislation_citations`), then DEDUPED by a normalized
//! `citation_key` into unique RESOLUTIONS (`legislation_citation_resolutions`) that are resolved against
//! the Legifrance API exactly once each. Reads go through `execute_sql` (JSON); writes use a
//! parameterized client (like the other ingestion writes).

use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

/// Page the LATEST archived Judilibre `/decision` response per decision that carries a `visa`, for
/// citation extraction. Keyset on `subject_document_id` (exclusive `after_cursor`); de-duplicates
/// re-fetches by taking the highest `response_id` per decision. Returns
/// `{ "decisions": [{response_id, document_id, source_uid, visa}], "next_cursor": <last id|null> }`.
pub fn load_archived_decisions_with_visa_json(
    postgres: &ManagedPostgres,
    after_cursor: Option<&str>,
    limit: u32,
) -> Result<String, StorageError> {
    let cursor_predicate = after_cursor
        .map(|cursor| format!("AND subject_document_id > {}", sql_string_literal(cursor)))
        .unwrap_or_default();
    let limit = limit.max(1);
    postgres.execute_sql(&format!(
        r#"
WITH latest AS (
    SELECT DISTINCT ON (subject_document_id)
           response_id, subject_document_id, subject_source_uid, response_json
    FROM official_api_responses
    WHERE provider = 'judilibre'
      AND endpoint = '/cassation/judilibre/v1.0/decision'
      AND outcome = 'ok'
      AND subject_document_id IS NOT NULL
      AND response_json ? 'visa'
      AND jsonb_typeof(response_json->'visa') = 'array'
      AND jsonb_array_length(response_json->'visa') > 0
      {cursor_predicate}
    ORDER BY subject_document_id, response_id DESC
)
SELECT jsonb_build_object(
    'decisions', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'response_id', response_id,
            'document_id', subject_document_id,
            'source_uid', subject_source_uid,
            'visa', response_json->'visa'
        ) ORDER BY subject_document_id)
        FROM (SELECT * FROM latest ORDER BY subject_document_id LIMIT {limit}) page
    ), '[]'::jsonb),
    'next_cursor', (SELECT max(subject_document_id) FROM (SELECT subject_document_id FROM latest ORDER BY subject_document_id LIMIT {limit}) p)
)::text;
"#
    ))
}

/// One per-decision citation occurrence to record (idempotent: ON CONFLICT DO NOTHING on the
/// `(decision_document_id, visa_index, citation_key)` unique).
pub struct InsertCitationOccurrence<'a> {
    pub decision_document_id: &'a str,
    pub decision_source_uid: &'a str,
    pub source_response_id: i64,
    pub visa_index: i32,
    pub citation_key: &'a str,
    pub article_number_raw: Option<&'a str>,
    pub article_number_norm: &'a str,
    pub code_name_raw: Option<&'a str>,
    pub code_name_norm: &'a str,
    pub canonical_query: &'a str,
    pub legifrance_url: Option<&'a str>,
    pub raw_title: &'a str,
    pub extraction_method: &'a str,
}

/// Insert one citation occurrence; returns `true` when a new row was written (idempotent re-collect).
pub fn insert_citation_occurrence_with_client<C: postgres::GenericClient>(
    client: &mut C,
    occurrence: &InsertCitationOccurrence<'_>,
) -> Result<bool, StorageError> {
    let occurrence_id = format!(
        "{}#{}#{}",
        occurrence.decision_document_id, occurrence.visa_index, occurrence.citation_key
    );
    let affected = client
        .execute(
            "INSERT INTO decision_legislation_citations (\
                 citation_occurrence_id, decision_document_id, decision_source_uid, source_response_id, \
                 visa_index, citation_key, article_number_raw, article_number_norm, code_name_raw, \
                 code_name_norm, canonical_query, legifrance_url, raw_title, extraction_method) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) \
             ON CONFLICT (decision_document_id, visa_index, citation_key) DO NOTHING;",
            &[
                &occurrence_id,
                &occurrence.decision_document_id,
                &occurrence.decision_source_uid,
                &occurrence.source_response_id,
                &occurrence.visa_index,
                &occurrence.citation_key,
                &occurrence.article_number_raw,
                &occurrence.article_number_norm,
                &occurrence.code_name_raw,
                &occurrence.code_name_norm,
                &occurrence.canonical_query,
                &occurrence.legifrance_url,
                &occurrence.raw_title,
                &occurrence.extraction_method,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(affected > 0)
}

/// Upsert the deduped resolution row for a citation_key as `pending` (no-op on the dedup fields if it
/// already exists; never resets a resolved row's Legifrance status).
pub fn upsert_citation_resolution_pending_with_client<C: postgres::GenericClient>(
    client: &mut C,
    citation_key: &str,
    article_number_norm: &str,
    code_name_norm: &str,
    canonical_query: &str,
) -> Result<(), StorageError> {
    client
        .execute(
            "INSERT INTO legislation_citation_resolutions (\
                 citation_key, article_number_norm, code_name_norm, canonical_query) \
             VALUES ($1,$2,$3,$4) \
             ON CONFLICT (citation_key) DO NOTHING;",
            &[
                &citation_key,
                &article_number_norm,
                &code_name_norm,
                &canonical_query,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Recompute `occurrence_count` on every resolution from the occurrence table (collect finalize).
pub fn finalize_citation_occurrence_counts(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    postgres
        .execute_sql(
            "UPDATE legislation_citation_resolutions r \
             SET occurrence_count = (\
                 SELECT count(*) FROM decision_legislation_citations c \
                 WHERE c.citation_key = r.citation_key), \
                 updated_at = now();",
        )
        .map(|_| ())
}

/// Page unique citation resolutions still needing a Legifrance call (`pending`, or `upstream_error`
/// when `retry_errors`). Keyset on `citation_key`. Returns
/// `{ "citations": [{citation_key, article_number_norm, code_name_norm, canonical_query}], "next_cursor" }`.
pub fn load_pending_citation_resolutions_json(
    postgres: &ManagedPostgres,
    after_cursor: Option<&str>,
    retry_errors: bool,
    limit: u32,
) -> Result<String, StorageError> {
    let cursor_predicate = after_cursor
        .map(|cursor| format!("AND citation_key > {}", sql_string_literal(cursor)))
        .unwrap_or_default();
    let status_predicate = if retry_errors {
        "legifrance_status IN ('pending','upstream_error','parse_error')"
    } else {
        "legifrance_status = 'pending'"
    };
    let limit = limit.max(1);
    postgres.execute_sql(&format!(
        r#"
WITH page AS (
    SELECT citation_key, article_number_norm, code_name_norm, canonical_query
    FROM legislation_citation_resolutions
    WHERE {status_predicate}
      {cursor_predicate}
    ORDER BY citation_key
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'citations', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'citation_key', citation_key,
            'article_number_norm', article_number_norm,
            'code_name_norm', code_name_norm,
            'canonical_query', canonical_query
        ) ORDER BY citation_key)
        FROM page
    ), '[]'::jsonb),
    'next_cursor', (SELECT max(citation_key) FROM page)
)::text;
"#
    ))
}

/// Record the result of a Legifrance call for one citation_key.
pub fn update_citation_resolution_with_client<C: postgres::GenericClient>(
    client: &mut C,
    citation_key: &str,
    legifrance_status: &str,
    legifrance_response_id: Option<i64>,
    legifrance_request_fingerprint: Option<&str>,
    error: Option<&str>,
) -> Result<(), StorageError> {
    client
        .execute(
            "UPDATE legislation_citation_resolutions \
             SET legifrance_status = $2, legifrance_response_id = $3, \
                 legifrance_request_fingerprint = $4, error = $5, fetched_at = now(), updated_at = now() \
             WHERE citation_key = $1;",
            &[
                &citation_key,
                &legifrance_status,
                &legifrance_response_id,
                &legifrance_request_fingerprint,
                &error,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Coverage report for the legislation-citation enrichment (`status` / command reports).
pub fn legislation_citations_coverage_json(
    postgres: &ManagedPostgres,
) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT jsonb_build_object(
    'occurrences', (SELECT count(*) FROM decision_legislation_citations),
    'decisions_with_citations', (SELECT count(DISTINCT decision_document_id) FROM decision_legislation_citations),
    'unique_citations', (SELECT count(*) FROM legislation_citation_resolutions),
    'by_legifrance_status', COALESCE((
        SELECT jsonb_agg(jsonb_build_object('status', status, 'count', n) ORDER BY status)
        FROM (SELECT legifrance_status AS status, count(*) AS n
              FROM legislation_citation_resolutions GROUP BY legifrance_status) s
    ), '[]'::jsonb)
)::text;
"#,
    )
}
