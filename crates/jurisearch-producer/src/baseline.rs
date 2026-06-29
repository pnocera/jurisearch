//! Automatic `auto-on-new-baseline` detection + the per-source ADOPTION marker (M3 Phase 5).
//!
//! DILA re-issues the `Freemium_<src>_global_*` baseline occasionally (no fixed schedule). Adopting a
//! newer baseline is a REBASELINE — a full re-anchor of the whole `core` corpus — and must NEVER happen
//! by silently mutating a cursor or by applying the deltas that straddle the new baseline as ordinary
//! incrementals. This module makes the boundary EXPLICIT and DETECTABLE without a DB or the network:
//!
//! - The **fetch cursor** (owned by `jurisearch-fetch`) records `baseline_file_name` = the newest
//!   baseline that has been DOWNLOADED + integrity-checked for a source.
//! - The producer keeps a separate, durable **adoption marker** per source under `state_dir`
//!   (`adopted-baseline-<src>.json`): the baseline that has actually been incorporated through a
//!   recorded rebaseline run (or the initial operator baseline).
//!
//! A NEW baseline is therefore "pending" exactly when the fetched baseline is newer than the adopted
//! one. [`baseline_decision`] reports that purely from those two files; [`group_run_kind`] folds the
//! per-source decisions into the run-level routing (`Incremental` vs `Rebaseline`); and
//! [`ensure_incremental_may_proceed`] is the GUARD an ordinary incremental path calls to REFUSE to cross
//! a pending baseline boundary (`needs-rebaseline`). Adoption is recorded by [`AdoptedBaseline::adopt`]
//! only after a rebaseline run publishes.

use std::path::{Path, PathBuf};

use jurisearch_fetch::{ArchiveSource, FetchCursor};
use serde::{Deserialize, Serialize};

use crate::error::ProducerError;
use crate::timestamp::now_rfc3339;

/// The durable, per-source record of which DILA baseline has been ADOPTED (incorporated through a
/// recorded rebaseline run). Distinct from the fetch cursor's "newest baseline downloaded".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptedBaseline {
    pub source: String,
    /// The adopted baseline file name (e.g. `Freemium_legi_global_20250713-140000.tar.gz`), or `None`
    /// before any baseline has been adopted on this host.
    pub baseline_file_name: Option<String>,
    /// RFC3339 timestamp of the adoption (when the rebaseline run published), if any.
    #[serde(default)]
    pub adopted_at: Option<String>,
}

impl AdoptedBaseline {
    #[must_use]
    pub fn path_for(state_dir: &Path, source: ArchiveSource) -> PathBuf {
        state_dir.join(format!("adopted-baseline-{}.json", source.as_str()))
    }

    /// Load the adoption marker for `source`, or an empty (never-adopted) marker if none is persisted.
    pub fn load(state_dir: &Path, source: ArchiveSource) -> Result<Self, ProducerError> {
        let path = Self::path_for(state_dir, source);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| ProducerError::Io {
                path: path.clone(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                source: source.as_str().to_owned(),
                baseline_file_name: None,
                adopted_at: None,
            }),
            Err(source_err) => Err(ProducerError::Io {
                path,
                source: source_err,
            }),
        }
    }

    /// Record adoption of `baseline_file_name` for `source` (atomic write). Called ONLY after a
    /// rebaseline run has published the signed `core` rebaseline package — never speculatively.
    pub fn adopt(
        state_dir: &Path,
        source: ArchiveSource,
        baseline_file_name: &str,
    ) -> Result<(), ProducerError> {
        std::fs::create_dir_all(state_dir).map_err(|source_err| ProducerError::Io {
            path: state_dir.to_path_buf(),
            source: source_err,
        })?;
        let marker = Self {
            source: source.as_str().to_owned(),
            baseline_file_name: Some(baseline_file_name.to_owned()),
            adopted_at: Some(now_rfc3339()),
        };
        let path = Self::path_for(state_dir, source);
        let tmp = path.with_extension("json.part");
        let bytes = serde_json::to_vec_pretty(&marker).expect("adoption marker serializes");
        std::fs::write(&tmp, &bytes).map_err(|source_err| ProducerError::Io {
            path: tmp.clone(),
            source: source_err,
        })?;
        std::fs::rename(&tmp, &path).map_err(|source_err| ProducerError::Io {
            path: path.clone(),
            source: source_err,
        })?;
        Ok(())
    }
}

