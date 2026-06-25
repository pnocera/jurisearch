//! Official-source ingestion orchestration: the `ingest` subcommands and `sync`. Converts
//! CLI flags into parser/storage/enrichment calls and JSON artifacts — the LEGI/JURI archive
//! run lifecycle (plan/select/read/flush/quarantine/accounting/replay-snapshot), embed-chunks,
//! hierarchy backfill, and the zone-unit pipeline (enrich-zones/build/embed). The embedding
//! pool wrappers live in crate::embedding_runtime; the legislation-citation payloads live in
//! crate::enrichment::legislation.

use crate::*;

/// Incremental sync: pull a source's new delta archives into the existing index. Reuses the proven
/// per-source ingest path (and its compatibility-based resume, which skips already-ingested members
/// and blocks parser/schema/code/source-payload mismatches — so sync can never silently mix
/// incompatible versions). `--since` bounds which delta archives are scanned so a sync never
/// re-reads the full baseline corpus; `status.corpus_sources` then reports the new freshness.
pub(crate) fn sync_payload(args: SyncArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let source_token = args.source.as_deref().ok_or_else(|| {
        ErrorObject::bad_input("sync requires --source (legi|cass|capp|inca|jade)")
    })?;
    let source = ArchiveSource::from_token(source_token).ok_or_else(|| {
        ErrorObject::bad_input(format!(
            "unknown sync --source `{source_token}`; expected legi|cass|capp|inca|jade"
        ))
    })?;
    let archives_dir = args
        .archives_dir
        .as_deref()
        .ok_or_else(|| ErrorObject::bad_input("sync requires --archives-dir"))?;
    let since_compact = match args.since.as_deref() {
        None => None,
        Some(raw) => Some(normalize_since(raw).ok_or_else(|| {
            ErrorObject::bad_input(format!(
                "invalid --since `{raw}`; expected YYYY-MM-DD or YYYYMMDDHHMMSS"
            ))
        })?),
    };
    // Incremental: a prior full build already ingested the baseline; only newer deltas are pulled.
    let archive_filter = ArchiveSyncFilter {
        incremental: true,
        since_compact: since_compact.as_deref(),
    };

    let mut response = if source.is_jurisprudence() {
        ingest_juri_archives_payload(
            index_dir,
            source,
            archives_dir,
            None,
            None,
            DEFAULT_MEMBER_BYTE_LIMIT,
            args.quarantine_dir.as_deref(),
            args.safe_mode,
            archive_filter,
        )?
    } else {
        ingest_legi_archives_payload(
            index_dir,
            archives_dir,
            None,
            None,
            DEFAULT_MEMBER_BYTE_LIMIT,
            args.quarantine_dir.as_deref(),
            args.safe_mode,
            archive_filter,
        )?
    };

    // Re-frame the ingest result as a sync result.
    if let Value::Object(map) = &mut response {
        map.insert("command".to_owned(), json!("sync"));
        map.insert("mode".to_owned(), json!("incremental"));
        map.insert("source".to_owned(), json!(source.as_str()));
        map.insert("synced_since".to_owned(), json!(args.since));
    }
    Ok(response)
}

