//! `jurisearch-producer status --json` (M3 Phase 4): the one read that tells an operator whether the
//! corpus is CURRENT, STALE, or BROKEN — without reading any log.
//!
//! It folds, per fetch group: the last run record (outcome + exit class + timing), the per-source fetch
//! cursor position and baseline decision (is a newer DILA baseline pending adoption?), the package
//! high-water mark, plus the served root's last published signed manifest head and the live
//! `update-core` lock state. The top-level [`OverallState`] classifies the whole producer.

use std::path::Path;

use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::published_manifest_path;
use serde::{Deserialize, Serialize};

use crate::baseline::{BaselineDecision, baseline_decision};
use crate::config::ProducerConfig;
use crate::cursors::FetchCursorCoordinate;
use crate::error::ProducerError;
use crate::fetch::read_fetch_cursor;
use crate::lock::is_update_lock_held;
use crate::runrecord::{RunOutcome, RunRecord};
use crate::timestamp::{now_unix, rfc3339_from_unix, unix_from_rfc3339};

/// How many cadences a group's last SUCCESSFUL run may age before `status` flags it `stale` by age. A
/// daily producer whose last good run is older than this (2 days) is stalled even with no pending
/// baseline and an existing manifest, so the operator sees the stuck cursor without reading logs.
const STALE_CADENCE_FACTOR: u64 = 2;

/// The producer's top-level health, derived without reading logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverallState {
    /// A run failed, or no signed manifest has ever been published — operator attention needed.
    Broken,
    /// Healthy chain, but a newer DILA baseline awaits its (automatic) rebaseline adoption, OR a run is
    /// in flight — the corpus is behind upstream / not yet settled.
    Stale,
    /// Last run of every group succeeded and no baseline is pending — the corpus is up to date.
    Current,
}

impl OverallState {
    /// The pure classification an operator reads off `status` (no logs):
    /// - `Broken` if a run failed OR nothing has been published yet;
    /// - else `Stale` if the corpus is not settled (`behind`: a pending baseline, an in-flight run, or a
    ///   group that has never run);
    /// - else `Current`.
    #[must_use]
    pub fn classify(any_failure: bool, has_published_manifest: bool, behind: bool) -> Self {
        if any_failure || !has_published_manifest {
            OverallState::Broken
        } else if behind {
            OverallState::Stale
        } else {
            OverallState::Current
        }
    }
}

/// Per-source baseline state, flattened for the JSON view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceBaselineStatus {
    pub source: String,
    /// `no_baseline_fetched` / `current` / `rebaseline_pending`.
    pub state: String,
    pub fetched_baseline: Option<String>,
    pub adopted_baseline: Option<String>,
}

/// Per-group status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupStatus {
    pub group: String,
    pub sources: Vec<String>,
    /// `none` if the group has never run.
    pub last_run_id: Option<String>,
    pub last_outcome: Option<RunOutcome>,
    pub last_exit_class: Option<String>,
    pub last_ended_at: Option<String>,
    pub last_error: Option<String>,
    pub fetch_cursors: Vec<FetchCursorCoordinate>,
    pub baselines: Vec<SourceBaselineStatus>,
    /// True if any source in the group has a newer DILA baseline pending adoption.
    pub rebaseline_pending: bool,
    /// True when the last run SUCCEEDED but ended longer than its cadence budget ago — the cursor is
    /// stalled by age even though nothing failed and a baseline is not pending.
    #[serde(default)]
    pub stale_by_age: bool,
}

/// The full `status --json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerStatus {
    pub generated_at: String,
    pub corpus: String,
    pub overall: OverallState,
    /// The served root's last published signed manifest head sequence, or `None` if none published yet.
    pub published_head_sequence: Option<u64>,
    pub published_manifest_generated_at: Option<String>,
    pub active_baseline_id: Option<String>,
    /// True while a run currently holds the `update-core` lock.
    pub update_lock_held: bool,
    pub groups: Vec<GroupStatus>,
}

/// Build the producer status from on-disk state only (run records, fetch cursors, adoption markers, the
/// served manifest, the lock). No DB connection, no network.
pub fn build_status(config: &ProducerConfig) -> Result<ProducerStatus, ProducerError> {
    build_status_at(config, now_unix())
}