/// The per-source baseline state, derived purely from the fetch cursor + the adoption marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineDecision {
    /// No baseline has been fetched yet (a brand-new mirror, before the first global baseline lands).
    NoBaselineFetched,
    /// The fetched baseline equals the adopted one — ordinary incremental territory.
    Current { baseline_file_name: String },
    /// A NEWER baseline has been fetched than the adopted one — a recorded rebaseline is required before
    /// any further incremental may advance the chain. Crossing this boundary as a delta is forbidden.
    RebaselinePending {
        fetched: String,
        adopted: Option<String>,
    },
}

/// Classify one source's baseline state from the on-disk fetch cursor + adoption marker. No DB, no
/// network. The cursor's `baseline_file_name` is the newest baseline DOWNLOADED + integrity-checked.
pub fn baseline_decision(
    state_dir: &Path,
    source: ArchiveSource,
) -> Result<BaselineDecision, ProducerError> {
    let cursor = FetchCursor::load(state_dir, source)?;
    let adopted = AdoptedBaseline::load(state_dir, source)?;
    Ok(decide(
        cursor.baseline_file_name,
        adopted.baseline_file_name,
    ))
}

/// The pure decision function (extracted so it is trivially unit-testable without files).
#[must_use]
pub fn decide(
    fetched_baseline: Option<String>,
    adopted_baseline: Option<String>,
) -> BaselineDecision {
    match fetched_baseline {
        None => BaselineDecision::NoBaselineFetched,
        Some(fetched) => {
            if adopted_baseline.as_deref() == Some(fetched.as_str()) {
                BaselineDecision::Current {
                    baseline_file_name: fetched,
                }
            } else {
                BaselineDecision::RebaselinePending {
                    fetched,
                    adopted: adopted_baseline,
                }
            }
        }
    }
}

/// The run-level routing for a fetch group: `Rebaseline` if ANY source in the group has a pending newer
/// baseline (it must be adopted through a recorded rebaseline run), else `Incremental`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunKind {
    Incremental,
    /// One or more sources have a pending newer baseline; each entry is `(source_token, fetched_baseline)`.
    Rebaseline {
        sources_with_new_baseline: Vec<(String, String)>,
    },
}

/// Fold the per-source [`baseline_decision`]s for a group into the run-level [`RunKind`].
pub fn group_run_kind(
    state_dir: &Path,
    sources: &[ArchiveSource],
) -> Result<RunKind, ProducerError> {
    let mut pending = Vec::new();
    for &source in sources {
        if let BaselineDecision::RebaselinePending { fetched, .. } =
            baseline_decision(state_dir, source)?
        {
            pending.push((source.as_str().to_owned(), fetched));
        }
    }
    if pending.is_empty() {
        Ok(RunKind::Incremental)
    } else {
        Ok(RunKind::Rebaseline {
            sources_with_new_baseline: pending,
        })
    }
}

/// The GUARD an ordinary incremental path calls before advancing the chain: REFUSE
/// (`needs-rebaseline`) if any source has a pending newer baseline, so a delta is never applied across a
/// baseline boundary. (In `auto-on-new-baseline` mode the orchestrator routes to the rebaseline path
/// instead of hitting this; the guard is the hard backstop / the `manual` mode behaviour.)
pub fn ensure_incremental_may_proceed(
    state_dir: &Path,
    sources: &[ArchiveSource],
) -> Result<(), ProducerError> {
    if let RunKind::Rebaseline {
        sources_with_new_baseline,
    } = group_run_kind(state_dir, sources)?
    {
        let (source, fetched) = &sources_with_new_baseline[0];
        return Err(ProducerError::NeedsRebaseline {
            source_token: source.clone(),
            baseline: fetched.clone(),
        });
    }
    Ok(())
}