pub(crate) fn emit_ingest(ingest: IngestCommand, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match ingest.command {
        Some(IngestSubcommand::PlanArchives {
            source,
            archives_dir,
        }) => {
            let source = ArchiveSource::from(source);
            let plan = plan_from_dir(source, &archives_dir).map_err(|error| {
                anyhow::anyhow!(
                    "failed to plan archives in `{}`: {error}",
                    archives_dir.display()
                )
            })?;
            write_json(&json!({
                "schema_version": SCHEMA_VERSION,
                "command": "ingest plan-archives",
                "plan": plan,
            }))
        }
        Some(IngestSubcommand::LegiArchives {
            archives_dir,
            run_id,
            limit_members,
            max_member_bytes,
            quarantine_dir,
            safe_mode,
        }) => {
            if limit_members == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest legi-archives --limit-members must be at least 1 when provided",
                ));
            }
            if max_member_bytes == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest legi-archives --max-member-bytes must be at least 1",
                ));
            }
            match ingest_legi_archives_payload(
                index_dir,
                archives_dir.as_path(),
                run_id,
                limit_members,
                max_member_bytes,
                quarantine_dir.as_deref(),
                safe_mode,
                ArchiveSyncFilter::default(),
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::JuriArchives {
            source,
            archives_dir,
            run_id,
            limit_members,
            max_member_bytes,
            quarantine_dir,
            safe_mode,
        }) => {
            if limit_members == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest juri-archives --limit-members must be at least 1 when provided",
                ));
            }
            if max_member_bytes == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest juri-archives --max-member-bytes must be at least 1",
                ));
            }
            match ingest_juri_archives_payload(
                index_dir,
                ArchiveSource::from(source),
                archives_dir.as_path(),
                run_id,
                limit_members,
                max_member_bytes,
                quarantine_dir.as_deref(),
                safe_mode,
                ArchiveSyncFilter::default(),
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EmbedChunks {
            limit,
            index_lists,
            batch_size,
            pool_concurrency,
        }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --limit must be at least 1 when provided",
                ));
            }
            if index_lists == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --index-lists must be at least 1",
                ));
            }
            if batch_size == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --batch-size must be at least 1",
                ));
            }
            if pool_concurrency == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --pool-concurrency must be at least 1",
                ));
            }
            match embed_chunks_payload(index_dir, limit, index_lists, batch_size, pool_concurrency)
            {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EnrichZones {
            source,
            limit,
            since,
            concurrency,
            order,
        }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest enrich-zones --limit must be at least 1 when provided",
                ));
            }
            if concurrency == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest enrich-zones --concurrency must be at least 1",
                ));
            }
            match enrich_zones_payload(
                index_dir,
                &source,
                limit,
                since.as_deref(),
                concurrency,
                order,
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::BuildZoneUnits { limit, rebuild }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest build-zone-units --limit must be at least 1 when provided",
                ));
            }
            match build_zone_units_payload(index_dir, limit, rebuild) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EmbedZoneUnits {
            limit,
            index_lists,
            batch_size,
            pool_concurrency,
        }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --limit must be at least 1 when provided",
                ));
            }
            if index_lists == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --index-lists must be at least 1",
                ));
            }
            if batch_size == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --batch-size must be at least 1",
                ));
            }
            if pool_concurrency == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --pool-concurrency must be at least 1",
                ));
            }
            match embed_zone_units_payload(index_dir, limit, index_lists, batch_size, pool_concurrency)
            {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::CollectLegislationCitations { limit }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest collect-legislation-citations --limit must be at least 1 when provided",
                ));
            }
            match collect_legislation_citations_payload(index_dir, limit) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EnrichLegislationCitations { limit, retry_errors }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest enrich-legislation-citations --limit must be at least 1 when provided",
                ));
            }
            match enrich_legislation_citations_payload(index_dir, limit, retry_errors) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::BackfillLegiHierarchy) => {
            match backfill_legi_hierarchy_payload(index_dir) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        None => emit_error(ErrorObject::not_implemented("ingest")),
    }
}

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
) -> Value {
    // Freshness/source_version reflect the latest archive ACTUALLY processed (so an incremental or
    // no-op sync never advances reported corpus freshness for archives it did not read).
    let freshness = latest_processed.map_or(Value::Null, |archive| {
        json!({
            "latest_archive": archive.file_name.as_str(),
            "latest_archive_kind": archive.kind,
            "latest_archive_timestamp": archive.timestamp.to_string(),
            "latest_archive_timestamp_compact": archive.timestamp.compact()
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

pub(crate) fn planned_archive_manifest(archive: &PlannedArchive) -> Value {
    json!({
        "source": archive.source,
        "kind": archive.kind,
        "timestamp": archive.timestamp.to_string(),
        "timestamp_compact": archive.timestamp.compact(),
        "file_name": archive.file_name.as_str()
    })
}

/// Which archives in a plan to process. The default (`incremental=false`, no `since`) processes the
/// baseline plus every delta — the full-build behavior. `sync` uses `incremental=true` (a prior full
/// build already ingested the baseline) plus an optional `since_compact` lower bound on delta
/// timestamps so a sync never re-scans the entire baseline corpus.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ArchiveSyncFilter<'a> {
    pub(crate) incremental: bool,
    pub(crate) since_compact: Option<&'a str>,
}

/// Ordered list of plan archives to process under `filter` (baseline first when not incremental,
/// then deltas at/after `since_compact`). Deltas keep the planner's deterministic order.
pub(crate) fn select_archives_to_process<'a>(
    plan: &'a ArchivePlan,
    filter: ArchiveSyncFilter<'_>,
) -> Vec<&'a PlannedArchive> {
    let mut archives = Vec::new();
    if !filter.incremental {
        archives.push(&plan.baseline);
    }
    for delta in &plan.deltas {
        if filter
            .since_compact
            .is_none_or(|since| delta.timestamp.compact() >= since)
        {
            archives.push(delta);
        }
    }
    archives
}

/// Normalize a `--since` value to the 14-digit compact archive-timestamp form for lexicographic
/// comparison. Accepts ONLY the two documented shapes — `YYYY-MM-DD` or compact `YYYYMMDDHHMMSS` —
/// and returns `None` for anything else (e.g. `2025/01/15`, `2025-01-15T00:00:00`, noise).
pub(crate) fn normalize_since(since: &str) -> Option<String> {
    let bytes = since.as_bytes();
    if bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        let digits: String = since.chars().filter(char::is_ascii_digit).collect();
        return Some(format!("{digits}000000"));
    }
    if since.len() == 14 && since.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(since.to_owned());
    }
    None
}

