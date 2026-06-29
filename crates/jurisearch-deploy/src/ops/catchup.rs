//! `site catch-up` (plan `01` Phase 5): wrap syncd's verified `plan_catchup`/`run_catchup` loop and
//! decide GREEN honestly.
//!
//! The acceptance gate: catch-up is NEVER green with no active corpus, nor with the local cursor behind
//! the verified producer head. The green decision is a pure function ([`classify_catchup`]) over the
//! verified manifest head + the local cursor, unit-tested without a live DB. The wrapper itself reuses
//! the syncd primitives verbatim (`load_package_verifier`, `DirectoryCatchupSource`,
//! `fetch_verify_manifest`, `read_client_cursor`, `plan_catchup`, `run_catchup`) — the apply-time
//! entitlement/signature/digest gates inside `run_catchup` remain the authoritative trust boundary.

use jurisearch_storage::backend::WriterConnection;
use jurisearch_syncd::{
    DirectoryCatchupSource, fetch_verify_manifest, load_package_verifier, plan_catchup,
    read_client_cursor, run_catchup,
};

use crate::config::SiteConfig;
use crate::error::DeployError;

/// The default `artifact_uri` base the producer publishes with (matches the manifest URIs); mirrors the
/// `jurisearch-syncd` CLI default.
pub const DEFAULT_URI_BASE: &str = "media://";

/// The local-vs-producer position of one corpus, the only facts the green decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatchupState {
    /// The verified producer head sequence from the signed remote manifest.
    pub head_sequence: u64,
    /// The local cursor sequence, or `None` when no corpus is installed/active.
    pub cursor_sequence: Option<u64>,
}

/// The honest green decision for one corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatchupGreen {
    /// The local cursor equals the verified producer head — caught up.
    Green,
    /// No corpus is installed/active — catch-up is NOT green.
    NoActiveCorpus,
    /// The local cursor is not at the head (behind, or ahead = wrong feed) — NOT green.
    NotAtHead { cursor: u64, head: u64 },
}

/// PURE: a corpus is green ONLY when an active cursor exists AND it equals the verified producer head.
/// No active corpus, or any non-equal cursor (behind/ahead), is not green.
#[must_use]
pub fn classify_catchup(state: CatchupState) -> CatchupGreen {
    match state.cursor_sequence {
        None => CatchupGreen::NoActiveCorpus,
        Some(cursor) if cursor == state.head_sequence => CatchupGreen::Green,
        Some(cursor) => CatchupGreen::NotAtHead {
            cursor,
            head: state.head_sequence,
        },
    }
}

impl CatchupGreen {
    #[must_use]
    pub fn is_green(self) -> bool {
        matches!(self, CatchupGreen::Green)
    }
}

/// The result of one corpus catch-up attempt.
#[derive(Debug, Clone)]
pub struct CorpusCatchupResult {
    pub corpus: String,
    pub state: CatchupState,
    pub green: CatchupGreen,
}

/// Run a SINGLE catch-up pass for one corpus (fetch+verify manifest, plan, apply), then re-read the
/// cursor and classify. Live: opens the writer connection + reads the filesystem package root.
pub fn catch_up_corpus(
    conn: &dyn WriterConnection,
    config: &SiteConfig,
    corpus: &str,
) -> Result<CorpusCatchupResult, DeployError> {
    let verifier =
        load_package_verifier(conn).map_err(|error| sync_err("catchup.verifier", error))?;
    let source = DirectoryCatchupSource::new(&config.sync.source_root, DEFAULT_URI_BASE);
    let manifest = fetch_verify_manifest(&source, &verifier, corpus)
        .map_err(|error| sync_err("catchup.manifest", error))?;
    let head = manifest.head_sequence.get();

    let cursor =
        read_client_cursor(conn, corpus).map_err(|error| sync_err("catchup.cursor", error))?;
    let plan = plan_catchup(&manifest, cursor.as_ref());
    run_catchup(conn, &source, &verifier, plan)
        .map_err(|error| sync_err("catchup.apply", error))?;

    // Re-read the cursor AFTER applying, then classify green honestly against the verified head.
    let after = read_client_cursor(conn, corpus)
        .map_err(|error| sync_err("catchup.cursor", error))?
        .map(|cursor| cursor.sequence);
    let state = CatchupState {
        head_sequence: head,
        cursor_sequence: after,
    };
    Ok(CorpusCatchupResult {
        corpus: corpus.to_owned(),
        state,
        green: classify_catchup(state),
    })
}

fn sync_err(code: &'static str, error: jurisearch_syncd::SyncError) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        error.to_string(),
        "check trust anchors, the package source root, and corpus entitlement",
    );
    DeployError::Validation(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_active_corpus_is_never_green() {
        let state = CatchupState {
            head_sequence: 5,
            cursor_sequence: None,
        };
        assert_eq!(classify_catchup(state), CatchupGreen::NoActiveCorpus);
        assert!(!classify_catchup(state).is_green());
    }

    #[test]
    fn a_cursor_behind_the_verified_head_is_not_green() {
        let state = CatchupState {
            head_sequence: 5,
            cursor_sequence: Some(3),
        };
        assert_eq!(
            classify_catchup(state),
            CatchupGreen::NotAtHead { cursor: 3, head: 5 }
        );
        assert!(!classify_catchup(state).is_green());
    }

    #[test]
    fn a_cursor_at_the_verified_head_is_green() {
        let state = CatchupState {
            head_sequence: 5,
            cursor_sequence: Some(5),
        };
        assert_eq!(classify_catchup(state), CatchupGreen::Green);
        assert!(classify_catchup(state).is_green());
    }

    #[test]
    fn a_cursor_ahead_of_the_head_is_not_green_either() {
        // Ahead = wrong feed/environment; never green.
        let state = CatchupState {
            head_sequence: 5,
            cursor_sequence: Some(9),
        };
        assert!(!classify_catchup(state).is_green());
    }
}
