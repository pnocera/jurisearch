//! Optional retention tooling (M7, resolved decision #7): reclaim ONLY temporary / partial / quarantined
//! download files. Accepted official archives are RETAINED INDEFINITELY for reproducibility, audit, and
//! rebuilds, and published Storebox packages/manifests are NEVER touched.
//!
//! The scan is an ALLOWLIST, not a denylist: it only ever looks in, and only ever returns paths under,
//! four reclaimable locations, and it NEVER descends into the served `corpora_dir` (Storebox packages +
//! manifest) at all. Within the DILA mirror it matches ONLY the hidden `.part` partial-download sidecars,
//! never an accepted `.tar.gz`. `--dry-run` (the default) reports reclaimable bytes without deleting.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::ProducerConfig;
use crate::error::ProducerError;

/// Which reclaimable class a file belongs to (all safe to delete; none is an accepted archive/package).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReclaimCategory {
    /// A download that failed the integrity gate and was quarantined by `jurisearch-fetch`
    /// (`state_dir/quarantine/<src>/...`). It was never an accepted archive.
    FetchQuarantine,
    /// A member/archive quarantined by the ingest pass (`state_dir/ingest-quarantine/...`).
    IngestQuarantine,
    /// An interrupted partial download sidecar (`archives_dir/<src>/.<name>.part`) — never promoted.
    PartialDownload,
    /// A leftover atomic-write temp (`*.json.part` / `*.json.tmp`) from a crash mid-write of a cursor /
    /// run record / adoption marker. The committed `.json` final is never matched.
    StaleTempWrite,
}

/// One reclaimable file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReclaimItem {
    pub path: PathBuf,
    pub category: ReclaimCategory,
    pub size_bytes: u64,
}

/// The outcome of a retention run (`--dry-run` reports; opt-in `delete` reclaims).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionReport {
    pub dry_run: bool,
    pub items: Vec<ReclaimItem>,
    pub reclaimable_bytes: u64,
    pub reclaimable_files: usize,
    /// Files actually deleted (always `0` on a dry run).
    pub deleted_files: usize,
    pub deleted_bytes: u64,
}

/// True for the hidden `.<name>.part` partial-download sidecars in the DILA mirror. NEVER true for an
/// accepted `.tar.gz` archive — the safety predicate that keeps the mirror's official archives untouched.
#[must_use]
fn is_partial_download(name: &str) -> bool {
    name.ends_with(".part") && !name.ends_with(".tar.gz")
}

/// True for a leftover atomic-write temp (`.json.part` from run records/markers, `.json.tmp` from fetch
/// cursors). The committed `.json` final never matches.
#[must_use]
fn is_stale_temp_write(name: &str) -> bool {
    name.ends_with(".json.part") || name.ends_with(".json.tmp")
}

/// Recursively collect regular files under `dir` whose file name satisfies `accept`. Missing `dir` is an
/// empty result (not an error). Symlinks are skipped (never followed) so the walk cannot escape `dir`.
fn collect_files<F>(dir: &Path, accept: &F, out: &mut Vec<PathBuf>) -> Result<(), ProducerError>
where
    F: Fn(&str) -> bool,
{
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ProducerError::Io {
                path: dir.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(|source| ProducerError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| ProducerError::Io {
            path: entry.path(),
            source,
        })?;
        if file_type.is_symlink() {
            continue; // never follow symlinks out of the reclaimable tree
        }
        let path = entry.path();
        if file_type.is_dir() {
            collect_files(&path, accept, out)?;
        } else if file_type.is_file() {
            let name = entry.file_name();
            if accept(&name.to_string_lossy()) {
                out.push(path);
            }
        }
    }
    Ok(())
}

