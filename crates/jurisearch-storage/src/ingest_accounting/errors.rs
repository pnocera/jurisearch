//! Ingest error rows + class counts.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestErrorInput<'a> {
    pub run_id: &'a str,
    pub member_id: Option<i64>,
    pub error_class: &'a str,
    pub error_code: &'a str,
    pub message: &'a str,
    pub retry_policy: &'a str,
    pub context_json: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestErrorClassCount {
    pub error_class: String,
    pub error_code: String,
    pub count: i64,
}

pub fn record_ingest_error(
    postgres: &ManagedPostgres,
    input: &IngestErrorInput<'_>,
) -> Result<i64, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let error_id = record_ingest_error_with_client(&mut transaction, input)?;
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(error_id)
}

pub fn record_ingest_error_with_client<C: GenericClient>(
    client: &mut C,
    input: &IngestErrorInput<'_>,
) -> Result<i64, StorageError> {
    let row = client
        .query_one(
            "INSERT INTO ingest_error \
                (run_id, member_id, error_class, error_code, message, retry_policy, context) \
             VALUES ($1, $2, $3, $4, $5, $6, COALESCE($7::text::jsonb, '{}'::jsonb)) \
             RETURNING error_id;",
            &[
                &input.run_id,
                &input.member_id,
                &input.error_class,
                &input.error_code,
                &input.message,
                &input.retry_policy,
                &input.context_json,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    if let Some(member_id) = input.member_id {
        client
            .execute(
                "UPDATE ingest_member \
                 SET error_count = error_count + 1, \
                     last_error_class = $2, \
                     last_error_code = $3, \
                     last_error_message = $4, \
                     updated_at = now() \
                 WHERE member_id = $1;",
                &[
                    &member_id,
                    &input.error_class,
                    &input.error_code,
                    &input.message,
                ],
            )
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(row.get(0))
}
