//! The three producer cursor coordinate systems — kept SEPARATE by the type system so they cannot be
//! conflated, plus a resumable run checkpoint that records all three.
//!
//! These live in three different coordinate systems (work/10 `02` Phase 2, "do not conflate the clocks"):
//!
//! 1. [`FetchCursorCoordinate`] — per DILA source, in **archive-timestamp / file-name** space, owned by
//!    `jurisearch-fetch`'s persisted `FetchCursor`. It answers "which remote files have I already
//!    downloaded + integrity-checked?". It is NEVER a package sequence.
//! 2. [`IngestJournalCoordinate`] — per accepted archive **file name / compact timestamp**, owned by the
//!    storage ingest accounting. It answers "which archives have I streamed into canonical storage?".
//!    Archive SELECTION keys on this (the DILA `ArchiveTimestamp`), never on `change_seq`.
//! 3. [`PackageHighWaterMark`] — in **package `change_seq` / sequence** space, owned by
//!    `producer_cycle()` / the package catalog. It answers "what outbox window has been packaged?".
//!
//! Making them distinct newtypes means a function that selects archives cannot accidentally be handed a
//! package `change_seq` (the BLOCKER-2 trap): the types do not unify.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ProducerError;

/// (1) The DILA fetch cursor coordinate: the latest archive name + compact timestamp downloaded for a
/// source. Lives in archive-timestamp space; carries NO package sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchCursorCoordinate {
    pub source: String,
    /// Newest fully-downloaded + integrity-passed archive file name, if any.
    pub latest_file_name: Option<String>,
    /// Its compact `YYYYMMDDHHMMSS` archive timestamp, if any.
    pub latest_compact_timestamp: Option<String>,
}

/// (2) The ingest-journal coordinate: the compact timestamp of the latest archive streamed into
/// canonical storage for a source. Lives in archive-timestamp space (NOT `change_seq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestJournalCoordinate {
    pub source: String,
    pub run_id: Option<String>,
    /// Compact `YYYYMMDDHHMMSS` timestamp of the latest archive actually processed, if any.
    pub journal_compact_timestamp: Option<String>,
    pub archives_ingested: usize,
    /// Whether this source full-scanned this cycle (`!mode.incremental`: rebaseline / pending baseline /
    /// cold-or-stale cursor). Carries the per-source refresh signal to the cycle-level `any_full_scan`
    /// that gates the chunk-embed replay-snapshot refresh.
    #[serde(default)]
    pub full_scan: bool,
}

/// (3) The package high-water mark: the corpus's packaged sequence + the `change_seq` window high it
/// captured. Lives in package-sequence space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageHighWaterMark {
    pub corpus: String,
    /// The newest published package sequence, if a package was built this cycle.
    pub head_sequence: Option<u64>,
    /// The frozen outbox `change_seq` high captured by the built incremental, if any.
    pub included_change_seq_high: Option<u64>,
}

/// The phase a run last completed — the resume point if it crashes before publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    Started,
    Fetched,
    Ingested,
    Enriched,
    Embedded,
    Published,
}

/// A resumable checkpoint persisted after each phase under `state_dir/runs/<group>/<run_id>.json`. It
/// records all three cursor coordinates SEPARATELY so a resumed/failed run never reconstructs one clock
/// from another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCheckpoint {
    pub group: String,
    pub run_id: String,
    pub phase: RunPhase,
    pub fetch_cursors: Vec<FetchCursorCoordinate>,
    pub ingest_journals: Vec<IngestJournalCoordinate>,
    pub package_high_water_mark: Option<PackageHighWaterMark>,
}

impl RunCheckpoint {
    #[must_use]
    pub fn started(group: &str, run_id: &str) -> Self {
        Self {
            group: group.to_owned(),
            run_id: run_id.to_owned(),
            phase: RunPhase::Started,
            fetch_cursors: Vec::new(),
            ingest_journals: Vec::new(),
            package_high_water_mark: None,
        }
    }

    /// The on-disk checkpoint path for a run.
    #[must_use]
    pub fn path(state_dir: &Path, group: &str, run_id: &str) -> PathBuf {
        state_dir
            .join("runs")
            .join(group)
            .join(format!("{run_id}.json"))
    }

    /// Atomically persist the checkpoint (write a sidecar, then rename). A crash never leaves a
    /// half-written checkpoint that points past the work actually done.
    pub fn save(&self, state_dir: &Path) -> Result<(), ProducerError> {
        let path = Self::path(state_dir, &self.group, &self.run_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ProducerError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let tmp = path.with_extension("json.part");
        let bytes = serde_json::to_vec_pretty(self).expect("checkpoint serializes");
        std::fs::write(&tmp, &bytes).map_err(|source| ProducerError::Io {
            path: tmp.clone(),
            source,
        })?;
        std::fs::rename(&tmp, &path).map_err(|source| ProducerError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(())
    }

    /// Load a previously-persisted checkpoint, if any (for resume / status).
    pub fn load(
        state_dir: &Path,
        group: &str,
        run_id: &str,
    ) -> Result<Option<Self>, ProducerError> {
        let path = Self::path(state_dir, group, run_id);
        match std::fs::read(&path) {
            Ok(bytes) => {
                let checkpoint =
                    serde_json::from_slice(&bytes).map_err(|err| ProducerError::Io {
                        path: path.clone(),
                        source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
                    })?;
                Ok(Some(checkpoint))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(ProducerError::Io { path, source }),
        }
    }
}
