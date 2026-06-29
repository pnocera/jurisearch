//! Per-source fetch cursor.
//!
//! The cursor records, per DILA [`ArchiveSource`], which archives have been
//! fully downloaded AND passed integrity. It is the producer's persistent memory
//! of "what is already mirrored", so re-runs only pull genuinely new files.
//!
//! # Coordinate system
//!
//! The cursor lives entirely in DILA archive-name / [`ArchiveTimestamp`] space.
//! It is keyed per archive **file name** with per-archive state (timestamp,
//! kind, sha256, size). It is NOT a package `change_seq` high-water mark and is
//! never derived from one. Selection is by archive timestamp + per-archive
//! presence, exactly as the invariant in the macro plan requires.
//!
//! The cursor advances (`record`) ONLY after an archive's integrity check has
//! passed — see [`crate::engine`].

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use jurisearch_ingest::archive::{ArchiveKind, ArchiveSource, ArchiveTimestamp, ParsedArchive};

use crate::{error::FetchError, integrity::IntegrityReport};

/// One recorded, integrity-passed archive in the cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorEntry {
    /// DILA archive timestamp parsed from the file name.
    pub timestamp: ArchiveTimestamp,
    /// Whether this archive is a baseline or a delta.
    pub kind: ArchiveKind,
    /// `sha256:<hex>` of the accepted on-disk bytes.
    pub sha256: String,
    /// On-disk size in bytes of the accepted file.
    pub size_bytes: u64,
}

/// Persistent per-source fetch cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchCursor {
    /// The source this cursor tracks.
    pub source: ArchiveSource,
    /// File name of the highest baseline that has been fetched + verified, if
    /// any. Used by the rebaseline-detection path (a newer baseline id on the
    /// server than this is a rebaseline candidate).
    #[serde(default)]
    pub baseline_file_name: Option<String>,
    /// Every archive fetched + verified, keyed by file name.
    #[serde(default)]
    pub fetched: BTreeMap<String, CursorEntry>,
}

impl FetchCursor {
    /// A fresh, empty cursor for `source`.
    #[must_use]
    pub fn new(source: ArchiveSource) -> Self {
        FetchCursor {
            source,
            baseline_file_name: None,
            fetched: BTreeMap::new(),
        }
    }

    /// Path of the on-disk cursor file for `source` under `state_dir`.
    #[must_use]
    pub fn path_for(state_dir: &Path, source: ArchiveSource) -> PathBuf {
        state_dir.join(format!("fetch-cursor-{}.json", source.as_str()))
    }

    /// Load the cursor for `source` from `state_dir`, or return a fresh empty
    /// cursor if none has been persisted yet.
    pub fn load(state_dir: &Path, source: ArchiveSource) -> Result<Self, FetchError> {
        let path = Self::path_for(state_dir, source);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let cursor: FetchCursor =
                    serde_json::from_slice(&bytes).map_err(|err| FetchError::Cursor {
                        path: path.clone(),
                        message: format!("malformed cursor json: {err}"),
                    })?;
                if cursor.source != source {
                    return Err(FetchError::Cursor {
                        path,
                        message: format!(
                            "cursor source mismatch: file is `{}`, requested `{}`",
                            cursor.source, source
                        ),
                    });
                }
                Ok(cursor)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::new(source)),
            Err(err) => Err(FetchError::io(path, err)),
        }
    }

    /// Persist the cursor under `state_dir`, creating the directory if needed.
    /// The write is atomic (temp file + rename) so a crash mid-write cannot
    /// corrupt the cursor.
    pub fn save(&self, state_dir: &Path) -> Result<(), FetchError> {
        std::fs::create_dir_all(state_dir).map_err(|err| FetchError::io(state_dir, err))?;
        let path = Self::path_for(state_dir, self.source);
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(|err| FetchError::Cursor {
            path: path.clone(),
            message: format!("failed to serialize cursor: {err}"),
        })?;
        std::fs::write(&tmp, &json).map_err(|err| FetchError::io(&tmp, err))?;
        std::fs::rename(&tmp, &path).map_err(|err| FetchError::io(&path, err))?;
        Ok(())
    }

    /// Whether `file_name` has already been fetched + verified.
    #[must_use]
    pub fn is_fetched(&self, file_name: &str) -> bool {
        self.fetched.contains_key(file_name)
    }

    /// Highest archive timestamp recorded so far, if any.
    #[must_use]
    pub fn highest_timestamp(&self) -> Option<&ArchiveTimestamp> {
        self.fetched.values().map(|entry| &entry.timestamp).max()
    }

    /// Record an archive that has just passed integrity. Advancing the cursor is
    /// the ONLY way this map grows; callers must call it strictly after a
    /// successful [`crate::integrity::verify_targz`].
    pub fn record(&mut self, parsed: &ParsedArchive, report: &IntegrityReport) {
        if parsed.kind == ArchiveKind::Baseline {
            let newer = self
                .baseline_file_name
                .as_ref()
                .and_then(|name| self.fetched.get(name))
                .map(|existing| parsed.timestamp > existing.timestamp)
                .unwrap_or(true);
            if newer {
                self.baseline_file_name = Some(parsed.file_name.clone());
            }
        }
        self.fetched.insert(
            parsed.file_name.clone(),
            CursorEntry {
                timestamp: parsed.timestamp.clone(),
                kind: parsed.kind,
                sha256: report.sha256.clone(),
                size_bytes: report.size_bytes,
            },
        );
    }
}