pub(crate) fn ingest_legi_archives_payload(
    index_dir: Option<&Path>,
    archives_dir: &Path,
    run_id: Option<String>,
    limit_members: Option<u32>,
    max_member_bytes: u64,
    quarantine_dir: Option<&Path>,
    safe_mode: bool,
    archive_filter: ArchiveSyncFilter<'_>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_configured_index_dir(index_dir)?;
    let postgres = open_index_for_bulk_ingest(index_dir.as_path())?;
    let mut ingest_client =
        postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
            .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
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
    let initial_manifest = legi_archive_manifest(
        &plan,
        latest_processed,
        &LegiArchiveIngestCounters::default(),
        IngestRunStatus::Running.as_str(),
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
        let mut pending_members = Vec::with_capacity(LEGI_INGEST_TRANSACTION_BATCH_SIZE);
        let mut pending_member_bytes = 0usize;
        let read_result = for_each_xml_member_until(&archive.path, max_member_bytes, |member| {
            if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                return Ok(ArchiveVisit::Stop);
            }
            counters.visited_members += 1;
            let member_bytes = member.bytes.len();
            if !pending_members.is_empty()
                && pending_member_bytes.saturating_add(member_bytes)
                    > LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT
                && let Err(error) = flush_legi_archive_member_batch(
                    &mut ingest_client,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            pending_members.push(member);
            pending_member_bytes = pending_member_bytes.saturating_add(member_bytes);
            if (pending_members.len() >= LEGI_INGEST_TRANSACTION_BATCH_SIZE
                || pending_member_bytes >= LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT)
                && let Err(error) = flush_legi_archive_member_batch(
                    &mut ingest_client,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            Ok(
                if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                    ArchiveVisit::Stop
                } else {
                    ArchiveVisit::Continue
                },
            )
        });

        if fatal_error.is_none()
            && read_result.is_ok()
            && !pending_members.is_empty()
            && let Err(error) = flush_legi_archive_member_batch(
                &mut ingest_client,
                run_id.as_str(),
                archive_name,
                &mut pending_members,
                &mut pending_member_bytes,
                quarantine_dir,
                &mut counters,
            )
        {
            fatal_error = Some(storage_error_object(error));
        }

        if let Err(error) = read_result {
            let error = ErrorObject::bad_input(format!(
                "failed to read LEGI archive `{}`: {error}",
                archive.path.display()
            ));
            fatal_error = Some(error);
        }
        if fatal_error.is_some()
            || limit_members.is_some_and(|limit| counters.visited_members >= limit)
        {
            break 'archives;
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
            match backfill_legi_article_hierarchy_from_metadata_scoped(&postgres, &backfill_scope) {
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
    let final_manifest =
        legi_archive_manifest(&plan, latest_processed, &counters, manifest_run_status.as_str());
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
    let replay_snapshot_cache = if run_status == IngestRunStatus::Completed {
        Some(maybe_refresh_replay_snapshot(&postgres)?)
    } else {
        None
    };

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest legi-archives",
        "run_id": run_id,
        "run_status": run_status.as_str(),
        "safe_mode": safe_mode,
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
    }))
}

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

/// Monotonic in-process counter making default run IDs unique even within the same nanosecond.
pub(crate) static RUN_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A collision-resistant run-id suffix. `ingest`/`sync` runs without an explicit `--run-id` must not
/// share an id: `start_ingest_run_with_client` upserts on `ON CONFLICT (run_id)`, so a collision lets
/// a later run overwrite an earlier completed run's manifest (e.g. two rapid same-source syncs in the
/// same second erasing the first run's freshness). Nanosecond clock + PID + an in-process counter
/// makes the id unique across rapid same-process and separate-process invocations.
pub(crate) fn unique_run_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let sequence = RUN_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{pid}-{sequence}")
}

