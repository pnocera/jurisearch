//! LEGI archive ingestion: counters, manifest, the archive-run payload, per-member LEGI processing/metadata-root projection, accounting/quarantine classification, and hierarchy backfill.

use crate::*;

#[derive(Debug, Default)]
pub(crate) struct LegiArchiveIngestCounters {
    pub(crate) visited_members: usize,
    pub(crate) inserted_documents: usize,
    pub(crate) inserted_chunks: usize,
    pub(crate) inserted_publisher_edges: usize,
    pub(crate) parsed_metadata_members: usize,
    pub(crate) persisted_metadata_members: usize,
    pub(crate) hierarchy_backfilled_documents: usize,
    pub(crate) hierarchy_backfill_invalidated_embeddings: usize,
    pub(crate) skipped_members: usize,
    pub(crate) skipped_compatible_members: usize,
    pub(crate) skipped_no_text_articles: usize,
    pub(crate) failed_members: usize,
    pub(crate) quarantined_payloads: usize,
    pub(crate) parsed_metadata_roots: BTreeMap<String, usize>,
    pub(crate) unsupported_roots: BTreeMap<String, usize>,
    pub(crate) processed_article_document_ids: BTreeSet<String>,
    pub(crate) processed_section_source_uids: BTreeSet<String>,
    pub(crate) processed_text_source_uids: BTreeSet<String>,
}

impl LegiArchiveIngestCounters {
    pub(crate) fn merge_committed(&mut self, committed: Self) {
        self.inserted_documents += committed.inserted_documents;
        self.inserted_chunks += committed.inserted_chunks;
        self.inserted_publisher_edges += committed.inserted_publisher_edges;
        self.parsed_metadata_members += committed.parsed_metadata_members;
        self.persisted_metadata_members += committed.persisted_metadata_members;
        self.skipped_members += committed.skipped_members;
        self.skipped_compatible_members += committed.skipped_compatible_members;
        self.skipped_no_text_articles += committed.skipped_no_text_articles;
        self.failed_members += committed.failed_members;
        self.quarantined_payloads += committed.quarantined_payloads;
        for (root, count) in committed.parsed_metadata_roots {
            *self.parsed_metadata_roots.entry(root).or_default() += count;
        }
        for (root, count) in committed.unsupported_roots {
            *self.unsupported_roots.entry(root).or_default() += count;
        }
        self.processed_article_document_ids
            .extend(committed.processed_article_document_ids);
        self.processed_section_source_uids
            .extend(committed.processed_section_source_uids);
        self.processed_text_source_uids
            .extend(committed.processed_text_source_uids);
    }
}

pub(crate) fn legi_archive_manifest(
    plan: &ArchivePlan,
    latest_processed: Option<&PlannedArchive>,
    counters: &LegiArchiveIngestCounters,
    run_status: &str,
    member_limited: bool,
) -> Value {
    // Freshness/source_version reflect the latest archive ACTUALLY processed (so an incremental or
    // no-op sync never advances reported corpus freshness for archives it did not read).
    //
    // `member_limited` records that this run ran under `--limit-members`: `latest_archive_*` is the
    // PLANNED last archive, but an early member-limit stop means members of `latest_archive` (and any
    // earlier archives after the stop) were NEVER processed even though `run_status` can still be
    // `completed`. A delta-only producer cursor MUST NOT trust the freshness of a member-limited run
    // (see `latest_completed_ingest_archive_compact_with_client`); the producer always passes
    // `limit_members = None`, so its own runs are never flagged.
    let freshness = latest_processed.map_or(Value::Null, |archive| {
        json!({
            "latest_archive": archive.file_name.as_str(),
            "latest_archive_kind": archive.kind,
            "latest_archive_timestamp": archive.timestamp.to_string(),
            "latest_archive_timestamp_compact": archive.timestamp.compact(),
            "member_limited": member_limited
        })
    });
    json!({
        "source": "legi",
        "dataset": "LEGI",
        "run_status": run_status,
        "complete": run_status == IngestRunStatus::Completed.as_str(),
        "parser_version": LEGI_PARSER_VERSION,
        "canonical_schema_version": CANONICAL_SCHEMA_VERSION,
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
            "parsed_metadata_members": counters.parsed_metadata_members,
            "persisted_metadata_members": counters.persisted_metadata_members,
            "hierarchy_backfill_scoped_documents": counters.processed_article_document_ids.len(),
            "hierarchy_backfill_scoped_sections": counters.processed_section_source_uids.len(),
            "hierarchy_backfill_scoped_texts": counters.processed_text_source_uids.len(),
            "hierarchy_backfilled_documents": counters.hierarchy_backfilled_documents,
            "hierarchy_backfill_invalidated_embeddings": counters.hierarchy_backfill_invalidated_embeddings,
            "skipped_members": counters.skipped_members,
            "skipped_compatible_members": counters.skipped_compatible_members,
            "skipped_no_text_articles": counters.skipped_no_text_articles,
            "failed_members": counters.failed_members,
            "quarantined_payloads": counters.quarantined_payloads,
            "parsed_metadata_roots": &counters.parsed_metadata_roots,
            "unsupported_roots": &counters.unsupported_roots
        }
    })
}

