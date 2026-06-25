//! JURI (jurisprudence) archive ingestion: counters, manifest, the archive-run payload, per-member decision processing, and accounting.

use crate::*;

pub(crate) const JURI_PARSER_VERSION: &str = "juri_decision_parser:v1";

pub(crate) const JURI_CANONICAL_SCHEMA_VERSION: &str = "juri_decision:v1";

#[derive(Default)]
pub(crate) struct JuriArchiveIngestCounters {
    pub(crate) visited_members: usize,
    pub(crate) inserted_documents: usize,
    pub(crate) inserted_chunks: usize,
    pub(crate) inserted_publisher_edges: usize,
    pub(crate) inserted_inferred_edges: usize,
    pub(crate) skipped_members: usize,
    pub(crate) skipped_compatible_members: usize,
    pub(crate) skipped_empty_body_members: usize,
    pub(crate) failed_members: usize,
    pub(crate) quarantined_payloads: usize,
    pub(crate) unsupported_roots: BTreeMap<String, usize>,
}

impl JuriArchiveIngestCounters {
    pub(crate) fn merge_committed(&mut self, committed: Self) {
        self.inserted_documents += committed.inserted_documents;
        self.inserted_chunks += committed.inserted_chunks;
        self.inserted_publisher_edges += committed.inserted_publisher_edges;
        self.inserted_inferred_edges += committed.inserted_inferred_edges;
        self.skipped_members += committed.skipped_members;
        self.skipped_compatible_members += committed.skipped_compatible_members;
        self.skipped_empty_body_members += committed.skipped_empty_body_members;
        self.failed_members += committed.failed_members;
        self.quarantined_payloads += committed.quarantined_payloads;
        for (root, count) in committed.unsupported_roots {
            *self.unsupported_roots.entry(root).or_default() += count;
        }
    }
}