/// [`build_status`] with an injectable `now` (whole UNIX seconds) so the stale-by-age classification is
/// deterministic in tests. Production callers use [`build_status`].
pub fn build_status_at(
    config: &ProducerConfig,
    now_unix: u64,
) -> Result<ProducerStatus, ProducerError> {
    let state_dir = &config.producer.state_dir;
    let corpus = &config.package.corpus;

    let mut groups = Vec::with_capacity(config.fetch_groups.len());
    let mut any_failure = false;
    let mut any_pending = false;
    let mut any_running = false;
    let mut any_never_ran = false;
    let mut any_stale = false;

    for group in &config.fetch_groups {
        let sources = config.resolve_group(&group.name)?;
        let last = RunRecord::load_last(state_dir, &group.name)?;

        let mut fetch_cursors = Vec::new();
        let mut baselines = Vec::new();
        let mut rebaseline_pending = false;
        for &source in &sources {
            fetch_cursors.push(read_fetch_cursor(config, source)?);
            let decision = baseline_decision(state_dir, source)?;
            let (state, fetched, adopted) = match decision {
                BaselineDecision::NoBaselineFetched => {
                    ("no_baseline_fetched".to_owned(), None, None)
                }
                BaselineDecision::Current { baseline_file_name } => (
                    "current".to_owned(),
                    Some(baseline_file_name.clone()),
                    Some(baseline_file_name),
                ),
                BaselineDecision::RebaselinePending { fetched, adopted } => {
                    rebaseline_pending = true;
                    ("rebaseline_pending".to_owned(), Some(fetched), adopted)
                }
            };
            baselines.push(SourceBaselineStatus {
                source: source.as_str().to_owned(),
                state,
                fetched_baseline: fetched,
                adopted_baseline: adopted,
            });
        }
        if rebaseline_pending {
            any_pending = true;
        }

        match &last {
            None => any_never_ran = true,
            Some(record) => match record.outcome {
                RunOutcome::Failure => any_failure = true,
                RunOutcome::Running => any_running = true,
                RunOutcome::Success => {}
            },
        }

        // Freshness: a group whose last GOOD run is older than its cadence budget is stalled by age.
        let stale_after = group.cadence_secs().saturating_mul(STALE_CADENCE_FACTOR);
        let stale_by_age = is_stale_by_age(last.as_ref(), stale_after, now_unix);
        if stale_by_age {
            any_stale = true;
        }

        groups.push(GroupStatus {
            group: group.name.clone(),
            sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
            last_run_id: last.as_ref().map(|r| r.run_id.clone()),
            last_outcome: last.as_ref().map(|r| r.outcome),
            last_exit_class: last.as_ref().map(|r| r.exit_class.clone()),
            last_ended_at: last.as_ref().and_then(|r| r.ended_at.clone()),
            last_error: last.as_ref().and_then(|r| r.error.clone()),
            fetch_cursors,
            baselines,
            rebaseline_pending,
            stale_by_age,
        });
    }

    // The served root's last published signed manifest (head + active baseline + generated_at).
    let manifest = read_published_manifest(&config.producer.corpora_dir, corpus)?;

    let overall = OverallState::classify(
        any_failure,
        manifest.head_sequence.is_some(),
        any_pending || any_running || any_never_ran || any_stale,
    );

    Ok(ProducerStatus {
        generated_at: rfc3339_from_unix(now_unix),
        corpus: corpus.clone(),
        overall,
        published_head_sequence: manifest.head_sequence,
        published_manifest_generated_at: manifest.generated_at,
        active_baseline_id: manifest.active_baseline_id,
        update_lock_held: is_update_lock_held(state_dir),
        groups,
    })
}

/// Whether a group's last run is stale BY AGE: it SUCCEEDED but ended more than `stale_after_secs` ago
/// (relative to `now_unix`). A failed/running/never-run group is classified by the OTHER `behind`
/// signals, not here, so this catches the gap the review found: a last-successful cursor left stalled for
/// days with no pending baseline and an existing manifest. Pure + injectable-`now` for deterministic tests.
#[must_use]
pub fn is_stale_by_age(last: Option<&RunRecord>, stale_after_secs: u64, now_unix: u64) -> bool {
    let Some(record) = last else {
        return false;
    };
    if record.outcome != RunOutcome::Success {
        return false;
    }
    let Some(ended_at) = record.ended_at.as_deref() else {
        return false;
    };
    let Some(ended_unix) = unix_from_rfc3339(ended_at) else {
        return false;
    };
    now_unix.saturating_sub(ended_unix) > stale_after_secs
}

/// The few fields `status` reads from the served root's signed remote manifest.
#[derive(Debug, Clone, Default)]
struct PublishedManifestSummary {
    head_sequence: Option<u64>,
    generated_at: Option<String>,
    active_baseline_id: Option<String>,
}

/// Read the served root's signed remote manifest. A MISSING manifest is an empty summary (nothing
/// published yet) — NOT an error, so `status` works on a fresh host. A present-but-corrupt manifest IS
/// an error.
fn read_published_manifest(
    corpora_dir: &Path,
    corpus: &str,
) -> Result<PublishedManifestSummary, ProducerError> {
    let path = published_manifest_path(corpora_dir, corpus);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let signed: Signed<RemoteManifest> =
                serde_json::from_slice(&bytes).map_err(|err| ProducerError::Io {
                    path: path.clone(),
                    source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
                })?;
            Ok(PublishedManifestSummary {
                head_sequence: Some(signed.payload.head_sequence.get()),
                generated_at: Some(signed.payload.generated_at.clone()),
                active_baseline_id: Some(signed.payload.active_baseline.baseline_id.clone()),
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(PublishedManifestSummary::default())
        }
        Err(source) => Err(ProducerError::Io { path, source }),
    }
}
