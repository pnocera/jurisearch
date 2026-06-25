//! Ingest run lifecycle (start/finish/manifest) + compatibility checks.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestRunStatus {
    Running,
    Completed,
    Failed,
    Aborted,
}

impl IngestRunStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestCompatibility<'a> {
    pub parser_version: &'a str,
    pub schema_version: &'a str,
    pub code_version: &'a str,
    pub source_payload_hash: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestRunInput<'a> {
    pub run_id: &'a str,
    pub source: &'a str,
    pub parser_version: &'a str,
    pub schema_version: &'a str,
    pub code_version: &'a str,
    pub safe_mode: bool,
    pub archive_plan_json: Option<&'a str>,
    pub manifest_json: Option<&'a str>,
}

pub fn start_ingest_run(
    postgres: &ManagedPostgres,
    input: &IngestRunInput<'_>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    start_ingest_run_with_client(&mut client, input)
}

pub fn start_ingest_run_with_client<C: GenericClient>(
    client: &mut C,
    input: &IngestRunInput<'_>,
) -> Result<(), StorageError> {
    client
        .execute(
            "INSERT INTO ingest_run \
                (run_id, source, status, parser_version, schema_version, code_version, \
                 safe_mode, archive_plan, manifest, error_message, completed_at) \
             VALUES \
                ($1, $2, $3, $4, $5, $6, $7, \
                 COALESCE($8::text::jsonb, '{}'::jsonb), \
                 COALESCE($9::text::jsonb, '{}'::jsonb), NULL, NULL) \
             ON CONFLICT (run_id) DO UPDATE SET \
                source = EXCLUDED.source, \
                status = EXCLUDED.status, \
                parser_version = EXCLUDED.parser_version, \
                schema_version = EXCLUDED.schema_version, \
                code_version = EXCLUDED.code_version, \
                safe_mode = EXCLUDED.safe_mode, \
                archive_plan = EXCLUDED.archive_plan, \
                manifest = EXCLUDED.manifest, \
                error_message = NULL, \
                completed_at = NULL, \
                updated_at = now();",
            &[
                &input.run_id,
                &input.source,
                &IngestRunStatus::Running.as_str(),
                &input.parser_version,
                &input.schema_version,
                &input.code_version,
                &input.safe_mode,
                &input.archive_plan_json,
                &input.manifest_json,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    // An ingest run can add documents/chunks, changing coverage; drop the readiness cache so the
    // next query-readiness check recomputes live.
    invalidate_query_readiness(client)?;
    Ok(())
}

pub fn finish_ingest_run(
    postgres: &ManagedPostgres,
    run_id: &str,
    status: IngestRunStatus,
    error_message: Option<&str>,
) -> Result<(), StorageError> {
    if status == IngestRunStatus::Running {
        return Err(StorageError::IngestAccounting {
            message: "finish_ingest_run requires a terminal status".to_owned(),
        });
    }
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    finish_ingest_run_with_client(&mut client, run_id, status, error_message)
}

pub fn finish_ingest_run_with_client<C: GenericClient>(
    client: &mut C,
    run_id: &str,
    status: IngestRunStatus,
    error_message: Option<&str>,
) -> Result<(), StorageError> {
    if status == IngestRunStatus::Running {
        return Err(StorageError::IngestAccounting {
            message: "finish_ingest_run requires a terminal status".to_owned(),
        });
    }
    let updated = client
        .execute(
            "UPDATE ingest_run \
             SET status = $2, error_message = $3, completed_at = now(), updated_at = now() \
             WHERE run_id = $1;",
            &[&run_id, &status.as_str(), &error_message],
        )
        .map_err(StorageError::PostgresClient)?;
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::IngestAccounting {
            message: format!("ingest run `{run_id}` does not exist"),
        })
    }
}

pub fn update_ingest_run_manifest(
    postgres: &ManagedPostgres,
    run_id: &str,
    manifest_json: &str,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    update_ingest_run_manifest_with_client(&mut client, run_id, manifest_json)
}

pub fn update_ingest_run_manifest_with_client<C: GenericClient>(
    client: &mut C,
    run_id: &str,
    manifest_json: &str,
) -> Result<(), StorageError> {
    let updated = client
        .execute(
            "UPDATE ingest_run \
             SET manifest = $2::text::jsonb, updated_at = now() \
             WHERE run_id = $1;",
            &[&run_id, &manifest_json],
        )
        .map_err(StorageError::PostgresClient)?;
    if updated == 1 {
        Ok(())
    } else {
        Err(StorageError::IngestAccounting {
            message: format!("ingest run `{run_id}` does not exist"),
        })
    }
}

pub(crate) fn compatibility_mismatches(
    actual: (&str, &str, &str, &str),
    expected: IngestCompatibility<'_>,
) -> Vec<String> {
    let mut mismatches = Vec::new();
    if actual.0 != expected.parser_version {
        mismatches.push("parser_version".to_owned());
    }
    if actual.1 != expected.schema_version {
        mismatches.push("schema_version".to_owned());
    }
    if actual.2 != expected.code_version {
        mismatches.push("code_version".to_owned());
    }
    if actual.3 != expected.source_payload_hash {
        mismatches.push("source_payload_hash".to_owned());
    }
    mismatches
}