pub(crate) fn juri_archive_manifest(
    source: ArchiveSource,
    plan: &ArchivePlan,
    latest_processed: Option<&PlannedArchive>,
    counters: &JuriArchiveIngestCounters,
    run_status: &str,
) -> Value {
    // Freshness/source_version reflect the latest archive ACTUALLY processed by this run, not the
    // newest archive in the directory — so an incremental/`--since`-filtered or no-op sync never
    // advances reported corpus freshness for archives it did not read.
    let freshness = latest_processed.map_or(Value::Null, |archive| {
        json!({
            "latest_archive": archive.file_name.as_str(),
            "latest_archive_kind": archive.kind,
            "latest_archive_timestamp": archive.timestamp.to_string(),
            "latest_archive_timestamp_compact": archive.timestamp.compact()
        })
    });
    json!({
        "source": source.as_str(),
        "dataset": source.as_str().to_uppercase(),
        // Honest provenance: bulk jurisprudence carries NO official Judilibre zone offsets, so all
        // decision chunking is heuristic and never satisfies the official-zone gate by assertion.
        "chunking_provenance": "heuristic",
        "zone_accurate": false,
        "run_status": run_status,
        "complete": run_status == IngestRunStatus::Completed.as_str(),
        "parser_version": JURI_PARSER_VERSION,
        "canonical_schema_version": JURI_CANONICAL_SCHEMA_VERSION,
        "code_version": CLI_CODE_VERSION,
        "source_version": latest_processed.map(|archive| archive.timestamp.to_string()),
        "freshness": freshness,
        "archive_plan": {
            "baseline": planned_archive_manifest(&plan.baseline),
            "deltas": plan.deltas.iter().map(planned_archive_manifest).collect::<Vec<_>>(),
            "skipped_count": plan.skipped.len(),
            "skipped": &plan.skipped
        },
        "coverage": {
            "visited_members": counters.visited_members,
            "inserted_documents": counters.inserted_documents,
            "inserted_chunks": counters.inserted_chunks,
            "inserted_publisher_edges": counters.inserted_publisher_edges,
            "inserted_inferred_edges": counters.inserted_inferred_edges,
            "skipped_members": counters.skipped_members,
            "skipped_compatible_members": counters.skipped_compatible_members,
            "skipped_empty_body_members": counters.skipped_empty_body_members,
            "failed_members": counters.failed_members,
            "quarantined_payloads": counters.quarantined_payloads,
            "unsupported_roots": &counters.unsupported_roots
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn ingest_juri_archives_payload(
    index_dir: Option<&Path>,
    source: ArchiveSource,
    archives_dir: &Path,
    run_id: Option<String>,
    limit_members: Option<u32>,
    max_member_bytes: u64,
    quarantine_dir: Option<&Path>,
    safe_mode: bool,
    archive_filter: ArchiveSyncFilter<'_>,
) -> Result<Value, ErrorObject> {
    if !source.is_jurisprudence() {
        return Err(ErrorObject::bad_input(format!(
            "ingest juri-archives source `{}` is not a jurisprudence dataset",
            source.as_str()
        )));
    }
    let index_dir = require_configured_index_dir(index_dir)?;
    let postgres = open_index_for_bulk_ingest(index_dir.as_path())?;
    let mut ingest_client =
        postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
            .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    ingest_client
        .batch_execute("SET synchronous_commit TO off;")
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let plan = plan_from_dir(source, archives_dir).map_err(|error| {
        ErrorObject::bad_input(format!(
            "failed to plan {} archives: {error}",
            source.as_str()
        ))
    })?;
    let run_id = run_id.unwrap_or_else(|| default_juri_run_id(source));
    let archive_plan_json =
        serde_json::to_string(&plan).map_err(|error| dependency_unavailable(error.to_string()))?;
    let archives = select_archives_to_process(&plan, archive_filter);
    let latest_processed = archives.last().copied();
    let initial_manifest = juri_archive_manifest(
        source,
        &plan,
        latest_processed,
        &JuriArchiveIngestCounters::default(),
        IngestRunStatus::Running.as_str(),
    );
    let initial_manifest_json = initial_manifest.to_string();

    start_ingest_run_with_client(
        &mut ingest_client,
        &IngestRunInput {
            run_id: run_id.as_str(),
            source: source.as_str(),
            parser_version: JURI_PARSER_VERSION,
            schema_version: JURI_CANONICAL_SCHEMA_VERSION,
            code_version: CLI_CODE_VERSION,
            safe_mode,
            archive_plan_json: Some(archive_plan_json.as_str()),
            manifest_json: Some(initial_manifest_json.as_str()),
        },
    )
    .map_err(storage_error_object)?;

    let mut counters = JuriArchiveIngestCounters::default();
    let mut fatal_error = None::<ErrorObject>;
    let limit_members = limit_members.map(|limit| limit as usize);

    'archives: for archive in &archives {
        let archive_name = archive.file_name.as_str();
        // Shared per-archive batching loop (see `read_archive_members_batched`); only the flush call
        // and read-error label are JURI-specific. The visit count is threaded by value so the flush
        // closure can hold the JURI-specific `&mut counters`.
        let visited_before = counters.visited_members;
        let read = read_archive_members_batched(
            &archive.path,
            max_member_bytes,
            limit_members,
            visited_before,
            |pending_members, pending_member_bytes| {
                flush_juri_archive_member_batch(
                    &mut ingest_client,
                    source,
                    run_id.as_str(),
                    archive_name,
                    pending_members,
                    pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            },
        );
        match read {
            Ok(report) => {
                counters.visited_members = report.visited_members;
                if report.stopped_by_limit {
                    break 'archives;
                }
            }
            Err(ArchiveBatchReadError::Flush {
                visited_members,
                error,
            }) => {
                counters.visited_members = visited_members;
                fatal_error = Some(storage_error_object(error));
                break 'archives;
            }
            Err(ArchiveBatchReadError::Read {
                visited_members,
                error,
            }) => {
                counters.visited_members = visited_members;
                fatal_error = Some(ErrorObject::bad_input(format!(
                    "failed to read {} archive `{}`: {error}",
                    source.as_str(),
                    archive.path.display()
                )));
                break 'archives;
            }
        }
    }

    // Build the manifest from the pre-finalization state, then RECOMPUTE the terminal run_status
    // after the manifest update so a fatal manifest-update failure cannot persist `completed`
    // (mirrors the LEGI reference; review 2026-06-23 phase2-1bc WARN).
    let manifest_run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let final_manifest =
        juri_archive_manifest(source, &plan, latest_processed, &counters, manifest_run_status.as_str());
    let final_manifest_json = final_manifest.to_string();
    if let Err(error) = update_ingest_run_manifest_with_client(
        &mut ingest_client,
        run_id.as_str(),
        &final_manifest_json,
    ) {
        fatal_error.get_or_insert_with(|| storage_error_object(error));
    }

    let run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let error_message = fatal_error.as_ref().map(|error| error.message.as_str());
    finish_ingest_run_with_client(&mut ingest_client, run_id.as_str(), run_status, error_message)
        .map_err(storage_error_object)?;
    if let Some(error) = fatal_error {
        return Err(error);
    }
    let replay_snapshot_cache = if run_status == IngestRunStatus::Completed {
        Some(maybe_refresh_replay_snapshot(&postgres)?)
    } else {
        None
    };

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest juri-archives",
        "source": source.as_str(),
        "run_id": run_id,
        "run_status": run_status.as_str(),
        "safe_mode": safe_mode,
        "zone_accurate": false,
        "chunking_provenance": "heuristic",
        "index_dir": index_dir,
        "archives_dir": archives_dir,
        "archives": {
            "baseline": plan.baseline.file_name,
            "deltas": plan.deltas.iter().map(|archive| archive.file_name.as_str()).collect::<Vec<_>>(),
            "skipped": plan.skipped
        },
        "manifest": final_manifest,
        "limit_members": limit_members,
        "max_member_bytes": max_member_bytes,
        "visited_members": counters.visited_members,
        "inserted_documents": counters.inserted_documents,
        "inserted_chunks": counters.inserted_chunks,
        "inserted_publisher_edges": counters.inserted_publisher_edges,
        "inserted_inferred_edges": counters.inserted_inferred_edges,
        "skipped_members": counters.skipped_members,
        "skipped_compatible_members": counters.skipped_compatible_members,
        "skipped_empty_body_members": counters.skipped_empty_body_members,
        "failed_members": counters.failed_members,
        "quarantined_payloads": counters.quarantined_payloads,
        "unsupported_roots": counters.unsupported_roots,
        "quarantine_dir": quarantine_dir,
        "replay_snapshot_cache": replay_snapshot_cache
            .as_ref()
            .map(|snapshot| replay_snapshot_cache_value(snapshot.as_ref()))
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn flush_juri_archive_member_batch(
    client: &mut postgres::Client,
    source: ArchiveSource,
    run_id: &str,
    archive_name: &str,
    pending_members: &mut Vec<ArchiveMember>,
    pending_member_bytes: &mut usize,
    quarantine_dir: Option<&Path>,
    counters: &mut JuriArchiveIngestCounters,
) -> Result<(), StorageError> {
    if pending_members.is_empty() {
        return Ok(());
    }
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute("SET LOCAL synchronous_commit TO off;")
        .map_err(StorageError::PostgresClient)?;
    let projection_statements = prepare_document_projection_statements(&mut transaction)?;
    let mut committed = JuriArchiveIngestCounters::default();
    for member in pending_members.iter() {
        process_juri_archive_member(
            &mut transaction,
            source,
            run_id,
            archive_name,
            member,
            &projection_statements,
            quarantine_dir,
            &mut committed,
        )?;
    }
    transaction.commit().map_err(StorageError::PostgresClient)?;
    counters.merge_committed(committed);
    pending_members.clear();
    *pending_member_bytes = 0;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_juri_archive_member<C: postgres::GenericClient>(
    client: &mut C,
    source: ArchiveSource,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    projection_statements: &DocumentProjectionStatements,
    quarantine_dir: Option<&Path>,
    counters: &mut JuriArchiveIngestCounters,
) -> Result<(), StorageError> {
    let source_payload_hash = source_payload_hash(&member.bytes);
    let compatibility = IngestCompatibility {
        parser_version: JURI_PARSER_VERSION,
        schema_version: JURI_CANONICAL_SCHEMA_VERSION,
        code_version: CLI_CODE_VERSION,
        source_payload_hash: source_payload_hash.as_str(),
    };
    let resume = ingest_resume_decision_with_client(
        client,
        archive_name,
        member.member_path.as_str(),
        compatibility,
    )?;
    match resume.action {
        IngestResumeAction::Skip => {
            if resume.previous_run_id.as_deref() != Some(run_id) {
                record_juri_member(
                    client,
                    source,
                    run_id,
                    JuriMemberRecordInput {
                        archive_name,
                        member_path: member.member_path.as_str(),
                        source_entity: None,
                        date_anchor: None,
                        status: IngestMemberStatus::Skipped,
                        compatibility,
                    },
                )?;
            }
            counters.skipped_members += 1;
            counters.skipped_compatible_members += 1;
            return Ok(());
        }
        IngestResumeAction::BlockedIncompatible => {
            let message = format!(
                "resume blocked by compatibility mismatch on fields [{}]",
                resume.mismatched_fields.join(", ")
            );
            let record = record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_juri_member_error(
                client,
                run_id,
                Some(record.member_id),
                "validation_error",
                "compatibility_mismatch",
                message.as_str(),
                archive_name,
                member,
                quarantine_dir,
                counters,
            )?;
            counters.failed_members += 1;
            return Ok(());
        }
        IngestResumeAction::Process | IngestResumeAction::Retry => {}
    }

    match parse_juri_member(source, member) {
        Ok(ParsedJuriXml::Decision(decision)) => {
            let decision = *decision;
            let record = record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(decision.source_uid.as_str()),
                    date_anchor: Some(decision.decision_date.as_str()),
                    status: IngestMemberStatus::Parsed,
                    compatibility,
                },
            )?;
            let report =
                insert_decision_documents_with_statements(client, projection_statements, &[decision], None)?;
            update_ingest_member_status_with_client(
                client,
                record.member_id,
                IngestMemberStatus::Inserted,
                None,
            )?;
            counters.inserted_documents += report.documents;
            counters.inserted_chunks += report.chunks;
            counters.inserted_publisher_edges += report.publisher_edges;
            counters.inserted_inferred_edges += report.inferred_edges;
        }
        Ok(ParsedJuriXml::UnsupportedRoot { root }) => {
            *counters.unsupported_roots.entry(root.clone()).or_default() += 1;
            record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(root.as_str()),
                    date_anchor: None,
                    status: IngestMemberStatus::Skipped,
                    compatibility,
                },
            )?;
            counters.skipped_members += 1;
        }
        // A decision with no textual body is not corrupt — there is just nothing to index. Record it
        // as a SKIP (not a failure/quarantine) so the run completes cleanly, matching the LEGI
        // no-text-article handling.
        Err(JuriParseError::EmptyBody { source_uid }) => {
            record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(source_uid.as_str()),
                    date_anchor: None,
                    status: IngestMemberStatus::Skipped,
                    compatibility,
                },
            )?;
            counters.skipped_members += 1;
            counters.skipped_empty_body_members += 1;
        }
        Err(error) => {
            let (error_class, error_code) = juri_parse_error_class(&error);
            let message = error.to_string();
            let record = record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_juri_member_error(
                client,
                run_id,
                Some(record.member_id),
                error_class,
                error_code,
                message.as_str(),
                archive_name,
                member,
                quarantine_dir,
                counters,
            )?;
            counters.failed_members += 1;
        }
    }
    Ok(())
}