pub(crate) fn default_juri_run_id(source: ArchiveSource) -> String {
    format!("{}-{}", source.as_str(), unique_run_suffix())
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
        let mut pending_members = Vec::with_capacity(LEGI_INGEST_TRANSACTION_BATCH_SIZE);
        let mut pending_member_bytes = 0usize;
        let read_result = for_each_xml_member_until(&archive.path, max_member_bytes, |member| {
            if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                return Ok(ArchiveVisit::Stop);
            }
            counters.visited_members += 1;
            let member_bytes = member.bytes.len();
            if !pending_members.is_empty()
                && pending_member_bytes.saturating_add(member_bytes)
                    > LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT
                && let Err(error) = flush_juri_archive_member_batch(
                    &mut ingest_client,
                    source,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            pending_members.push(member);
            pending_member_bytes = pending_member_bytes.saturating_add(member_bytes);
            if (pending_members.len() >= LEGI_INGEST_TRANSACTION_BATCH_SIZE
                || pending_member_bytes >= LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT)
                && let Err(error) = flush_juri_archive_member_batch(
                    &mut ingest_client,
                    source,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            Ok(
                if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                    ArchiveVisit::Stop
                } else {
                    ArchiveVisit::Continue
                },
            )
        });

        if fatal_error.is_none()
            && read_result.is_ok()
            && !pending_members.is_empty()
            && let Err(error) = flush_juri_archive_member_batch(
                &mut ingest_client,
                source,
                run_id.as_str(),
                archive_name,
                &mut pending_members,
                &mut pending_member_bytes,
                quarantine_dir,
                &mut counters,
            )
        {
            fatal_error = Some(storage_error_object(error));
        }

        if let Err(error) = read_result {
            fatal_error = Some(ErrorObject::bad_input(format!(
                "failed to read {} archive `{}`: {error}",
                source.as_str(),
                archive.path.display()
            )));
        }
        if fatal_error.is_some()
            || limit_members.is_some_and(|limit| counters.visited_members >= limit)
        {
            break 'archives;
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
            let report = insert_legi_documents_with_statements(
                client,
                projection_statements,
                &[document],
                None,
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
    let report = insert_legi_metadata_roots_with_client(client, &[metadata_root])?;
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

pub(crate) fn maybe_quarantine_payload(
    quarantine_dir: Option<&Path>,
    run_id: &str,
    archive_name: &str,
    member_path: &str,
    bytes: &[u8],
) -> Result<bool, StorageError> {
    let Some(quarantine_dir) = quarantine_dir else {
        return Ok(false);
    };
    let run_dir = quarantine_dir.join(sanitize_quarantine_component(run_id));
    fs::create_dir_all(&run_dir).map_err(StorageError::Io)?;
    let file_name = format!(
        "{}__{}",
        sanitize_quarantine_component(archive_name),
        sanitize_quarantine_component(member_path)
    );
    fs::write(run_dir.join(file_name), bytes).map_err(StorageError::Io)?;
    Ok(true)
}

pub(crate) fn sanitize_quarantine_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
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

pub(crate) fn backfill_legi_hierarchy_payload(index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    // Hierarchy backfill can delete chunk_embeddings / clear embedding fingerprints, making the
    // index no longer query-ready; drop the readiness cache up front so a stale "ready" entry can
    // never let a subsequent search skip the live coverage check.
    invalidate_cached_query_readiness(&postgres).map_err(storage_error_object)?;
    let report =
        backfill_legi_article_hierarchy_from_metadata(&postgres).map_err(storage_error_object)?;
    let replay_snapshot = maybe_refresh_replay_snapshot(&postgres)?;

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest backfill-legi-hierarchy",
        "index_dir": index_dir,
        "scope": "full",
        "hierarchy_backfilled_documents": report.documents_updated,
        "hierarchy_backfill_invalidated_embeddings": report.embeddings_invalidated,
        "embedding_rebuild_required": report.embeddings_invalidated > 0,
        "recommended_next_command": if report.embeddings_invalidated > 0 {
            Some("jurisearch ingest embed-chunks")
        } else {
            None::<&str>
        },
        "replay_snapshot_cache": replay_snapshot_cache_value(replay_snapshot.as_ref())
    }))
}

pub(crate) fn default_legi_run_id() -> String {
    format!("legi-{}", unique_run_suffix())
}

/// Whether maintenance commands should skip the (expensive, full-table MD5) replay-snapshot refresh
/// at their command boundary. Default false: the refresh keeps `status` cheap via the cached
/// signature. Setting `JURISEARCH_SKIP_REPLAY_SNAPSHOT` skips it (hundreds of seconds on a large
/// index) at the cost of a stale cached signature until the next `status --deep`.
pub(crate) fn replay_snapshot_refresh_skipped() -> bool {
    std::env::var_os("JURISEARCH_SKIP_REPLAY_SNAPSHOT").is_some()
}

/// Refresh the replay snapshot unless skipped via env. Returns `None` when skipped.
pub(crate) fn maybe_refresh_replay_snapshot(
    postgres: &ManagedPostgres,
) -> Result<Option<ReplaySnapshotReport>, ErrorObject> {
    if replay_snapshot_refresh_skipped() {
        Ok(None)
    } else {
        Ok(Some(
            refresh_replay_snapshot(postgres).map_err(storage_error_object)?,
        ))
    }
}

/// Report value for a maybe-refreshed snapshot: the full cache JSON when refreshed, else `skipped`.
pub(crate) fn replay_snapshot_cache_value(snapshot: Option<&ReplaySnapshotReport>) -> Value {
    match snapshot {
        Some(snapshot) => replay_snapshot_cache_json("refreshed", snapshot),
        None => json!({ "source": "skipped" }),
    }
}

pub(crate) fn replay_snapshot_cache_json(source: &str, snapshot: &ReplaySnapshotReport) -> Value {
    json!({
        "source": source,
        "status": snapshot.status(),
        "signature": snapshot.signature.as_str(),
        "documents": snapshot.documents.count,
        "chunks": snapshot.chunks.count,
        "publisher_edges": snapshot.publisher_edges.count,
        "embeddings": snapshot.embeddings.count,
        "manifests": snapshot.manifests.count
    })
}

