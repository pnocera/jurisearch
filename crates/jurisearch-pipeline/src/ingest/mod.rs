//! Ingest seam (S4): DILA LEGI/JURI `.tar.gz` archive ingestion over a [`DbClientSource`].
//!
//! [`ingest_archives`] runs the same per-source archive-run lifecycle the CLI ran
//! (plan → select → read/batch/flush → accounting/quarantine → manifest → run-status → LEGI hierarchy
//! backfill → replay-snapshot refresh), but against any producer DB (managed OR external) and returns a
//! typed [`IngestReport`] instead of a JSON `Value` + `ErrorObject`. The source-family detail lives in
//! the `legi`/`juri` submodules; this root owns the dispatch + the helpers both families share
//! (run-id / quarantine / planned-archive manifest / replay snapshot).

use crate::*;

mod juri;
mod legi;
mod run;

pub(crate) use juri::*;
pub(crate) use legi::*;
pub(crate) use run::*;

/// Which archives in a plan to process. The default (`incremental=false`, no `since`) processes the
/// baseline plus every delta — the full-build behavior. `sync` uses `incremental=true` (a prior full
/// build already ingested the baseline) plus an optional `since_compact` lower bound on delta
/// timestamps so a sync never re-scans the entire baseline corpus.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArchiveSyncFilter<'a> {
    pub incremental: bool,
    pub since_compact: Option<&'a str>,
}

/// Inputs for one [`ingest_archives`] pass. `source` selects the LEGI or JURI run; the caller resolves
/// the archive directory and (for the CLI) the producer DB it passes as `db`.
#[derive(Debug, Clone)]
pub struct IngestArchivesRequest<'a> {
    pub source: ArchiveSource,
    pub archives_dir: &'a Path,
    pub run_id: Option<String>,
    pub limit_members: Option<u32>,
    pub max_member_bytes: u64,
    pub quarantine_dir: Option<&'a Path>,
    pub safe_mode: bool,
    pub filter: ArchiveSyncFilter<'a>,
}

/// What one archive-ingest run produced. The typed fields are the producer's contract surface;
/// `body` carries the full per-source payload the CLI historically emitted (command / schema_version /
/// manifest / coverage / archive plan …) EXCEPT `index_dir`, which the thin CLI consumer injects since
/// it owns the index location.
#[derive(Debug, Clone)]
pub struct IngestReport {
    pub source: ArchiveSource,
    pub run_id: String,
    pub run_status: IngestRunStatus,
    /// Number of planned archives this run actually processed.
    pub archives_ingested: usize,
    /// The journal cursor — the compact timestamp of the latest archive actually processed (the
    /// "archive cursor", distinct from the package cursor). `None` when no archive was processed.
    pub journal_cursor: Option<String>,
    pub visited_members: u64,
    pub inserted_documents: u64,
    pub inserted_chunks: u64,
    pub inserted_publisher_edges: u64,
    pub skipped_members: u64,
    pub failed_members: u64,
    pub quarantined_payloads: u64,
    pub replay_snapshot: Option<ReplaySnapshotReport>,
    pub body: Value,
}

/// Ingest a source's archives (S4). LEGI when `source` is not a jurisprudence dataset; LEGI/JURI
/// share the run lifecycle but keep distinct per-member projection + accounting.
///
/// # Errors
/// [`IngestError`] on an archive read/parse failure, a DB/accounting failure, or a fatal run error.
pub fn ingest_archives(
    db: &impl DbClientSource,
    req: IngestArchivesRequest<'_>,
) -> Result<IngestReport, IngestError> {
    let result = if req.source.is_jurisprudence() {
        ingest_juri_archives(db, req)
    } else {
        ingest_legi_archives(db, req)
    };
    result.map_err(IngestError::from)
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

pub(crate) fn planned_archive_manifest(archive: &PlannedArchive) -> Value {
    json!({
        "source": archive.source,
        "kind": archive.kind,
        "timestamp": archive.timestamp.to_string(),
        "timestamp_compact": archive.timestamp.compact(),
        "file_name": archive.file_name.as_str()
    })
}

/// Monotonic in-process counter making default run IDs unique even within the same nanosecond.
pub(crate) static RUN_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A collision-resistant run-id suffix (nanosecond clock + PID + an in-process counter), unique across
/// rapid same-process and separate-process invocations so two same-second runs never share a run_id.
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

pub(crate) fn default_legi_run_id() -> String {
    format!("legi-{}", unique_run_suffix())
}

/// A producer run id for a non-archive mutation command (embedding, zone derivation/embedding, zone
/// enrichment) — the `package_change_log.ingest_run_id` for every outbox row it emits.
pub(crate) fn producer_run_id(command: &str) -> String {
    format!("{command}-{}", unique_run_suffix())
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

/// Whether maintenance commands should skip the (expensive, full-table MD5) replay-snapshot refresh at
/// their command boundary (`JURISEARCH_SKIP_REPLAY_SNAPSHOT`).
pub(crate) fn replay_snapshot_refresh_skipped() -> bool {
    std::env::var_os("JURISEARCH_SKIP_REPLAY_SNAPSHOT").is_some()
}

/// Refresh the replay snapshot on `client` unless skipped via env. Returns `None` when skipped.
pub(crate) fn maybe_refresh_replay_snapshot(
    client: &mut postgres::Client,
) -> Result<Option<ReplaySnapshotReport>, ErrorObject> {
    if replay_snapshot_refresh_skipped() {
        Ok(None)
    } else {
        Ok(Some(
            refresh_replay_snapshot_with_client(client).map_err(storage_error_object)?,
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