pub(crate) struct JuriMemberRecordInput<'a> {
    pub(crate) archive_name: &'a str,
    pub(crate) member_path: &'a str,
    pub(crate) source_entity: Option<&'a str>,
    pub(crate) date_anchor: Option<&'a str>,
    pub(crate) status: IngestMemberStatus,
    pub(crate) compatibility: IngestCompatibility<'a>,
}

pub(crate) fn record_juri_member<C: postgres::GenericClient>(
    client: &mut C,
    source: ArchiveSource,
    run_id: &str,
    input: JuriMemberRecordInput<'_>,
) -> Result<jurisearch_storage::ingest_accounting::IngestMemberRecord, StorageError> {
    record_ingest_member_with_client(
        client,
        &IngestMemberInput {
            run_id,
            archive_name: input.archive_name,
            member_path: input.member_path,
            source: source.as_str(),
            source_entity: input.source_entity,
            date_anchor: input.date_anchor,
            status: input.status,
            compatibility: input.compatibility,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_juri_member_error<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    member_id: Option<i64>,
    error_class: &str,
    error_code: &str,
    message: &str,
    archive_name: &str,
    member: &ArchiveMember,
    quarantine_dir: Option<&Path>,
    counters: &mut JuriArchiveIngestCounters,
) -> Result<(), StorageError> {
    let quarantined = maybe_quarantine_payload(
        quarantine_dir,
        run_id,
        archive_name,
        member.member_path.as_str(),
        &member.bytes,
    )?;
    if quarantined {
        counters.quarantined_payloads += 1;
    }
    let context = json!({
        "archive_name": archive_name,
        "member_path": member.member_path,
        "quarantined": quarantined
    })
    .to_string();
    record_ingest_error_with_client(
        client,
        &IngestErrorInput {
            run_id,
            member_id,
            error_class,
            error_code,
            message,
            retry_policy: "none",
            context_json: Some(context.as_str()),
        },
    )?;
    Ok(())
}

pub(crate) fn juri_parse_error_class(error: &JuriParseError) -> (&'static str, &'static str) {
    match error {
        JuriParseError::Xml { .. } => ("parse_error", "parse_malformed_xml"),
        JuriParseError::NotUtf8 { .. } => ("parse_error", "parse_not_utf8"),
        JuriParseError::MissingRequiredField { .. } => {
            ("validation_error", "validation_missing_required_field")
        }
        JuriParseError::InvalidDate { .. } => ("validation_error", "validation_invalid_date"),
        JuriParseError::InvalidId { .. } => ("validation_error", "validation_invalid_id"),
        // EmptyBody is handled as a skip before this classifier; map it for completeness.
        JuriParseError::EmptyBody { .. } => ("validation_error", "validation_empty_body"),
        JuriParseError::UnknownSource { .. } | JuriParseError::SourceFamilyMismatch { .. } => {
            ("validation_error", "validation_source_mismatch")
        }
    }
}
