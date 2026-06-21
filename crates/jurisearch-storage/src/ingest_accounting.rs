use postgres::GenericClient;
use serde::Serialize;

use crate::runtime::{ManagedPostgres, StorageError};

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

    fn from_db(value: &str) -> Result<Self, StorageError> {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestResumeAction {
    Process,
    Skip,
    Retry,
    BlockedIncompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestResumeDecision {
    pub action: IngestResumeAction,
    pub reason: String,
    pub previous_run_id: Option<String>,
    pub previous_status: Option<IngestMemberStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mismatched_fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestHealthReport {
    pub latest_run_id: Option<String>,
    pub latest_run_status: Option<String>,
    pub latest_completed_run_id: Option<String>,
    pub total_members: i64,
    pub inserted_members: i64,
    pub skipped_members: i64,
    pub failed_members: i64,
    pub failed_member_percentage: Option<f64>,
    pub error_classes: Vec<IngestErrorClassCount>,
    pub projection_coverage: CoverageMetric,
    pub embedding_coverage: CoverageMetric,
    pub replay_snapshot_status: String,
    pub replay_snapshot: ReplaySnapshotReport,
    pub recovery_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestReadinessReport {
    pub projection_coverage: CoverageMetric,
    pub embedding_coverage: CoverageMetric,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplaySnapshotReport {
    pub documents: ReplaySnapshotComponent,
    pub chunks: ReplaySnapshotComponent,
    pub publisher_edges: ReplaySnapshotComponent,
    pub embeddings: ReplaySnapshotComponent,
    pub manifests: ReplaySnapshotComponent,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplaySnapshotComponent {
    pub count: i64,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IngestErrorClassCount {
    pub error_class: String,
    pub error_code: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CoverageMetric {
    pub covered: i64,
    pub total: i64,
    pub percentage: Option<f64>,
}

pub fn start_ingest_run(
    postgres: &ManagedPostgres,
    input: &IngestRunInput<'_>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
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

pub fn record_ingest_member(
    postgres: &ManagedPostgres,
    input: &IngestMemberInput<'_>,
) -> Result<IngestMemberRecord, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
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
    Ok(IngestMemberRecord {
        member_id: row.get(0),
        attempt_count: row.get(1),
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

pub fn record_ingest_error(
    postgres: &ManagedPostgres,
    input: &IngestErrorInput<'_>,
) -> Result<i64, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let row = transaction
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
        transaction
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
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(row.get(0))
}

pub fn ingest_resume_decision(
    postgres: &ManagedPostgres,
    archive_name: &str,
    member_path: &str,
    compatibility: IngestCompatibility<'_>,
) -> Result<IngestResumeDecision, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let Some(row) = client
        .query_opt(
            "SELECT run_id, status, parser_version, schema_version, code_version, source_payload_hash \
             FROM ingest_member \
             WHERE archive_name = $1 AND member_path = $2 \
             ORDER BY updated_at DESC, member_id DESC \
             LIMIT 1;",
            &[&archive_name, &member_path],
        )
        .map_err(StorageError::PostgresClient)?
    else {
        return Ok(IngestResumeDecision {
            action: IngestResumeAction::Process,
            reason: "new_member".to_owned(),
            previous_run_id: None,
            previous_status: None,
            mismatched_fields: Vec::new(),
        });
    };

    let previous_run_id: String = row.get(0);
    let status = IngestMemberStatus::from_db(&row.get::<_, String>(1))?;
    let parser_version: String = row.get(2);
    let schema_version: String = row.get(3);
    let code_version: String = row.get(4);
    let source_payload_hash: String = row.get(5);
    let mismatched_fields = compatibility_mismatches(
        (
            &parser_version,
            &schema_version,
            &code_version,
            &source_payload_hash,
        ),
        compatibility,
    );
    if !mismatched_fields.is_empty() {
        return Ok(IngestResumeDecision {
            action: IngestResumeAction::BlockedIncompatible,
            reason: "compatibility_mismatch".to_owned(),
            previous_run_id: Some(previous_run_id),
            previous_status: Some(status),
            mismatched_fields,
        });
    }

    let (action, reason) = match status {
        IngestMemberStatus::Inserted | IngestMemberStatus::Skipped => {
            (IngestResumeAction::Skip, "compatible_complete")
        }
        IngestMemberStatus::Failed => (IngestResumeAction::Retry, "previous_failed"),
        IngestMemberStatus::Discovered | IngestMemberStatus::Parsed => {
            (IngestResumeAction::Retry, "previous_unfinished")
        }
    };

    Ok(IngestResumeDecision {
        action,
        reason: reason.to_owned(),
        previous_run_id: Some(previous_run_id),
        previous_status: Some(status),
        mismatched_fields: Vec::new(),
    })
}

pub fn load_ingest_health(postgres: &ManagedPostgres) -> Result<IngestHealthReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let latest = client
        .query_opt(
            "SELECT run_id, status \
             FROM ingest_run \
             ORDER BY started_at DESC, run_id DESC \
             LIMIT 1;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let latest_run_id = latest.as_ref().map(|row| row.get::<_, String>(0));
    let latest_run_status = latest.as_ref().map(|row| row.get::<_, String>(1));
    let latest_completed_run_id = client
        .query_opt(
            "SELECT run_id \
             FROM ingest_run \
             WHERE status = 'completed' \
             ORDER BY completed_at DESC NULLS LAST, started_at DESC, run_id DESC \
             LIMIT 1;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .map(|row| row.get::<_, String>(0));

    let counts = client
        .query_one(
            "SELECT count(*)::bigint, \
                    count(*) FILTER (WHERE status = 'inserted')::bigint, \
                    count(*) FILTER (WHERE status = 'skipped')::bigint, \
                    count(*) FILTER (WHERE status = 'failed')::bigint \
             FROM ingest_member \
             WHERE ($1::text IS NULL OR run_id = $1);",
            &[&latest_run_id],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_members: i64 = counts.get(0);
    let inserted_members: i64 = counts.get(1);
    let skipped_members: i64 = counts.get(2);
    let failed_members: i64 = counts.get(3);

    let error_classes = client
        .query(
            "SELECT error_class, error_code, count(*)::bigint \
             FROM ingest_error \
             WHERE ($1::text IS NULL OR run_id = $1) \
             GROUP BY error_class, error_code \
             ORDER BY count(*) DESC, error_class, error_code;",
            &[&latest_run_id],
        )
        .map_err(StorageError::PostgresClient)?
        .into_iter()
        .map(|row| IngestErrorClassCount {
            error_class: row.get(0),
            error_code: row.get(1),
            count: row.get(2),
        })
        .collect();

    let readiness = load_readiness_metrics(&mut client)?;
    let replay_snapshot = load_replay_snapshot(&mut client)?;
    let replay_snapshot_status = if replay_snapshot.documents.count == 0
        && replay_snapshot.chunks.count == 0
        && replay_snapshot.publisher_edges.count == 0
        && replay_snapshot.embeddings.count == 0
    {
        "empty"
    } else {
        "available"
    };

    let mut recovery_warnings = Vec::new();
    if let Some(status) = &latest_run_status
        && status != IngestRunStatus::Completed.as_str()
    {
        recovery_warnings.push(format!("latest ingest run is `{status}`"));
    }
    if failed_members > 0 {
        recovery_warnings.push(format!(
            "{failed_members} member(s) failed in latest ingest run"
        ));
    }

    Ok(IngestHealthReport {
        latest_run_id,
        latest_run_status,
        latest_completed_run_id,
        total_members,
        inserted_members,
        skipped_members,
        failed_members,
        failed_member_percentage: percentage(failed_members, total_members),
        error_classes,
        projection_coverage: readiness.projection_coverage,
        embedding_coverage: readiness.embedding_coverage,
        replay_snapshot_status: replay_snapshot_status.to_owned(),
        replay_snapshot,
        recovery_warnings,
    })
}

pub fn load_ingest_readiness(
    postgres: &ManagedPostgres,
) -> Result<IngestReadinessReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_readiness_metrics(&mut client)
}

pub fn load_ingest_projection_coverage(
    postgres: &ManagedPostgres,
) -> Result<CoverageMetric, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_projection_coverage(&mut client)
}

pub fn load_ingest_embedding_coverage(
    postgres: &ManagedPostgres,
) -> Result<CoverageMetric, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_embedding_coverage(&mut client)
}

fn load_readiness_metrics(
    client: &mut postgres::Client,
) -> Result<IngestReadinessReport, StorageError> {
    Ok(IngestReadinessReport {
        projection_coverage: load_projection_coverage(client)?,
        embedding_coverage: load_embedding_coverage(client)?,
    })
}

fn load_projection_coverage<C: GenericClient>(
    client: &mut C,
) -> Result<CoverageMetric, StorageError> {
    let projection = client
        .query_one(
            "SELECT count(DISTINCT d.document_id)::bigint, \
                    count(DISTINCT d.document_id) FILTER (WHERE c.chunk_id IS NOT NULL)::bigint \
             FROM documents d \
             LEFT JOIN chunks c ON c.document_id = d.document_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_documents: i64 = projection.get(0);
    let projected_documents: i64 = projection.get(1);

    Ok(CoverageMetric {
        covered: projected_documents,
        total: total_documents,
        percentage: percentage(projected_documents, total_documents),
    })
}

fn load_embedding_coverage<C: GenericClient>(
    client: &mut C,
) -> Result<CoverageMetric, StorageError> {
    let embedding = client
        .query_one(
            "SELECT count(*)::bigint, \
                    count(*) FILTER (WHERE ce.chunk_id IS NOT NULL)::bigint \
             FROM chunks c \
             LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_chunks: i64 = embedding.get(0);
    let embedded_chunks: i64 = embedding.get(1);

    Ok(CoverageMetric {
        covered: embedded_chunks,
        total: total_chunks,
        percentage: percentage(embedded_chunks, total_chunks),
    })
}

fn load_replay_snapshot(
    client: &mut postgres::Client,
) -> Result<ReplaySnapshotReport, StorageError> {
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;")
        .map_err(StorageError::PostgresClient)?;
    let documents = snapshot_component(
        &mut transaction,
        "documents",
        "SELECT document_id AS row_key, \
                md5(concat_ws(chr(31), document_id, source, kind, source_uid, \
                    coalesce(version_group, ''), coalesce(citation, ''), \
                    coalesce(title, ''), body, coalesce(valid_from::text, ''), \
                    coalesce(valid_to::text, ''), coalesce(valid_to_raw, ''), \
                    coalesce(source_url, ''), source_payload_hash, canonical_json::text)) AS row_hash \
         FROM documents",
    )?;
    let chunks = snapshot_component(
        &mut transaction,
        "chunks",
        "SELECT chunk_id AS row_key, \
                md5(concat_ws(chr(31), chunk_id, document_id, chunk_index::text, body, \
                    chunk_kind, source_fields::text, source_payload_hash, \
                    chunk_builder_version, coalesce(embedding_fingerprint, ''))) AS row_hash \
         FROM chunks",
    )?;
    let publisher_edges = snapshot_component(
        &mut transaction,
        "publisher_edges",
        "SELECT edge_id AS row_key, \
                md5(concat_ws(chr(31), edge_id, coalesce(from_document_id, ''), \
                    coalesce(to_document_id, ''), edge_kind, edge_source, payload::text)) AS row_hash \
         FROM graph_edges \
         WHERE edge_source = 'publisher'",
    )?;
    let embeddings = snapshot_component(
        &mut transaction,
        "chunk_embeddings",
        "SELECT chunk_id AS row_key, \
                md5(concat_ws(chr(31), chunk_id, embedding_fingerprint, embedding::text, \
                    model, dimension::text)) AS row_hash \
         FROM chunk_embeddings",
    )?;
    let manifests = snapshot_component(
        &mut transaction,
        "index_manifest",
        "SELECT key AS row_key, \
                md5(concat_ws(chr(31), key, value::text)) AS row_hash \
         FROM index_manifest",
    )?;
    let signature_input = format!(
        "documents:{}:{}|chunks:{}:{}|publisher_edges:{}:{}|embeddings:{}:{}|manifests:{}:{}",
        documents.count,
        documents.signature,
        chunks.count,
        chunks.signature,
        publisher_edges.count,
        publisher_edges.signature,
        embeddings.count,
        embeddings.signature,
        manifests.count,
        manifests.signature
    );
    let signature = transaction
        .query_one("SELECT md5($1);", &[&signature_input])
        .map_err(StorageError::PostgresClient)?
        .get(0);
    transaction.commit().map_err(StorageError::PostgresClient)?;

    Ok(ReplaySnapshotReport {
        documents,
        chunks,
        publisher_edges,
        embeddings,
        manifests,
        signature,
    })
}

fn snapshot_component<C: GenericClient>(
    client: &mut C,
    component_name: &str,
    rows_sql: &str,
) -> Result<ReplaySnapshotComponent, StorageError> {
    let sql = format!(
        "SELECT count(*)::bigint, \
                md5(coalesce(string_agg(row_hash, E'\\n' ORDER BY row_key), '')) \
         FROM ({rows_sql}) {component_name}_snapshot_rows;"
    );
    let row = client
        .query_one(sql.as_str(), &[])
        .map_err(StorageError::PostgresClient)?;
    Ok(ReplaySnapshotComponent {
        count: row.get(0),
        signature: row.get(1),
    })
}

fn percentage(covered: i64, total: i64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some((covered as f64 / total as f64) * 100.0)
    }
}

fn compatibility_mismatches(
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