/// Outcome of a single decision enrichment attempt, for backfill accounting.
#[derive(Clone, Copy)]
pub(crate) enum ZoneEnrichOutcome {
    /// Resolved with official zones (a fresh `ok` `decision_zones` row).
    Official,
    /// No official zone (not_found / unsupported / invalid_offsets) — cached, not an error.
    Fallback,
    /// A storage/transport failure during enrichment (logged, never aborts the backfill).
    Error,
}

/// Eagerly backfill official Judilibre zones for a Cassation source (`cass`/`inca`) into
/// `decision_zones`, paging the resolver-reachable candidate set and resolving each decision via the
/// shipped `enrich_decision_from_judilibre` (now `text_hash`-populating). Resumable: every attempt
/// writes a `decision_zones` row, so a re-run skips fresh rows. Conservative bounded concurrency keeps
/// the Judilibre request rate well under the live limit.
pub(crate) fn enrich_zones_payload(
    index_dir: Option<&Path>,
    source: &str,
    limit: Option<u32>,
    since: Option<&str>,
    concurrency: usize,
    order: CliEnrichZoneOrder,
) -> Result<Value, ErrorObject> {
    if !matches!(source, "cass" | "inca") {
        return Err(ErrorObject::bad_input(
            "ingest enrich-zones --source must be 'cass' or 'inca' (Judilibre covers only Cour de cassation)",
        ));
    }
    // Preflight: validate Judilibre (KeyId) credentials via the SAME config the workers use
    // (`OfficialApiConfig::from_env`), which accepts `JURISEARCH_PISTE_JUDILIBRE_KEY_ID` / `PISTE_API_KEY`
    // in production and `PISTE_SANDBOX_API_KEY` in sandbox — so a supported deployment is not rejected up
    // front and the message matches the real credential contract.
    if OfficialApiConfig::from_env().judilibre_key_id.is_none() {
        return Err(dependency_unavailable(
            "no Judilibre (PISTE) API key configured; set JURISEARCH_PISTE_JUDILIBRE_KEY_ID or \
             PISTE_API_KEY (PISTE_SANDBOX_API_KEY in sandbox) before running zone enrichment",
        ));
    }
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;

    let mut considered: u64 = 0;
    let mut official: u64 = 0;
    let mut fallback: u64 = 0;
    let mut errors: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        // Respect --limit across pages.
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(considered).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(ENRICH_ZONES_PAGE_SIZE)
            }
            None => ENRICH_ZONES_PAGE_SIZE,
        };
        let page_json = enrich_zone_candidates_json(
            &postgres,
            source,
            cursor.as_deref(),
            since,
            page_limit,
            order.into(),
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let doc_ids: Vec<String> = page["candidates"]
            .as_array()
            .map(|candidates| {
                candidates
                    .iter()
                    .filter_map(|candidate| candidate["document_id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if doc_ids.is_empty() {
            break;
        }
        for outcome in enrich_zone_page_concurrently(&postgres, &doc_ids, concurrency) {
            considered += 1;
            match outcome {
                ZoneEnrichOutcome::Official => official += 1,
                ZoneEnrichOutcome::Fallback => fallback += 1,
                ZoneEnrichOutcome::Error => errors += 1,
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let coverage: Value =
        serde_json::from_str(&zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest enrich-zones",
        "index_dir": index_dir.display().to_string(),
        "source": source,
        "since": since,
        "concurrency": concurrency,
        "order": order.as_str(),
        "considered": considered,
        "official_ok": official,
        "fallback": fallback,
        "errors": errors,
        "coverage": coverage,
    }))
}

/// Enrich one page of decisions with bounded concurrency (codex-recommended model (b)): one owning
/// `ManagedPostgres` stays on the main thread; each scoped worker opens its OWN `postgres::Client` +
/// `PisteClient` from the `Send` connection string and resolves a contiguous slice via the thread-safe
/// core. A worker that cannot even connect, or panics, drops only its slice from accounting (counted as
/// errors) rather than aborting the whole backfill.
pub(crate) fn enrich_zone_page_concurrently(
    postgres: &ManagedPostgres,
    doc_ids: &[String],
    concurrency: usize,
) -> Vec<ZoneEnrichOutcome> {
    let workers = concurrency.max(1).min(doc_ids.len().max(1));
    let connection_string = postgres.connection_string();
    let mut groups: Vec<Vec<&str>> = (0..workers).map(|_| Vec::new()).collect();
    for (index, doc_id) in doc_ids.iter().enumerate() {
        groups[index % workers].push(doc_id.as_str());
    }
    std::thread::scope(|scope| {
        let connection_string = &connection_string;
        let handles: Vec<(usize, _)> = groups
            .into_iter()
            .map(|group| {
                let group_len = group.len();
                let handle = scope.spawn(move || {
                    let mut db =
                        match postgres::Client::connect(connection_string, postgres::NoTls) {
                            Ok(db) => db,
                            // Whole slice fails to connect -> count as errors, don't abort the run.
                            Err(_) => return vec![ZoneEnrichOutcome::Error; group.len()],
                        };
                    let piste = PisteClient::new(OfficialApiConfig::from_env());
                    group
                        .into_iter()
                        .map(|doc_id| {
                            match enrich_decision_from_judilibre_with_client(&mut db, &piste, doc_id)
                            {
                                Ok(Some(_)) => ZoneEnrichOutcome::Official,
                                Ok(None) => ZoneEnrichOutcome::Fallback,
                                Err(_) => ZoneEnrichOutcome::Error,
                            }
                        })
                        .collect::<Vec<_>>()
                });
                (group_len, handle)
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|(group_len, handle)| {
                worker_outcomes_or_errors(handle.join().ok(), group_len)
            })
            .collect()
    })
}

/// Map a scoped worker's join result to per-decision outcomes. A panicked worker (join `None`) counts
/// its WHOLE slice as errors rather than silently dropping those decisions from the backfill accounting.
pub(crate) fn worker_outcomes_or_errors(
    returned: Option<Vec<ZoneEnrichOutcome>>,
    group_len: usize,
) -> Vec<ZoneEnrichOutcome> {
    returned.unwrap_or_else(|| vec![ZoneEnrichOutcome::Error; group_len])
}

/// Derive a decision's `zone_units` rows from its cached `zones_json` object (motivations/moyens/
/// dispositif fragment text). One row per non-empty fragment with a contiguous per-zone `fragment_index`.
/// Borrows the fragment text from `zones`, so the returned rows must be used before `zones` is dropped.
pub(crate) fn derive_zone_unit_rows<'a>(
    document_id: &'a str,
    source: &'a str,
    text_hash: &'a str,
    zones: &'a Value,
) -> Vec<ZoneUnitRow<'a>> {
    let mut rows = Vec::new();
    for zone in ["motivations", "moyens", "dispositif"] {
        let Some(fragments) = zones[zone].as_array() else {
            continue;
        };
        let mut fragment_index = 0i32;
        for fragment in fragments {
            let Some(text) = fragment["text"].as_str() else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            rows.push(ZoneUnitRow {
                document_id,
                zone,
                fragment_index,
                body: text,
                search_body: text,
                source,
                text_hash,
                builder_version: ZONE_UNIT_BUILDER_VERSION,
            });
            fragment_index += 1;
        }
    }
    rows
}

/// `ingest build-zone-units`: derive `zone_units` from the cached official zones in `decision_zones`.
/// Pages the derivable set (fresh `ok` Cassation rows with stale/absent units), deriving each decision's
/// units in one idempotent `replace_zone_units_for_document` transaction.
pub(crate) fn build_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    rebuild: bool,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;

    let mut decisions: u64 = 0;
    let mut units_written: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(decisions).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(BUILD_ZONE_UNITS_PAGE_SIZE)
            }
            None => BUILD_ZONE_UNITS_PAGE_SIZE,
        };
        let page_json = load_derivable_decision_zones_json(
            &postgres,
            ZONE_UNIT_BUILDER_VERSION,
            rebuild,
            cursor.as_deref(),
            page_limit,
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let candidates = page["candidates"].as_array().cloned().unwrap_or_default();
        if candidates.is_empty() {
            break;
        }
        for candidate in &candidates {
            let document_id = candidate["document_id"].as_str().unwrap_or_default();
            if document_id.is_empty() {
                continue;
            }
            let source = candidate["source"].as_str().unwrap_or_default();
            let text_hash = candidate["text_hash"].as_str().unwrap_or_default();
            let rows = derive_zone_unit_rows(document_id, source, text_hash, &candidate["zones"]);
            replace_zone_units_for_document(&postgres, document_id, &rows)
                .map_err(storage_error_object)?;
            decisions += 1;
            units_written += rows.len() as u64;
            if let Some(limit) = limit
                && decisions >= u64::from(limit)
            {
                break;
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let coverage: Value =
        serde_json::from_str(&zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest build-zone-units",
        "index_dir": index_dir.display().to_string(),
        "builder_version": ZONE_UNIT_BUILDER_VERSION,
        "rebuild": rebuild,
        "decisions_derived": decisions,
        "zone_units_written": units_written,
        "coverage": coverage,
    }))
}

