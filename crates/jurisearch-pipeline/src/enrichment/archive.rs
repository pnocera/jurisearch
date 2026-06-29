//! Shared official_api_responses archive helpers: hash + persist a raw upstream exchange.

use crate::*;

/// Run a unit of producer mutations + their outbox emits inside ONE transaction on `db`, committing
/// only if every write *and* its `emit_change` succeed (P1 §5.1: the emit must be rollback-coupled to
/// the mutation). The scheduled enrichment/citation commands hold a plain auto-commit
/// `postgres::Client` per worker; without this, a mutation could commit while its later outbox insert
/// fails, leaving a silent diff gap. `db.transaction()` is a real transaction for a `Client` and a
/// savepoint when already inside one, so this is safe in every calling context.
pub fn in_outbox_txn<C, T>(
    db: &mut C,
    unit: impl FnOnce(&mut postgres::Transaction<'_>) -> Result<T, ErrorObject>,
) -> Result<T, ErrorObject>
where
    C: postgres::GenericClient,
{
    let mut tx = db
        .transaction()
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let out = unit(&mut tx)?;
    tx.commit()
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    Ok(out)
}

/// Resolve a Cassation decision on Judilibre by pourvoi (+ date), fetch its zones, normalize and cache
/// them, and return the cached row. Errors are cached and yield `Ok(None)` (never fail `fetch`). Thin
/// wrapper that opens its own DB client + `PisteClient` and delegates to the thread-safe core, so the
/// shipped `fetch --part --online` path is unchanged while the eager backfill can fan out workers.
/// `sha256:<hex>` of a UTF-8 body, for the archive's `response_body_sha256` integrity column.
pub fn sha256_hex(data: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// Append one captured official-API exchange to the durable `official_api_responses` archive (v16).
/// Archive writes are hard errors: a decision is not "touched successfully" unless its raw upstream
/// evidence was persisted (the user requirement: keep ALL API call results).
// Each argument is a distinct, independently-optional archive dimension (subject ids, provider id,
// citation key, explicit corpus); bundling them into a struct would only move the verbosity to the
// call sites without improving clarity.
#[allow(clippy::too_many_arguments)]
pub fn archive_exchange<C: postgres::GenericClient>(
    db: &mut C,
    exchange: &OfficialApiExchange,
    api_environment: &str,
    subject_document_id: Option<&str>,
    subject_source_uid: Option<&str>,
    provider_object_id: Option<&str>,
    citation_key: Option<&str>,
    corpus: Option<&str>,
    outbox: Option<&jurisearch_storage::outbox::OutboxContext<'_>>,
) -> Result<i64, ErrorObject> {
    let response_body_sha256 = sha256_hex(&exchange.response_body);
    insert_official_api_response_with_client(
        db,
        &InsertOfficialApiResponse {
            provider: exchange.provider,
            api_environment,
            endpoint: &exchange.endpoint,
            http_method: exchange.http_method,
            subject_document_id,
            subject_source_uid,
            provider_object_id,
            citation_key,
            request_fingerprint: &exchange.request_fingerprint,
            request_url: Some(&exchange.request_url),
            request_json: &exchange.request_json,
            request_body: exchange.request_body.as_deref(),
            outcome: exchange.outcome.as_str(),
            http_status: exchange.http_status.map(i32::from),
            response_body: &exchange.response_body,
            response_json: exchange.response_json.as_ref(),
            response_body_sha256: &response_body_sha256,
            error: exchange.error.as_deref(),
            run_id: None,
            code_version: Some(CLI_CODE_VERSION),
            // `None` for a subject-document exchange (corpus derives from the document); the
            // Legifrance citation lookup passes its resolution's corpus explicitly (no subject doc).
            corpus,
        },
        outbox,
    )
    .map_err(storage_error_object)
}

/// Durable accounting for a decision we touched but could NOT request (no parser-valid pourvoi): a
/// `provider='local'`, `http_method='LOCAL'` archive row, so every touched decision is recorded.
pub fn archive_local_unsupported<C: postgres::GenericClient>(
    db: &mut C,
    document_id: &str,
    source_uid: &str,
    api_environment: &str,
    outbox: Option<&jurisearch_storage::outbox::OutboxContext<'_>>,
) -> Result<(), ErrorObject> {
    let empty = json!({});
    insert_official_api_response_with_client(
        db,
        &InsertOfficialApiResponse {
            provider: "local",
            api_environment,
            endpoint: "judilibre:unsupported-no-pourvoi",
            http_method: "LOCAL",
            subject_document_id: Some(document_id),
            subject_source_uid: Some(source_uid),
            provider_object_id: None,
            citation_key: None,
            request_fingerprint: "local:unsupported-no-pourvoi",
            request_url: None,
            request_json: &empty,
            request_body: None,
            outcome: "unsupported",
            http_status: None,
            response_body: "",
            response_json: None,
            response_body_sha256: &sha256_hex(""),
            error: Some("no parser-valid pourvoi; not resolvable on Judilibre"),
            run_id: None,
            code_version: Some(CLI_CODE_VERSION),
            // Corpus derives from the subject decision document.
            corpus: None,
        },
        outbox,
    )
    .map(|_| ())
    .map_err(storage_error_object)
}
