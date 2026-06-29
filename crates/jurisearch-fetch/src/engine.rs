//! The fetch engine: list → select (by cursor) → download → integrity →
//! promote-or-quarantine → advance cursor.
//!
//! The engine is generic over the [`DirectoryLister`] / [`ArchiveDownloader`]
//! traits so tests drive it with fixture listings and fixture archive bytes and
//! never touch the network.

use std::path::{Path, PathBuf};

use jurisearch_ingest::archive::ArchiveSource;

use crate::{
    cursor::FetchCursor,
    error::FetchError,
    integrity::{IntegrityReport, verify_targz},
    listing::{RemoteArchive, parse_source_listing},
    remote::{ArchiveDownloader, DirectoryLister},
};

/// Where the engine reads cursors from and writes mirrored / quarantined files.
///
/// All three roots get a per-source sub-directory named by [`ArchiveSource::as_str`]
/// (e.g. `archives/legi/`), so one set of roots serves every source.
#[derive(Debug, Clone, Copy)]
pub struct FetchConfig<'a> {
    /// The DILA source to fetch.
    pub source: ArchiveSource,
    /// Mirror root; accepted archives land in `archives_dir/<source>/`.
    pub archives_dir: &'a Path,
    /// Quarantine root; failed downloads land in `quarantine_dir/<source>/`.
    pub quarantine_dir: &'a Path,
    /// State root holding the per-source cursor JSON.
    pub state_dir: &'a Path,
}

/// What a (dry) run WOULD do: the new archives to fetch and the ones already
/// mirrored.
#[derive(Debug, Clone)]
pub struct FetchPlan {
    /// The source planned.
    pub source: ArchiveSource,
    /// New archives (not yet in the cursor), ordered by archive timestamp.
    pub to_fetch: Vec<RemoteArchive>,
    /// File names present in the cursor and therefore skipped (no-op).
    pub already_fetched: Vec<String>,
    /// Total archives in the listing that parsed for this source.
    pub listing_total: usize,
}

/// A planned download item (re-exported alias for ergonomic call sites).
pub type PlannedDownload = RemoteArchive;

/// An archive that was downloaded and passed integrity, now in the mirror.
#[derive(Debug, Clone)]
pub struct DownloadedArchive {
    /// Archive file name.
    pub file_name: String,
    /// Final on-disk path inside the mirror.
    pub path: PathBuf,
    /// Integrity report (size, sha256, member count).
    pub report: IntegrityReport,
}

/// An archive whose download failed integrity and was quarantined.
#[derive(Debug, Clone)]
pub struct QuarantinedArchive {
    /// Archive file name.
    pub file_name: String,
    /// On-disk path inside the quarantine directory.
    pub path: PathBuf,
    /// Human-readable integrity-failure reason.
    pub reason: String,
}

/// Result of a real fetch run.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    /// The source fetched.
    pub source: ArchiveSource,
    /// Archives downloaded + verified this run.
    pub downloaded: Vec<DownloadedArchive>,
    /// Archives quarantined this run (did NOT advance the cursor).
    pub quarantined: Vec<QuarantinedArchive>,
    /// Archives already present per the cursor (skipped).
    pub already_present: Vec<String>,
    /// The cursor after this run.
    pub cursor: FetchCursor,
}

enum FetchOne {
    Downloaded(DownloadedArchive),
    Quarantined(QuarantinedArchive),
}

/// Drives DILA fetches behind injectable listing + download traits.
pub struct Fetcher<'c, L, D> {
    lister: &'c L,
    downloader: &'c D,
}