/// `ingest embed-zone-units`: embed `zone_units` via the SAME OpenRouter pool + fingerprint as
/// `embed-chunks`, then finalize the separate zone-unit dense ANN index. Mirrors the embed-chunks
/// streaming/finalize flow against the zone tables; the chunk dense path is untouched.
pub(crate) fn embed_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    ensure_embedding_runtime_ready(&embedding_config, false)?;
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let endpoint_configs = embedding_endpoint_pool_configs(
        &embedding_config,
        &loaded_embedding.pool_endpoints,
        &expected_fingerprint,
        embedding_fingerprint.as_str(),
    )?;
    let dimension = i32::try_from(embedding_config.dimension).map_err(|_| {
        dependency_unavailable(format!(
            "embedding dimension {} is too large for dense rebuild metadata",
            embedding_config.dimension
        ))
    })?;
    if dimension != DENSE_VECTOR_DIMENSION {
        return Err(dependency_unavailable(format!(
            "embedding dimension {} does not match storage vector({})",
            embedding_config.dimension, DENSE_VECTOR_DIMENSION
        )));
    }

    let to_chunk_inputs = |inputs: Vec<jurisearch_storage::zone_units::ZoneUnitEmbeddingInput>| {
        inputs
            .into_iter()
            .map(|input| ChunkEmbeddingInput {
                chunk_id: input.zone_unit_id,
                embedding_text: input.embedding_text,
            })
            .collect::<Vec<_>>()
    };

    let embedding_run = if let Some(limit) = limit {
        let inputs = load_zone_unit_embedding_inputs(
            &postgres,
            embedding_fingerprint.as_str(),
            embedding_config.model.as_str(),
            dimension,
            Some(limit.saturating_add(1)),
        )
        .map_err(storage_error_object)?;
        if inputs.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(ErrorObject::bad_input(
                "ingest embed-zone-units --limit would leave zone units unembedded; run on a smaller smoke index or omit --limit to finalize the full zone index",
            ));
        }
        if inputs.is_empty() {
            return Err(no_results("no zone units are available to embed"));
        }
        embed_and_insert_zone_units_with_pool(
            &postgres,
            to_chunk_inputs(inputs),
            &endpoint_configs,
            embedding_fingerprint.as_str(),
            &embedding_config,
            batch_size,
            pool_concurrency,
        )?
    } else {
        let mut run = EmbeddingPoolRun {
            chunks_considered: 0,
            embeddings_inserted: 0,
            embedding_inputs_truncated: 0,
            endpoint_stats: Vec::new(),
        };
        loop {
            let page = load_zone_unit_embedding_inputs(
                &postgres,
                embedding_fingerprint.as_str(),
                embedding_config.model.as_str(),
                dimension,
                Some(EMBED_STREAM_PAGE_SIZE),
            )
            .map_err(storage_error_object)?;
            if page.is_empty() {
                break;
            }
            let page_run = embed_and_insert_zone_units_with_pool(
                &postgres,
                to_chunk_inputs(page),
                &endpoint_configs,
                embedding_fingerprint.as_str(),
                &embedding_config,
                batch_size,
                pool_concurrency,
            )?;
            run.chunks_considered += page_run.chunks_considered;
            run.embeddings_inserted += page_run.embeddings_inserted;
            run.embedding_inputs_truncated += page_run.embedding_inputs_truncated;
            merge_embedding_endpoint_stats(&mut run.endpoint_stats, page_run.endpoint_stats);
        }
        if run.chunks_considered == 0 {
            return Err(no_results("no zone units are available to embed"));
        }
        run
    };

    let rebuild = finalize_zone_dense_rebuild(
        &postgres,
        &DenseRebuildSpec {
            embedding_fingerprint: embedding_fingerprint.as_str(),
            model: embedding_config.model.as_str(),
            dimension,
            normalize: embedding_config.normalize,
            provisional: embedding_config.provisional,
            reembeddable: embedding_config.reembeddable,
            index_lists,
        },
    )
    .map_err(storage_error_object)?;

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest embed-zone-units",
        "index_dir": index_dir.display().to_string(),
        "embedding_fingerprint": rebuild.embedding_fingerprint,
        "zone_units": rebuild.zone_units,
        "embeddings": rebuild.embeddings,
        "zone_units_considered": embedding_run.chunks_considered,
        "embeddings_inserted": embedding_run.embeddings_inserted,
        "embedding_inputs_truncated": embedding_run.embedding_inputs_truncated,
        "vector_index": {
            "name": rebuild.index_name,
            "index_lists": rebuild.index_lists
        },
        "endpoint_stats": embedding_run.endpoint_stats,
    }))
}

