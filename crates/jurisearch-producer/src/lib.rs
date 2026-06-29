//! `jurisearch-producer` — the update-server / package-origin orchestrator (work/10 milestone M2-B).
//!
//! Drives, IN-PROCESS, the core producer data path against an EXTERNAL PostgreSQL:
//! DILA fetch → ingest → enrich/skip honestly → document embed → `producer_cycle("core")` → signed
//! manifest. It is **library-first** (resolved decision #1): it calls the reusable crate APIs
//! ([`jurisearch_fetch`], [`jurisearch_pipeline`], [`jurisearch_package_build`], [`jurisearch_storage`])
//! and NEVER shells out to `jurisearch` / `jurisearch-package`.
//!
//! # The three cursor coordinate systems (kept separate by construction)
//!
//! See [`cursors`]: the DILA [`cursors::FetchCursorCoordinate`] (archive-timestamp space), the
//! [`cursors::IngestJournalCoordinate`] (accepted-archive name/timestamp space), and the
//! [`cursors::PackageHighWaterMark`] (package `change_seq`/sequence space) are distinct newtypes so a
//! function selecting archives can never be handed a package sequence (the BLOCKER-2 trap).
//!
//! # Exactly-once / no-partial publish
//!
//! [`update::run_update`] holds the single `update-core` lock across ingest→enrich→embed→cycle.
//! `producer_cycle` publishes each new incremental BEFORE the manifest references it, and refreshes the
//! signed manifest even when the outbox window is empty (an empty run is a SUCCESSFUL run that exits
//! zero). A failure before the publish phase leaves a resumable [`cursors::RunCheckpoint`] and no
//! manifest pointing at missing payloads.

pub mod alert;
pub mod baseline;
pub mod config;
pub mod cursors;
pub mod error;
pub mod exit;
pub mod fetch;
pub mod freshness;
pub mod lock;
pub mod render;
pub mod retention;
pub mod runrecord;
pub mod status;
pub mod timestamp;
pub mod update;

pub use baseline::{BaselineDecision, RunKind, baseline_decision, group_run_kind};
pub use config::{PRODUCER_CONFIG_EXAMPLE, ProducerConfig};
pub use error::ProducerError;
pub use exit::{exit_code_for, is_success};
pub use fetch::{FetchStepReport, fetch_source, read_fetch_cursor};
pub use freshness::JudilibreAccelerator;
pub use render::{InstallReport, cron_equivalent, install, render_all};
pub use retention::{
    ReclaimCategory, ReclaimItem, RetentionReport, run_retention, scan_reclaimable,
};
pub use runrecord::{RunOutcome, RunRecord};
pub use status::{ProducerStatus, build_status};
pub use update::{
    ForcedRebaselinePlan, UpdateOptions, UpdateReport, classify_cycle, plan_forced_rebaseline,
    run_update,
};

use jurisearch_storage::provision::{ProvisionReport, provision_external_db};

/// Provision (or converge) the external producer PostgreSQL database from `[database]`: create the DB,
/// migrate, install `pgvector`/`pg_search`, and provision the least-privilege producer roles. Idempotent.
pub fn provision_db(config: &ProducerConfig) -> Result<ProvisionReport, ProducerError> {
    let provision_config = config.provision_config()?;
    let report = provision_external_db(&provision_config)?;
    Ok(report)
}
