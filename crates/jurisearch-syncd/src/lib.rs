//! `jurisearch-syncd` — the consumer-side sync service (plan P3+, design §7, conception §4.2).
//!
//! The "consumer brain": it verifies a package, applies it into the client storage topology
//! (generations/views/cursor from P2), builds indexes inside the new generation, validates the
//! producer's postconditions, and atomically switches the view + advances the cursor. Each
//! sub-responsibility is a module (SRP); P3 ships the **baseline** applier and `corpus status`.

pub mod apply;
mod error;
pub mod planner;
pub mod status;
pub mod trust;

pub use apply::{
    BaselineApplyOutcome, IncrementalApplyOutcome, apply_baseline, apply_incremental,
    apply_rebaseline,
};
pub use error::SyncError;
pub use planner::{
    CatchupPlan, CatchupReport, CatchupSource, ClientCursor, DirectoryCatchupSource,
    apply_media_auto, check_manifest_corpus, plan_catchup, read_client_cursor, run_catchup,
};
pub use status::{CorpusStatus, corpus_status};
pub use trust::{
    check_entitlement, install_trust_anchor, install_verified_license_token, load_package_verifier,
};