pub(crate) fn embed_chunks_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    // Re-embedding changes embedding coverage; drop the readiness cache up front so the next query
    // recomputes (it is repopulated only when the index is fully ready again).
    invalidate_cached_query_readiness(&postgres).map_err(storage_error_object)?;
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    ensure_embedding_runtime_ready(&embedding_config, false)?;
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let endpoint_configs = embedding_endpoint_pool_configs(
        &embedding_config,
        &loaded_embedding.pool_endpoints,
        &expected_fingerprint,
        embedding_fingerprint.as_str(),
    )?;
    let dimension = i32::try_from(embedding_config.dimension).map_err(|_| {
        dependency_unavailable(format!(
            "embedding dimension {} is too large for dense rebuild metadata",
            embedding_config.dimension
        ))
    })?;
    if dimension != DENSE_VECTOR_DIMENSION {
        return Err(dependency_unavailable(format!(
            "embedding dimension {} does not match storage vector({})",
            embedding_config.dimension, DENSE_VECTOR_DIMENSION
        )));
    }

    // Embedding upserts and dense finalization are separate recoverable steps: re-running the
    // command converges before the manifest/index is advertised.
    let embedding_run = if let Some(limit) = limit {
        // --limit is a bounded smoke path on a small index: load the whole pending set (capped at
        // limit + 1), refuse if it would leave chunks unembedded, then embed it in one pass.
        let inputs = load_chunk_embedding_inputs(
            &postgres,
            embedding_fingerprint.as_str(),
            embedding_config.model.as_str(),
            dimension,
            Some(limit.saturating_add(1)),
        )
        .map_err(storage_error_object)?;
        if inputs.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(ErrorObject::bad_input(
                "ingest embed-chunks --limit would leave chunks unembedded; run on a smaller smoke index or omit --limit to finalize the full dense index",
            ));
        }
        if inputs.is_empty() {
            return Err(no_results("no chunks are available to embed"));
        }
        embed_and_insert_chunks_with_pool(
            &postgres,
            inputs,
            &endpoint_configs,
            embedding_fingerprint.as_str(),
            &embedding_config,
            batch_size,
            pool_concurrency,
        )?
    } else {
        // Production path: stream pending chunks in bounded pages so peak memory is one page, not
        // the full ~1.85M-chunk set (each input can hold up to ~6k chars of contextualized text).
        // Each batch's embeddings are inserted as it completes, so an embedded chunk leaves the
        // pending set and the next page query returns the next slice; a failed page aborts and is
        // recoverable (re-running converges). Embedding generation (the HTTP round-trips) dominates
        // runtime, so the repeated bounded page queries are negligible.
        let mut run = EmbeddingPoolRun {
            chunks_considered: 0,
            embeddings_inserted: 0,
            embedding_inputs_truncated: 0,
            endpoint_stats: Vec::new(),
        };
        loop {
            let page = load_chunk_embedding_inputs(
                &postgres,
                embedding_fingerprint.as_str(),
                embedding_config.model.as_str(),
                dimension,
                Some(EMBED_STREAM_PAGE_SIZE),
            )
            .map_err(storage_error_object)?;
            if page.is_empty() {
                break;
            }
            let page_run = embed_and_insert_chunks_with_pool(
                &postgres,
                page,
                &endpoint_configs,
                embedding_fingerprint.as_str(),
                &embedding_config,
                batch_size,
                pool_concurrency,
            )?;
            run.chunks_considered += page_run.chunks_considered;
            run.embeddings_inserted += page_run.embeddings_inserted;
            run.embedding_inputs_truncated += page_run.embedding_inputs_truncated;
            merge_embedding_endpoint_stats(&mut run.endpoint_stats, page_run.endpoint_stats);
        }
        if run.chunks_considered == 0 {
            return Err(no_results("no chunks are available to embed"));
        }
        run
    };
    let rebuild = finalize_dense_rebuild(
        &postgres,
        &DenseRebuildSpec {
            embedding_fingerprint: embedding_fingerprint.as_str(),
            model: embedding_config.model.as_str(),
            dimension,
            normalize: embedding_config.normalize,
            provisional: embedding_config.provisional,
            reembeddable: embedding_config.reembeddable,
            index_lists,
        },
    )
    .map_err(storage_error_object)?;
    let replay_snapshot = maybe_refresh_replay_snapshot(&postgres)?;

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest embed-chunks",
        "index_dir": index_dir,
        "limit": limit,
        "chunks_considered": embedding_run.chunks_considered,
        "embeddings_inserted": embedding_run.embeddings_inserted,
        "embedding_inputs_truncated": embedding_run.embedding_inputs_truncated,
        "embedding": {
            "model": embedding_config.model,
            "dimension": embedding_config.dimension,
            "normalize": embedding_config.normalize,
            "pooling": embedding_config.pooling,
            "base_urls": embedding_config.base_urls.clone(),
            "pool": embedding_pool_endpoints_status_json(&loaded_embedding.pool_endpoints),
            "pool_overrides_base_urls": !loaded_embedding.pool_endpoints.is_empty(),
            "max_input_chars": embedding_config.max_input_chars,
            "max_estimated_tokens": embedding_config.max_estimated_tokens,
            "estimated_chars_per_token": embedding_config.estimated_chars_per_token,
            "token_count_method": embedding_config.configured_token_count_method(),
            "tokenizer_path": embedding_config.tokenizer_path.as_ref().map(|path| path.display().to_string()),
            "fingerprint": embedding_fingerprint,
            "provisional": embedding_config.provisional,
            "reembeddable": embedding_config.reembeddable
        },
        "endpoint_pool": {
            "strategy": "least_outstanding_requests",
            "batch_size": batch_size,
            "pool_concurrency": pool_concurrency,
            "endpoints": embedding_run.endpoint_stats
        },
        "dense_rebuild": {
            "chunks": rebuild.chunks,
            "embeddings": rebuild.embeddings,
            "embedding_fingerprint": rebuild.embedding_fingerprint,
            "index_name": rebuild.index_name,
            "index_lists": rebuild.index_lists
        },
        "replay_snapshot_cache": replay_snapshot_cache_value(replay_snapshot.as_ref())
    }))
}