pub(crate) fn ingest_legi_archives(
    db: &impl DbClientSource,
    req: IngestArchivesRequest<'_>,
) -> Result<IngestReport, ErrorObject> {
    let IngestArchivesRequest {
        source: _,
        archives_dir,
        run_id,
        limit_members,
        max_member_bytes,
        quarantine_dir,
        safe_mode,
        filter: archive_filter,
        refresh_replay_snapshot,
    } = req;
    // The producer DB hands out a fresh client; the CLI opened it with the BulkIngest profile so the
    // bulk tuning is already applied server-side. Match the original by also relaxing
    // synchronous_commit on this loading session.
    let mut ingest_client = db.client().map_err(storage_error_object)?;
    ingest_client
        .batch_execute("SET synchronous_commit TO off;")
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let plan = plan_from_dir(ArchiveSource::Legi, archives_dir).map_err(|error| {
        ErrorObject::bad_input(format!("failed to plan LEGI archives: {error}"))
    })?;
    let run_id = run_id.unwrap_or_else(default_legi_run_id);
    let archive_plan_json =
        serde_json::to_string(&plan).map_err(|error| dependency_unavailable(error.to_string()))?;
    let archives = select_archives_to_process(&plan, archive_filter);
    let latest_processed = archives.last().copied();
    // A `--limit-members` run can stop early yet still finish `completed`, leaving `latest_processed`
    // (the PLANNED last archive) ahead of what was actually ingested. Record that so the producer's
    // completed-run cursor excludes it. `stopped_by_limit` implies `limit_members.is_some()`, so the
    // presence of a limit is the exact, conservative flag.
    let member_limited = limit_members.is_some();
    let initial_manifest = legi_archive_manifest(
        &plan,
        latest_processed,
        &LegiArchiveIngestCounters::default(),
        IngestRunStatus::Running.as_str(),
        member_limited,
    );
    let initial_manifest_json = initial_manifest.to_string();

    start_ingest_run_with_client(
        &mut ingest_client,
        &IngestRunInput {
            run_id: run_id.as_str(),
            source: "legi",
            parser_version: LEGI_PARSER_VERSION,
            schema_version: CANONICAL_SCHEMA_VERSION,
            code_version: CLI_CODE_VERSION,
            safe_mode,
            archive_plan_json: Some(archive_plan_json.as_str()),
            manifest_json: Some(initial_manifest_json.as_str()),
        },
    )
    .map_err(storage_error_object)?;

    let mut counters = LegiArchiveIngestCounters::default();
    let mut fatal_error = None::<ErrorObject>;
    let limit_members = limit_members.map(|limit| limit as usize);

    'archives: for archive in &archives {
        let archive_name = archive.file_name.as_str();
        // `read_archive_members_batched` owns the per-archive batching loop (pending buffer,
        // count/byte thresholds, flush-before-overflow, tail flush, `--limit-members` stop). The
        // flush closure captures the LEGI-specific `&mut counters`; the visit count is threaded by
        // value (read out into `counters.visited_members` below) to avoid borrowing `counters`
        // twice.
        let visited_before = counters.visited_members;
        let read = read_archive_members_batched(
            &archive.path,
            max_member_bytes,
            limit_members,
            visited_before,
            |pending_members, pending_member_bytes| {
                flush_legi_archive_member_batch(
                    &mut ingest_client,
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
                    "failed to read LEGI archive `{}`: {error}",
                    archive.path.display()
                )));
                break 'archives;
            }
        }
    }

    if fatal_error.is_none() {
        let scoped_backfill = LegiHierarchyBackfillScope {
            document_ids: counters
                .processed_article_document_ids
                .iter()
                .cloned()
                .collect(),
            section_source_uids: counters
                .processed_section_source_uids
                .iter()
                .cloned()
                .collect(),
            text_source_uids: counters
                .processed_text_source_uids
                .iter()
                .cloned()
                .collect(),
        };
        let full_resume_backfill = counters.skipped_compatible_members > 0;
        let backfill_scope = if full_resume_backfill {
            LegiHierarchyBackfillScope::default()
        } else {
            scoped_backfill
        };
        if full_resume_backfill || !backfill_scope.is_empty() {
            let outbox = jurisearch_storage::outbox::OutboxContext::new(
                &run_id,
                jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
            );
            match backfill_legi_article_hierarchy_from_metadata_scoped_with_client(
                &mut ingest_client,
                &backfill_scope,
                Some(&outbox),
            ) {
                Ok(report) => {
                    counters.hierarchy_backfilled_documents = report.documents_updated;
                    counters.hierarchy_backfill_invalidated_embeddings =
                        report.embeddings_invalidated;
                }
                Err(error) => {
                    fatal_error = Some(storage_error_object(error));
                }
            }
        }
    }

    let manifest_run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let final_manifest = legi_archive_manifest(
        &plan,
        latest_processed,
        &counters,
        manifest_run_status.as_str(),
        member_limited,
    );
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
    finish_ingest_run_with_client(
        &mut ingest_client,
        run_id.as_str(),
        run_status,
        error_message,
    )
    .map_err(storage_error_object)?;
    if let Some(error) = fatal_error {
        return Err(error);
    }
    // Refresh the replay snapshot ONLY on a completed run AND when the caller's policy allows it (the
    // producer passes `false` on delta-only cycles). `maybe_refresh_replay_snapshot` additionally honors
    // the `JURISEARCH_SKIP_REPLAY_SNAPSHOT` env skip.
    let replay_snapshot_cache = if run_status == IngestRunStatus::Completed {
        Some(maybe_refresh_replay_snapshot(
            &mut ingest_client,
            refresh_replay_snapshot,
        )?)
    } else {
        None
    };

    let body = json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest legi-archives",
        "run_id": run_id,
        "run_status": run_status.as_str(),
        "safe_mode": safe_mode,
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
        "parsed_metadata_members": counters.parsed_metadata_members,
        "persisted_metadata_members": counters.persisted_metadata_members,
        "hierarchy_backfill_scoped_documents": counters.processed_article_document_ids.len(),
        "hierarchy_backfill_scoped_sections": counters.processed_section_source_uids.len(),
        "hierarchy_backfill_scoped_texts": counters.processed_text_source_uids.len(),
        "hierarchy_backfilled_documents": counters.hierarchy_backfilled_documents,
        "hierarchy_backfill_invalidated_embeddings": counters.hierarchy_backfill_invalidated_embeddings,
        "skipped_members": counters.skipped_members,
        "skipped_compatible_members": counters.skipped_compatible_members,
        "skipped_no_text_articles": counters.skipped_no_text_articles,
        "failed_members": counters.failed_members,
        "quarantined_payloads": counters.quarantined_payloads,
        "parsed_metadata_roots": counters.parsed_metadata_roots,
        "unsupported_roots": counters.unsupported_roots,
        "quarantine_dir": quarantine_dir,
        "replay_snapshot_cache": replay_snapshot_cache
            .as_ref()
            .map(|snapshot| replay_snapshot_cache_value(snapshot.as_ref()))
    });

    Ok(IngestReport {
        source: ArchiveSource::Legi,
        run_id,
        run_status,
        archives_ingested: archives.len(),
        journal_cursor: latest_processed.map(|archive| archive.timestamp.compact().to_owned()),
        visited_members: counters.visited_members as u64,
        inserted_documents: counters.inserted_documents as u64,
        inserted_chunks: counters.inserted_chunks as u64,
        inserted_publisher_edges: counters.inserted_publisher_edges as u64,
        skipped_members: counters.skipped_members as u64,
        failed_members: counters.failed_members as u64,
        quarantined_payloads: counters.quarantined_payloads as u64,
        replay_snapshot: replay_snapshot_cache.flatten(),
        body,
    })
}

