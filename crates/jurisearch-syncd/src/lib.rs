//! `jurisearch-syncd` — the consumer-side sync service (plan P3+, design §7, conception §4.2).
//!
//! The "consumer brain": it verifies a package, applies it into the client storage topology
//! (generations/views/cursor from P2), builds indexes inside the new generation, validates the
//! producer's postconditions, and atomically switches the view + advances the cursor. Each
//! sub-responsibility is a module (SRP); P3 ships the **baseline** applier and `corpus status`.

pub mod apply;
mod error;
pub mod status;

pub use apply::{BaselineApplyOutcome, apply_baseline};
pub use error::SyncError;
pub use status::{CorpusStatus, corpus_status};
