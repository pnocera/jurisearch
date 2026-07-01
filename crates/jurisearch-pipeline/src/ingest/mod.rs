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
    /// Policy: whether this pass should refresh the (full-corpus, expensive) replay snapshot after a
    /// completed run. Producer sets it `false` on delta-only cycles (leaving the last full snapshot in
    /// `index_manifest` untouched); all other callers pass `true` to preserve prior behavior. It
    /// COMPOSES with `JURISEARCH_SKIP_REPLAY_SNAPSHOT` (env still forces skip when set).
    pub refresh_replay_snapshot: bool,
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

/// Refresh the replay snapshot on `client` unless the caller's `refresh_replay_snapshot` policy is
/// `false` OR the env skip (`JURISEARCH_SKIP_REPLAY_SNAPSHOT`) is set (the two compose as an AND-guard:
/// either condition skips). Returns `None` when skipped, which leaves the stored
/// `index_manifest['replay_snapshot']` row untouched (only `store_replay_snapshot`, reached via a real
/// refresh, ever writes it).
pub(crate) fn maybe_refresh_replay_snapshot(
    client: &mut postgres::Client,
    refresh_replay_snapshot: bool,
) -> Result<Option<ReplaySnapshotReport>, ErrorObject> {
    if replay_snapshot_refresh_effective(refresh_replay_snapshot) {
        Ok(Some(
            refresh_replay_snapshot_with_client(client).map_err(storage_error_object)?,
        ))
    } else {
        Ok(None)
    }
}

