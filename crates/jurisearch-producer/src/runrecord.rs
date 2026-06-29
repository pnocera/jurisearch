//! Structured, durable, machine-readable RUN RECORDS (M3 Phase 4).
//!
//! One record per run under `state_dir/runs/<group>/<run_id>.record.json`, plus a `last.json` pointer
//! per group so [`crate::status`] can answer "current / stale / broken" WITHOUT reading logs or scanning
//! the whole runs directory. A record is written when the run STARTS (so an in-flight/crashed run is
//! visible) and rewritten at the END with the outcome — on SUCCESS *and* on FAILURE — so the failing
//! class is always durable. This is distinct from [`crate::cursors::RunCheckpoint`], which is the
//! mid-run RESUME point; the record is the post-hoc OBSERVABILITY artifact.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cursors::{FetchCursorCoordinate, IngestJournalCoordinate, PackageHighWaterMark};
use crate::error::ProducerError;
use crate::timestamp::now_rfc3339;

/// Whether a run drove the ordinary incremental path or the rebaseline (adopt-new-baseline) path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKindTag {
    Incremental,
    Rebaseline,
    DryRun,
}

/// The terminal state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// The run is still in flight (start record written, end record not yet).
    Running,
    /// The run finished and its exit class is a success class.
    Success,
    /// The run finished with a failure class.
    Failure,
}

/// A complete record of one `update` (or `rebaseline`) run. Durable + machine-readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub group: String,
    pub sources: Vec<String>,
    pub kind: RunKindTag,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    pub outcome: RunOutcome,
    /// The stable exit class (see [`crate::exit`]). `running` while in flight.
    pub exit_class: String,
    /// A human-readable error message when `outcome == Failure`.
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub fetch_cursors: Vec<FetchCursorCoordinate>,
    #[serde(default)]
    pub ingest_journals: Vec<IngestJournalCoordinate>,
    #[serde(default)]
    pub package_high_water_mark: Option<PackageHighWaterMark>,
    /// The package id published this run (incremental or rebaseline), or `None` for a no-op/dry-run.
    #[serde(default)]
    pub published_package: Option<String>,
    /// For a rebaseline run, the baseline file name(s) adopted.
    #[serde(default)]
    pub adopted_baselines: Vec<String>,
}

impl RunRecord {
    /// Open a fresh `running` record for a starting run.
    #[must_use]
    pub fn started(group: &str, run_id: &str, sources: &[String], kind: RunKindTag) -> Self {
        Self {
            run_id: run_id.to_owned(),
            group: group.to_owned(),
            sources: sources.to_vec(),
            kind,
            started_at: now_rfc3339(),
            ended_at: None,
            outcome: RunOutcome::Running,
            exit_class: "running".to_owned(),
            error: None,
            fetch_cursors: Vec::new(),
            ingest_journals: Vec::new(),
            package_high_water_mark: None,
            published_package: None,
            adopted_baselines: Vec::new(),
        }
    }

    /// Stamp the terminal outcome (success or failure) from the exit class.
    pub fn finish(&mut self, exit_class: &str, error: Option<String>) {
        self.ended_at = Some(now_rfc3339());
        self.exit_class = exit_class.to_owned();
        self.outcome = if crate::exit::is_success(exit_class) {
            RunOutcome::Success
        } else {
            RunOutcome::Failure
        };
        self.error = error;
    }

    /// The on-disk record path for a run.
    #[must_use]
    pub fn path(state_dir: &Path, group: &str, run_id: &str) -> PathBuf {
        runs_group_dir(state_dir, group).join(format!("{run_id}.record.json"))
    }

    /// The `last.json` pointer path for a group (the newest record, for cheap status reads).
    #[must_use]
    pub fn last_pointer_path(state_dir: &Path, group: &str) -> PathBuf {
        runs_group_dir(state_dir, group).join("last.json")
    }

    /// Atomically persist this record AND refresh the group's `last.json` pointer.
    pub fn save(&self, state_dir: &Path) -> Result<(), ProducerError> {
        let dir = runs_group_dir(state_dir, &self.group);
        std::fs::create_dir_all(&dir).map_err(|source| ProducerError::Io {
            path: dir.clone(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(self).expect("run record serializes");
        write_atomic(&Self::path(state_dir, &self.group, &self.run_id), &bytes)?;
        write_atomic(&Self::last_pointer_path(state_dir, &self.group), &bytes)?;
        Ok(())
    }

    /// Load a specific run record.
    pub fn load(
        state_dir: &Path,
        group: &str,
        run_id: &str,
    ) -> Result<Option<Self>, ProducerError> {
        read_record(&Self::path(state_dir, group, run_id))
    }

    /// Load the newest record for a group via its `last.json` pointer (or `None` if no run yet).
    pub fn load_last(state_dir: &Path, group: &str) -> Result<Option<Self>, ProducerError> {
        read_record(&Self::last_pointer_path(state_dir, group))
    }
}

fn runs_group_dir(state_dir: &Path, group: &str) -> PathBuf {
    state_dir.join("runs").join(group)
}

fn read_record(path: &Path) -> Result<Option<RunRecord>, ProducerError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let record = serde_json::from_slice(&bytes).map_err(|err| ProducerError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
            })?;
            Ok(Some(record))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ProducerError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ProducerError> {
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, bytes).map_err(|source| ProducerError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| ProducerError::Io {
        path: path.to_path_buf(),
        source,
    })
}