impl<'c, L, D> Fetcher<'c, L, D>
where
    L: DirectoryLister,
    D: ArchiveDownloader,
{
    /// Build a fetcher over a directory lister and an archive downloader (often
    /// the same object, e.g. [`crate::remote::UreqDilaClient`]).
    pub fn new(lister: &'c L, downloader: &'c D) -> Self {
        Fetcher { lister, downloader }
    }

    /// Compute what a run WOULD fetch without downloading anything (`--dry-run`).
    ///
    /// Selection is purely by the persisted [`FetchCursor`] (archive name /
    /// timestamp space) — never by any package sequence.
    pub fn plan(&self, cfg: &FetchConfig<'_>) -> Result<FetchPlan, FetchError> {
        let cursor = FetchCursor::load(cfg.state_dir, cfg.source)?;
        let html = self.lister.fetch_index(cfg.source)?;
        let listing = parse_source_listing(cfg.source, &html);
        let listing_total = listing.len();

        let mut to_fetch = Vec::new();
        let mut already_fetched = Vec::new();
        for remote in listing {
            if cursor.is_fetched(remote.file_name()) {
                already_fetched.push(remote.parsed.file_name.clone());
            } else {
                to_fetch.push(remote);
            }
        }

        Ok(FetchPlan {
            source: cfg.source,
            to_fetch,
            already_fetched,
            listing_total,
        })
    }

    /// Execute a fetch: download every new archive, integrity-check it, and
    /// either promote it into the mirror (advancing the cursor) or move it to
    /// quarantine (leaving the cursor untouched). The cursor is persisted before
    /// returning, even if a hard transport error aborts the run, so completed
    /// work is never lost.
    pub fn run(&self, cfg: &FetchConfig<'_>) -> Result<FetchOutcome, FetchError> {
        let plan = self.plan(cfg)?;
        let mut cursor = FetchCursor::load(cfg.state_dir, cfg.source)?;

        let mirror_dir = cfg.archives_dir.join(cfg.source.as_str());
        let quarantine_dir = cfg.quarantine_dir.join(cfg.source.as_str());
        std::fs::create_dir_all(&mirror_dir).map_err(|err| FetchError::io(&mirror_dir, err))?;
        std::fs::create_dir_all(&quarantine_dir)
            .map_err(|err| FetchError::io(&quarantine_dir, err))?;

        let mut downloaded = Vec::new();
        let mut quarantined = Vec::new();
        let mut hard_error = None;

        for remote in &plan.to_fetch {
            match self.fetch_one(
                cfg.source,
                remote,
                &mirror_dir,
                &quarantine_dir,
                &mut cursor,
            ) {
                Ok(FetchOne::Downloaded(item)) => downloaded.push(item),
                Ok(FetchOne::Quarantined(item)) => quarantined.push(item),
                Err(err) => {
                    hard_error = Some(err);
                    break;
                }
            }
        }

        cursor.save(cfg.state_dir)?;

        if let Some(err) = hard_error {
            return Err(err);
        }

        Ok(FetchOutcome {
            source: cfg.source,
            downloaded,
            quarantined,
            already_present: plan.already_fetched,
            cursor,
        })
    }

    fn fetch_one(
        &self,
        source: ArchiveSource,
        remote: &RemoteArchive,
        mirror_dir: &Path,
        quarantine_dir: &Path,
        cursor: &mut FetchCursor,
    ) -> Result<FetchOne, FetchError> {
        let file_name = remote.parsed.file_name.clone();
        // Download to a hidden `.part` sidecar; an interrupted/partial write
        // never masquerades as a complete mirror file.
        let part_path = mirror_dir.join(format!(".{file_name}.part"));
        self.downloader
            .download_to(source, &file_name, &part_path)?;

        match verify_targz(&part_path, None) {
            Ok(report) => {
                let final_path = mirror_dir.join(&file_name);
                std::fs::rename(&part_path, &final_path)
                    .map_err(|err| FetchError::io(&final_path, err))?;
                // Advance the cursor ONLY here — strictly after integrity passes.
                cursor.record(&remote.parsed, &report);
                Ok(FetchOne::Downloaded(DownloadedArchive {
                    file_name,
                    path: final_path,
                    report,
                }))
            }
            Err(integrity_err) => {
                let quarantine_path = quarantine_dir.join(&file_name);
                // Move the bad bytes aside for inspection; do NOT touch the
                // cursor. Best-effort: if the move fails, remove the partial so
                // it cannot be mistaken for a good mirror file.
                if std::fs::rename(&part_path, &quarantine_path).is_err() {
                    let _ = std::fs::remove_file(&part_path);
                }
                Ok(FetchOne::Quarantined(QuarantinedArchive {
                    file_name,
                    path: quarantine_path,
                    reason: integrity_err.to_string(),
                }))
            }
        }
    }
}