/// Whether a real refresh should run given the caller's `refresh_replay_snapshot` policy: the policy
/// AND the absence of the `JURISEARCH_SKIP_REPLAY_SNAPSHOT` env skip. Both must allow it — the env skip
/// is an ADDITIONAL AND-guard that forces a skip even when the policy is `true`. Pure (env-only) so the
/// composition is unit-testable without a DB.
pub(crate) fn replay_snapshot_refresh_effective(refresh_replay_snapshot: bool) -> bool {
    refresh_replay_snapshot && !replay_snapshot_refresh_skipped()
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

#[cfg(test)]
mod replay_policy_tests {
    use super::{replay_snapshot_cache_value, replay_snapshot_refresh_effective};

    /// The refresh policy composes with the env skip as an AND-guard, and is fully covered without a DB:
    /// a real refresh runs iff the caller policy is `true` AND `JURISEARCH_SKIP_REPLAY_SNAPSHOT` is unset.
    /// A single test owns the process-global env var (set then restore) so it never races a sibling.
    #[test]
    fn refresh_policy_composes_with_env_skip() {
        let restore = std::env::var_os("JURISEARCH_SKIP_REPLAY_SNAPSHOT");
        // SAFETY: this is the only test that reads/writes this env var; it is restored before returning.
        unsafe { std::env::remove_var("JURISEARCH_SKIP_REPLAY_SNAPSHOT") };

        // No env skip: policy is the decider.
        assert!(
            replay_snapshot_refresh_effective(true),
            "policy true + no env skip -> refresh"
        );
        assert!(
            !replay_snapshot_refresh_effective(false),
            "policy false (delta-only cycle) -> skip even with no env skip"
        );

        // Env skip set: BOTH policies skip (additional AND-guard).
        // SAFETY: same single-owner justification as above.
        unsafe { std::env::set_var("JURISEARCH_SKIP_REPLAY_SNAPSHOT", "1") };
        assert!(
            !replay_snapshot_refresh_effective(true),
            "env skip forces skip even when policy is true"
        );
        assert!(
            !replay_snapshot_refresh_effective(false),
            "env skip + policy false -> skip"
        );

        // Restore the environment for any later-running code in this process.
        // SAFETY: restoring the original value; single owner.
        unsafe {
            match restore {
                Some(value) => std::env::set_var("JURISEARCH_SKIP_REPLAY_SNAPSHOT", value),
                None => std::env::remove_var("JURISEARCH_SKIP_REPLAY_SNAPSHOT"),
            }
        }
    }

    /// A skipped refresh surfaces `{"source":"skipped"}` (the report shape callers emit when the policy
    /// or the env skip suppressed the refresh) — distinct from a refreshed `{"source":"refreshed",...}`.
    #[test]
    fn skipped_refresh_reports_source_skipped() {
        let value = replay_snapshot_cache_value(None);
        assert_eq!(value, serde_json::json!({ "source": "skipped" }));
    }
}

#[cfg(test)]
mod select_tests {
    use jurisearch_ingest::archive::{ArchivePlan, ArchiveSource, ParsedArchive, PlannedArchive};

    use super::{ArchiveSyncFilter, select_archives_to_process};

    fn planned(name: &str) -> PlannedArchive {
        let parsed = ParsedArchive::parse_file_name(ArchiveSource::Legi, name).expect("parse name");
        PlannedArchive {
            source: parsed.source,
            kind: parsed.kind,
            timestamp: parsed.timestamp,
            path: std::path::PathBuf::from(&parsed.file_name),
            file_name: parsed.file_name,
        }
    }

    // baseline compact 20250713140000; deltas 20250714/15/16 000000.
    fn legi_plan() -> ArchivePlan {
        ArchivePlan {
            source: ArchiveSource::Legi,
            baseline: planned("Freemium_legi_global_20250713-140000.tar.gz"),
            deltas: vec![
                planned("LEGI_20250714-000000.tar.gz"),
                planned("LEGI_20250715-000000.tar.gz"),
                planned("LEGI_20250716-000000.tar.gz"),
            ],
            skipped: Vec::new(),
        }
    }

    fn names(archives: &[&PlannedArchive]) -> Vec<String> {
        archives.iter().map(|a| a.file_name.clone()).collect()
    }

    #[test]
    fn full_scan_selects_baseline_first_then_all_deltas_in_order() {
        let plan = legi_plan();
        let selected = select_archives_to_process(
            &plan,
            ArchiveSyncFilter {
                incremental: false,
                since_compact: None,
            },
        );
        assert_eq!(
            names(&selected),
            vec![
                "Freemium_legi_global_20250713-140000.tar.gz".to_owned(),
                "LEGI_20250714-000000.tar.gz".to_owned(),
                "LEGI_20250715-000000.tar.gz".to_owned(),
                "LEGI_20250716-000000.tar.gz".to_owned(),
            ]
        );
    }

    #[test]
    fn incremental_without_since_skips_baseline_and_takes_all_deltas() {
        let plan = legi_plan();
        let selected = select_archives_to_process(
            &plan,
            ArchiveSyncFilter {
                incremental: true,
                since_compact: None,
            },
        );
        assert_eq!(
            names(&selected),
            vec![
                "LEGI_20250714-000000.tar.gz".to_owned(),
                "LEGI_20250715-000000.tar.gz".to_owned(),
                "LEGI_20250716-000000.tar.gz".to_owned(),
            ],
            "incremental never opens the baseline tar"
        );
    }

    #[test]
    fn incremental_since_is_inclusive_and_drops_earlier_deltas() {
        let plan = legi_plan();
        // Cursor == the middle delta's compact: baseline absent, the == boundary INCLUDED, the earlier
        // (< cursor) delta EXCLUDED, later deltas kept.
        let selected = select_archives_to_process(
            &plan,
            ArchiveSyncFilter {
                incremental: true,
                since_compact: Some("20250715000000"),
            },
        );
        assert_eq!(
            names(&selected),
            vec![
                "LEGI_20250715-000000.tar.gz".to_owned(),
                "LEGI_20250716-000000.tar.gz".to_owned(),
            ],
            ">= cursor is inclusive (re-reads the cursor archive); < cursor is dropped"
        );
    }
}
