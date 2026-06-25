//! Durable, append-only archive of official-API exchanges (migration v16, `official_api_responses`).
//!
//! Every PISTE/Judilibre/Legifrance request the enrichment makes is persisted here — raw response body
//! (byte-faithful) + parsed jsonb + a sha256 of the body — so the quota-limited upstream evidence is
//! never lost, even when the TTL'd `decision_zones` cache expires/refreshes/invalidates. Append-only:
//! a re-fetch inserts a NEW row (full history). Writes go through a parameterized client (like the other
//! ingestion writes), so an enrichment worker archives on its own connection.

use crate::runtime::StorageError;

/// One official-API exchange to archive. `provider='local'` / `http_method='LOCAL'` records a decision
/// we touched but could not even request (e.g. no parser-valid pourvoi), so every touched decision has
/// durable accounting.
pub struct InsertOfficialApiResponse<'a> {
    pub provider: &'a str,
    pub api_environment: &'a str,
    pub endpoint: &'a str,
    pub http_method: &'a str,
    pub subject_document_id: Option<&'a str>,
    pub subject_source_uid: Option<&'a str>,
    pub provider_object_id: Option<&'a str>,
    pub citation_key: Option<&'a str>,
    pub request_fingerprint: &'a str,
    pub request_url: Option<&'a str>,
    pub request_json: &'a serde_json::Value,
    pub request_body: Option<&'a str>,
    pub outcome: &'a str,
    pub http_status: Option<i32>,
    pub response_body: &'a str,
    pub response_json: Option<&'a serde_json::Value>,
    pub response_body_sha256: &'a str,
    pub error: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub code_version: Option<&'a str>,
}

/// Append one exchange to `official_api_responses`, returning its `response_id` (used to link a
/// `/decision` archive row to the citations later extracted from it, in v17).
pub fn insert_official_api_response_with_client<C: postgres::GenericClient>(
    client: &mut C,
    row: &InsertOfficialApiResponse<'_>,
) -> Result<i64, StorageError> {
    // jsonb columns are passed as text + cast in SQL (mirrors the other parameterized writers).
    let request_json =
        serde_json::to_string(row.request_json).map_err(|error| StorageError::Projection {
            message: format!("serialize official_api_responses.request_json: {error}"),
        })?;
    let response_json = match row.response_json {
        Some(value) => {
            Some(
                serde_json::to_string(value).map_err(|error| StorageError::Projection {
                    message: format!("serialize official_api_responses.response_json: {error}"),
                })?,
            )
        }
        None => None,
    };
    let inserted = client
        .query_one(
            "INSERT INTO official_api_responses (\
                 provider, api_environment, endpoint, http_method, \
                 subject_document_id, subject_source_uid, provider_object_id, citation_key, \
                 request_fingerprint, request_url, request_json, request_body, \
                 outcome, http_status, response_body, response_json, response_body_sha256, \
                 error, run_id, code_version) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::text::jsonb,$12,$13,$14,$15,$16::text::jsonb,$17,$18,$19,$20) \
             RETURNING response_id;",
            &[
                &row.provider,
                &row.api_environment,
                &row.endpoint,
                &row.http_method,
                &row.subject_document_id,
                &row.subject_source_uid,
                &row.provider_object_id,
                &row.citation_key,
                &row.request_fingerprint,
                &row.request_url,
                &request_json,
                &row.request_body,
                &row.outcome,
                &row.http_status,
                &row.response_body,
                &response_json,
                &row.response_body_sha256,
                &row.error,
                &row.run_id,
                &row.code_version,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(inserted.get::<_, i64>("response_id"))
}