pub(crate) fn flush_legi_archive_member_batch(
    client: &mut postgres::Client,
    run_id: &str,
    archive_name: &str,
    pending_members: &mut Vec<ArchiveMember>,
    pending_member_bytes: &mut usize,
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    if pending_members.is_empty() {
        return Ok(());
    }
    process_legi_archive_member_batch(
        client,
        run_id,
        archive_name,
        pending_members,
        quarantine_dir,
        counters,
    )?;
    pending_members.clear();
    *pending_member_bytes = 0;
    Ok(())
}

pub(crate) fn process_legi_archive_member_batch(
    client: &mut postgres::Client,
    run_id: &str,
    archive_name: &str,
    members: &[ArchiveMember],
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute("SET LOCAL synchronous_commit TO off;")
        .map_err(StorageError::PostgresClient)?;
    // Prepare the document/chunk/edge upsert statements once for the whole batch instead of
    // re-parsing them for every member's insert.
    let projection_statements = prepare_legi_projection_statements(&mut transaction)?;
    let mut committed = LegiArchiveIngestCounters::default();
    for member in members {
        process_legi_archive_member(
            &mut transaction,
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
    Ok(())
}

pub(crate) fn process_legi_archive_member<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    projection_statements: &LegiProjectionStatements,
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    let source_payload_hash = source_payload_hash(&member.bytes);
    let compatibility = IngestCompatibility {
        parser_version: LEGI_PARSER_VERSION,
        schema_version: CANONICAL_SCHEMA_VERSION,
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
            // Same-run skips would collide with the existing member row and demote inserted work.
            if resume.previous_run_id.as_deref() != Some(run_id) {
                record_legi_member(
                    client,
                    run_id,
                    LegiMemberRecordInput {
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
            let record = record_legi_member(
                client,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_legi_member_error(
                client,
                run_id,
                Some(record.member_id),
                "validation_error",
                "compatibility_mismatch",
                message.as_str(),
                "none",
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

    match parse_legi_member(member) {
        Ok(ParsedLegiXml::Article(document)) => {
            let document = *document;
            let document_id = document.document_id.clone();
            let record = record_legi_member(
                client,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(document.source_uid.as_str()),
                    date_anchor: Some(document.valid_from.as_str()),
                    status: IngestMemberStatus::Parsed,
                    compatibility,
                },
            )?;
            let outbox = jurisearch_storage::outbox::OutboxContext::new(
                run_id,
                jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
            );
            let report = insert_legi_documents_with_statements(
                client,
                projection_statements,
                &[document],
                None,
                Some(&outbox),
            )?;
            update_ingest_member_status_with_client(
                client,
                record.member_id,
                IngestMemberStatus::Inserted,
                None,
            )?;
            counters.inserted_documents += report.documents;
            counters.inserted_chunks += report.chunks;
            counters.inserted_publisher_edges += report.publisher_edges;
            counters.processed_article_document_ids.insert(document_id);
        }
        Ok(ParsedLegiXml::TextVersion(text)) => {
            process_legi_metadata_root(
                client,
                run_id,
                archive_name,
                member,
                compatibility,
                counters,
                "TEXTE_VERSION",
                Some(text.text_id.as_str()),
                Some(text.valid_from.as_str()),
                LegiMetadataRoot::TextVersion(text.as_ref()),
            )?;
        }
        Ok(ParsedLegiXml::SectionTa(section)) => {
            let section_source_uid = section.section_id.clone();
            process_legi_metadata_root(
                client,
                run_id,
                archive_name,
                member,
                compatibility,
                counters,
                "SECTION_TA",
                section.section_id.as_deref(),
                Some(section.valid_from.as_str()),
                LegiMetadataRoot::SectionTa(section.as_ref()),
            )?;
            if let Some(section_source_uid) = section_source_uid {
                counters
                    .processed_section_source_uids
                    .insert(section_source_uid);
            }
        }
        Ok(ParsedLegiXml::TextStruct(text_struct)) => {
            let text_source_uid = text_struct.text_id.clone();
            process_legi_metadata_root(
                client,
                run_id,
                archive_name,
                member,
                compatibility,
                counters,
                "TEXTELR",
                Some(text_struct.text_id.as_str()),
                text_struct.source_date_debut_hint.as_deref(),
                LegiMetadataRoot::TextStruct(text_struct.as_ref()),
            )?;
            counters.processed_text_source_uids.insert(text_source_uid);
        }
        Ok(ParsedLegiXml::UnsupportedRoot { root }) => {
            *counters.unsupported_roots.entry(root.clone()).or_default() += 1;
            record_legi_member(
                client,
                run_id,
                LegiMemberRecordInput {
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
        Err(error) => {
            if is_no_text_article_error(&error) {
                record_legi_member(
                    client,
                    run_id,
                    LegiMemberRecordInput {
                        archive_name,
                        member_path: member.member_path.as_str(),
                        source_entity: legi_article_id_from_member_path(
                            member.member_path.as_str(),
                        ),
                        date_anchor: None,
                        status: IngestMemberStatus::Skipped,
                        compatibility,
                    },
                )?;
                counters.skipped_members += 1;
                counters.skipped_no_text_articles += 1;
                return Ok(());
            }
            let (error_class, error_code) = legi_parse_error_class(&error);
            let message = error.to_string();
            let record = record_legi_member(
                client,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_legi_member_error(
                client,
                run_id,
                Some(record.member_id),
                error_class,
                error_code,
                message.as_str(),
                "none",
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn process_legi_metadata_root<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    compatibility: IngestCompatibility<'_>,
    counters: &mut LegiArchiveIngestCounters,
    root: &str,
    source_uid: Option<&str>,
    date_anchor: Option<&str>,
    metadata_root: LegiMetadataRoot<'_>,
) -> Result<(), StorageError> {
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );
    let report = insert_legi_metadata_roots_with_client(client, &[metadata_root], Some(&outbox))?;
    *counters
        .parsed_metadata_roots
        .entry(root.to_owned())
        .or_default() += 1;
    record_legi_member(
        client,
        run_id,
        LegiMemberRecordInput {
            archive_name,
            member_path: member.member_path.as_str(),
            source_entity: source_uid.or(Some(root)),
            date_anchor,
            status: IngestMemberStatus::Skipped,
            compatibility,
        },
    )?;
    counters.parsed_metadata_members += 1;
    counters.persisted_metadata_members += report.metadata_roots;
    counters.skipped_members += 1;
    Ok(())
}

pub(crate) struct LegiMemberRecordInput<'a> {
    pub(crate) archive_name: &'a str,
    pub(crate) member_path: &'a str,
    pub(crate) source_entity: Option<&'a str>,
    pub(crate) date_anchor: Option<&'a str>,
    pub(crate) status: IngestMemberStatus,
    pub(crate) compatibility: IngestCompatibility<'a>,
}

pub(crate) fn record_legi_member<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    input: LegiMemberRecordInput<'_>,
) -> Result<jurisearch_storage::ingest_accounting::IngestMemberRecord, StorageError> {
    record_ingest_member_with_client(
        client,
        &IngestMemberInput {
            run_id,
            archive_name: input.archive_name,
            member_path: input.member_path,
            source: "legi",
            source_entity: input.source_entity,
            date_anchor: input.date_anchor,
            status: input.status,
            compatibility: input.compatibility,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_legi_member_error<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    member_id: Option<i64>,
    error_class: &str,
    error_code: &str,
    message: &str,
    retry_policy: &str,
    archive_name: &str,
    member: &ArchiveMember,
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
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
            retry_policy,
            context_json: Some(context.as_str()),
        },
    )?;
    Ok(())
}

pub(crate) fn legi_parse_error_class(error: &LegiParseError) -> (&'static str, &'static str) {
    match error {
        LegiParseError::Xml { .. } => ("parse_error", "parse_malformed_xml"),
        LegiParseError::MissingRequiredField { .. } => {
            ("validation_error", "validation_missing_required_field")
        }
        LegiParseError::InvalidDate { .. } => ("validation_error", "validation_invalid_date"),
        LegiParseError::InvalidId { .. } => ("validation_error", "validation_invalid_id"),
    }
}

pub(crate) fn is_no_text_article_error(error: &LegiParseError) -> bool {
    matches!(
        error,
        LegiParseError::MissingRequiredField { entity, field }
            if *entity == "article" && *field == "BLOC_TEXTUEL/CONTENU"
    )
}

pub(crate) fn legi_article_id_from_member_path(member_path: &str) -> Option<&str> {
    // Best-effort provenance for skipped ARTICLE members: official archive paths
    // end with the LEGIARTI source UID filename.
    let start = member_path.find("LEGIARTI")?;
    let end = start + "LEGIARTI".len() + 12;
    let candidate = member_path.get(start..end)?;
    let suffix = candidate.strip_prefix("LEGIARTI")?;
    if suffix.len() == 12 && suffix.chars().all(|character| character.is_ascii_digit()) {
        Some(candidate)
    } else {
        None
    }
}
