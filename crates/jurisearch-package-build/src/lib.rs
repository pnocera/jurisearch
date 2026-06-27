//! `jurisearch-package-build` — the producer-side package builder (plan P3+, workstream P3).
//!
//! Materialises distributable artifacts from the producer's authoritative tables + outbox, and owns
//! the producer package catalog (the per-corpus `package_sequence` ↔ frozen `change_seq`-window
//! bridge). P3 ships the **baseline** builder; incrementals/rebaselines extend it in P4/P5. Changes
//! only when the package format changes (conception §4 SRP).

pub mod baseline;
pub mod cycle;
mod error;
pub mod incremental;
pub mod publish;
pub mod remote_manifest;
pub mod verify;

pub use baseline::{
    BaselineBuildReport, BaselineParams, RebaselineBuildReport, build_baseline, build_rebaseline,
};
pub use cycle::{EnrichmentMode, ProducerCycleConfig, ProducerCycleReport, producer_cycle};
pub use error::BuildError;
pub use incremental::{IncrementalBuildReport, IncrementalParams, build_incremental};
pub use publish::{publish_package, publish_remote_manifest, published_manifest_path};
pub use remote_manifest::{RemoteManifestParams, build_remote_manifest};
pub use verify::{PublishedVerifyReport, verify_published_root};
