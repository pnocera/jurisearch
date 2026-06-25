//! Compatibility-based resume decision for a run.

use super::*;

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

pub fn ingest_resume_decision(
    postgres: &ManagedPostgres,
    archive_name: &str,
    member_path: &str,
    compatibility: IngestCompatibility<'_>,
) -> Result<IngestResumeDecision, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    ingest_resume_decision_with_client(&mut client, archive_name, member_path, compatibility)
}

pub fn ingest_resume_decision_with_client<C: GenericClient>(
    client: &mut C,
    archive_name: &str,
    member_path: &str,
    compatibility: IngestCompatibility<'_>,
) -> Result<IngestResumeDecision, StorageError> {
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
