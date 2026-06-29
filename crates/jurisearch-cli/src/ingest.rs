//! Official-source ingestion orchestration: the `ingest` subcommands and `sync`. Converts
//! CLI flags into parser/storage/enrichment calls and JSON artifacts — the LEGI/JURI archive
//! run lifecycle (plan/select/read/flush/quarantine/accounting/replay-snapshot), embed-chunks,
//! hierarchy backfill, and the zone-unit pipeline (enrich-zones/build/embed). The embedding
//! pool wrappers live in crate::embedding_runtime; the legislation-citation payloads live in
//! crate::enrichment::legislation.
//!
//! Source-family detail lives in submodules (legi/juri archive runs, the embed/zone-unit
//! pipeline); this root keeps the dispatch (`emit_ingest`, `sync`) and the helpers shared by
//! both archive families (run-id/manifest/quarantine/replay-snapshot).

use crate::*;

mod juri;
mod legi;
mod pipeline;

pub(crate) use juri::*;
pub(crate) use legi::*;
pub(crate) use pipeline::*;

// The archive ingest/enrich/embed implementation moved to `jurisearch-pipeline` (work/10 M1-C). The
// CLI keeps the thin dispatch (`emit_ingest`/`sync_payload`) + the maintenance payloads that are not
// named producer seams (build-zone-units, legislation citations, hierarchy backfill), delegating the
// archive runs to the library. Re-export the library's archive filter so `sync`/`emit` build it.
pub(crate) use jurisearch_pipeline::ArchiveSyncFilter;

/// Render a `jurisearch-pipeline` report `body` into the historical CLI payload by injecting the
/// CLI-owned `index_dir` the library does not know about (serde_json sorts object keys, so insertion
/// position does not affect the emitted JSON).
pub(crate) fn with_index_dir(mut body: Value, index_dir: &Path) -> Value {
    if let Value::Object(map) = &mut body {
        map.insert(
            "index_dir".to_owned(),
            json!(index_dir.display().to_string()),
        );
    }
    body
}

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
            // `--index-lists 0` is valid: it means auto-scale the ivfflat lists to the corpus size.
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
            // `--index-lists 0` is valid: it means auto-scale the ivfflat lists to the corpus size.
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
            match embed_zone_units_payload(
                index_dir,
                limit,
                index_lists,
                batch_size,
                pool_concurrency,
            ) {
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
        Some(IngestSubcommand::EnrichLegislationCitations {
            limit,
            retry_errors,
        }) => {
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

// Archive-run default IDs are generated inside `jurisearch-pipeline` now (the CLI passes the operator's
// `--run-id`, or `None`). Kept here only for the run-id-uniqueness unit tests.
#[cfg(test)]
pub(crate) fn default_juri_run_id(source: ArchiveSource) -> String {
    format!("{}-{}", source.as_str(), unique_run_suffix())
}

#[cfg(test)]
pub(crate) fn default_legi_run_id() -> String {
    format!("legi-{}", unique_run_suffix())
}

/// A producer run id for a non-archive mutation command (embedding, zone derivation/embedding, zone
/// enrichment, citation collect/enrich, hierarchy backfill) — the `package_change_log.ingest_run_id`
/// for every outbox row it emits (design §5.1; "the run that produced this mutation").
pub(crate) fn producer_run_id(command: &str) -> String {
    format!("{command}-{}", unique_run_suffix())
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