fn file_size(path: &Path) -> Result<u64, ProducerError> {
    Ok(std::fs::metadata(path)
        .map_err(|source| ProducerError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len())
}

/// Scan the reclaimable locations (allowlist only) and return every reclaimable file. Read-only: it
/// computes sizes but deletes nothing. It NEVER returns a path under `corpora_dir` or an accepted
/// `.tar.gz`.
pub fn scan_reclaimable(config: &ProducerConfig) -> Result<Vec<ReclaimItem>, ProducerError> {
    let state_dir = config.producer.state_dir.as_path();
    let archives_dir = config.producer.archives_dir.as_path();
    let mut items = Vec::new();

    // 1) Fetch quarantine (integrity-rejected downloads) — every file under it.
    let mut paths = Vec::new();
    collect_files(&state_dir.join("quarantine"), &|_| true, &mut paths)?;
    push_items(&paths, ReclaimCategory::FetchQuarantine, &mut items)?;

    // 2) Ingest quarantine — every file under it.
    let mut paths = Vec::new();
    collect_files(&state_dir.join("ingest-quarantine"), &|_| true, &mut paths)?;
    push_items(&paths, ReclaimCategory::IngestQuarantine, &mut items)?;

    // 3) Partial-download sidecars in the DILA mirror (ONLY `.part`, NEVER an accepted `.tar.gz`).
    let mut paths = Vec::new();
    collect_files(archives_dir, &is_partial_download, &mut paths)?;
    push_items(&paths, ReclaimCategory::PartialDownload, &mut items)?;

    // 4) Stale atomic-write temps under the state dir (cursors / records / markers, never their finals).
    let mut paths = Vec::new();
    collect_files(state_dir, &is_stale_temp_write, &mut paths)?;
    // Exclude anything already counted under the quarantine subtrees (defensive; quarantine holds DILA
    // archives, not `.json.*` temps, so this is normally a no-op).
    paths.retain(|p| !is_under(&state_dir.join("quarantine"), p));
    push_items(&paths, ReclaimCategory::StaleTempWrite, &mut items)?;

    Ok(items)
}

fn push_items(
    paths: &[PathBuf],
    category: ReclaimCategory,
    out: &mut Vec<ReclaimItem>,
) -> Result<(), ProducerError> {
    for path in paths {
        out.push(ReclaimItem {
            path: path.clone(),
            category,
            size_bytes: file_size(path)?,
        });
    }
    Ok(())
}

fn is_under(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

/// Defense in depth for the delete leg: a path is safe to delete ONLY if it is NOT under the served
/// `corpora_dir` (Storebox packages/manifest), AND — within the DILA mirror — it is a `.part` partial
/// sidecar rather than an accepted archive. Quarantine files live under the state dir, so a quarantined
/// `.tar.gz` (a rejected download) is correctly deletable while an accepted mirror `.tar.gz` is not.
fn is_safe_to_delete(path: &Path, corpora_dir: &Path, archives_dir: &Path) -> bool {
    if is_under(corpora_dir, path) {
        return false; // never touch a published package or manifest
    }
    if is_under(archives_dir, path) {
        // Inside the mirror, only the `.part` partial sidecars are reclaimable; accepted archives stay.
        return path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_partial_download);
    }
    true
}

/// Run retention. With `delete == false` (the default) it only REPORTS reclaimable files. With
/// `delete == true` it deletes them — but ONLY after a per-file defensive re-check that the path is a
/// reclaimable temp/partial/quarantine file and NOT under the served `corpora_dir` and NOT an accepted
/// `.tar.gz`. Accepted archives and published packages are therefore unreachable by construction.
pub fn run_retention(
    config: &ProducerConfig,
    delete: bool,
) -> Result<RetentionReport, ProducerError> {
    let items = scan_reclaimable(config)?;
    let reclaimable_bytes: u64 = items.iter().map(|i| i.size_bytes).sum();
    let reclaimable_files = items.len();

    let corpora_dir = config.producer.corpora_dir.as_path();
    let archives_dir = config.producer.archives_dir.as_path();
    let mut deleted_files = 0usize;
    let mut deleted_bytes = 0u64;
    if delete {
        for item in &items {
            if !is_safe_to_delete(&item.path, corpora_dir, archives_dir) {
                continue;
            }
            match std::fs::remove_file(&item.path) {
                Ok(()) => {
                    deleted_files += 1;
                    deleted_bytes += item.size_bytes;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(ProducerError::Io {
                        path: item.path.clone(),
                        source,
                    });
                }
            }
        }
    }

    Ok(RetentionReport {
        dry_run: !delete,
        items,
        reclaimable_bytes,
        reclaimable_files,
        deleted_files,
        deleted_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::{is_partial_download, is_stale_temp_write};

    #[test]
    fn partial_predicate_matches_part_sidecars_but_never_accepted_archives() {
        assert!(is_partial_download(
            ".Freemium_legi_global_20250713-140000.tar.gz.part"
        ));
        assert!(is_partial_download(".some-delta.part"));
        // An accepted official archive must NEVER be reclaimable.
        assert!(!is_partial_download(
            "Freemium_legi_global_20250713-140000.tar.gz"
        ));
        assert!(!is_partial_download("core-1-2.jzst"));
    }

    #[test]
    fn stale_temp_predicate_matches_atomic_temps_not_finals() {
        assert!(is_stale_temp_write("last.json.part"));
        assert!(is_stale_temp_write("fetch-cursor-legi.json.tmp"));
        assert!(!is_stale_temp_write("last.json"));
        assert!(!is_stale_temp_write("adopted-baseline-cass.json"));
    }
}
