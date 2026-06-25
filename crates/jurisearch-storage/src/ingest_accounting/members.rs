//! Per-member ingest accounting (status, record, status updates).

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestMemberStatus {
    Discovered,
    Parsed,
    Inserted,
    Skipped,
    Failed,
}

impl IngestMemberStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Parsed => "parsed",
            Self::Inserted => "inserted",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    pub(super) fn from_db(value: &str) -> Result<Self, StorageError> {
        match value {
            "discovered" => Ok(Self::Discovered),
            "parsed" => Ok(Self::Parsed),
            "inserted" => Ok(Self::Inserted),
            "skipped" => Ok(Self::Skipped),
            "failed" => Ok(Self::Failed),
            _ => Err(StorageError::IngestAccounting {
                message: format!("unknown ingest member status `{value}`"),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestMemberInput<'a> {
    pub run_id: &'a str,
    pub archive_name: &'a str,
    pub member_path: &'a str,
    pub source: &'a str,
    pub source_entity: Option<&'a str>,
    pub date_anchor: Option<&'a str>,
    pub status: IngestMemberStatus,
    pub compatibility: IngestCompatibility<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestMemberRecord {
    pub member_id: i64,
    pub attempt_count: i32,
    pub status: IngestMemberStatus,
}

pub fn record_ingest_member(
    postgres: &ManagedPostgres,
    input: &IngestMemberInput<'_>,
) -> Result<IngestMemberRecord, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    record_ingest_member_with_client(&mut client, input)
}

pub fn record_ingest_member_with_client<C: GenericClient>(
    client: &mut C,
    input: &IngestMemberInput<'_>,
) -> Result<IngestMemberRecord, StorageError> {
    let row = client
        .query_one(
            "INSERT INTO ingest_member \
                (run_id, archive_name, member_path, source, source_entity, date_anchor, status, \
                 parser_version, schema_version, code_version, source_payload_hash) \
             VALUES \
                ($1, $2, $3, $4, $5, $6::text::date, $7, $8, $9, $10, $11) \
             ON CONFLICT (run_id, archive_name, member_path) DO UPDATE SET \
                source = EXCLUDED.source, \
                source_entity = EXCLUDED.source_entity, \
                date_anchor = EXCLUDED.date_anchor, \
                status = EXCLUDED.status, \
                parser_version = EXCLUDED.parser_version, \
                schema_version = EXCLUDED.schema_version, \
                code_version = EXCLUDED.code_version, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                attempt_count = ingest_member.attempt_count + 1, \
                updated_at = now() \
             RETURNING member_id, attempt_count, status;",
            &[
                &input.run_id,
                &input.archive_name,
                &input.member_path,
                &input.source,
                &input.source_entity,
                &input.date_anchor,
                &input.status.as_str(),
                &input.compatibility.parser_version,
                &input.compatibility.schema_version,
                &input.compatibility.code_version,
                &input.compatibility.source_payload_hash,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    let status: String = row.get(2);
    let member_id = row.get(0);
    let attempt_count = row.get(1);
    if input.status != IngestMemberStatus::Failed && attempt_count > 1 {
        client
            .execute(
                "UPDATE ingest_member \
                 SET error_count = 0, \
                     last_error_class = NULL, \
                     last_error_code = NULL, \
                     last_error_message = NULL \
                 WHERE member_id = $1;",
                &[&member_id],
            )
            .map_err(StorageError::PostgresClient)?;
        client
            .execute(
                "DELETE FROM ingest_error WHERE member_id = $1;",
                &[&member_id],
            )
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(IngestMemberRecord {
        member_id,
        attempt_count,
        status: IngestMemberStatus::from_db(&status)?,
    })
}

pub fn update_ingest_member_status(
    postgres: &ManagedPostgres,
    member_id: i64,
    status: IngestMemberStatus,
    error_message: Option<&str>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    update_ingest_member_status_with_client(&mut client, member_id, status, error_message)
}

pub fn update_ingest_member_status_with_client<C: GenericClient>(
    client: &mut C,
    member_id: i64,
    status: IngestMemberStatus,
    error_message: Option<&str>,
) -> Result<(), StorageError> {
    let updated = client
        .execute(
            "UPDATE ingest_member \
             SET status = $2, last_error_message = COALESCE($3, last_error_message), updated_at = now() \
             WHERE member_id = $1;",
            &[&member_id, &status.as_str(), &error_message],
        )
        .map_err(StorageError::PostgresClient)?;
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::IngestAccounting {
            message: format!("ingest member `{member_id}` does not exist"),
        })
    }
}
