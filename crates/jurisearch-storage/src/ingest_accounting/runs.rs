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

/// The completed-run archive cursor for `source`: the MAX of
/// `manifest.freshness.latest_archive_timestamp_compact` over `ingest_run` rows whose `status` is
/// `completed` and whose freshness field is non-null. `None` when the source has no completed run with a
/// recorded latest archive (a cold DB, or a hand-loaded corpus that was never ingested).
///
/// This is deliberately an `ingest_run.status = 'completed'` cursor, NOT a `max()` over terminal
/// `ingest_member` rows: a run that reached a later archive with `inserted`/`skipped` members but left
/// an EARLIER archive with a `failed` member is recorded `status = 'failed'` (never `completed`), so a
/// member-max could advance the cursor PAST that failed archive and a future delta-only run would skip
/// it. Anchoring to completed runs guarantees every archive up to the returned compact was fully
/// processed (producer runs with `limit_members = None`), so it is safe to use as a `since_compact`
/// lower bound that a delta-only ingest will not re-open the baseline for.
///
/// Member-limited runs are EXCLUDED. A `--limit-members` CLI run can stop early yet still finish
/// `status = 'completed'`, and its manifest `freshness.latest_archive_timestamp_compact` is the PLANNED
/// last archive — ahead of what was actually ingested. Such runs set `freshness.member_limited = true`;
/// the query rejects them (`COALESCE(...member_limited..., false) = false`) so a partial CLI ingest can
/// never poison the producer's delta-only cursor. `COALESCE` treats historical runs that predate the
/// flag as NOT limited: those rows are pre-existing `completed` runs that were, in practice, produced by
/// the producer (`limit_members = None`) or a full CLI build, so treating an ABSENT flag as unlimited
/// matches their real semantics; only a run that explicitly recorded `member_limited = true` is skipped.
///
/// The stored compact is written by our own manifest code from `ArchiveTimestamp::compact()`, so it is
/// always `YYYYMMDDHHMMSS`; the shape is re-validated here (14 ASCII digits) and a malformed value is
/// surfaced as an [`StorageError::IngestAccounting`] rather than trusted.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a query failure; [`StorageError::IngestAccounting`] if the
/// stored cursor is not a 14-digit compact timestamp.
pub fn latest_completed_ingest_archive_compact_with_client<C: GenericClient>(
    client: &mut C,
    source: &str,
) -> Result<Option<String>, StorageError> {
    let cursor: Option<String> = client
        .query_one(
            "SELECT max(manifest->'freshness'->>'latest_archive_timestamp_compact') \
             FROM ingest_run \
             WHERE source = $1 \
               AND status = 'completed' \
               AND manifest->'freshness'->>'latest_archive_timestamp_compact' IS NOT NULL \
               AND COALESCE((manifest->'freshness'->>'member_limited')::boolean, false) = false;",
            &[&source],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if let Some(compact) = cursor.as_deref()
        && (compact.len() != 14 || !compact.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "completed ingest cursor for source `{source}` is not a 14-digit compact \
                 timestamp: `{compact}`"
            ),
        });
    }
    Ok(cursor)
}

pub(super) fn compatibility_mismatches(
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
