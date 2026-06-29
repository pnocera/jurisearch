//! The operated-producer cycle seam (plan P9): one ingest→build→sign→publish→refresh-manifest pass,
//! callable by tests/CLI now and by a cron/daemon later (the CADENCE is the ops boundary).
//!
//! Proactive enrichment ORCHESTRATION (running the existing `enrich-zones` / `build-zone-units` /
//! `embed-zone-units` steps before packaging) is the CALLER's responsibility — those steps live in the
//! producer CLI, and the enriched `decision_zones` / `zone_units` flow through the outbox into the
//! package automatically. The cycle RECORDS the enrichment outcome the caller supplies so a published
//! manifest never silently claims enrichment that did not run.

use std::path::Path;
use std::path::PathBuf;

use jurisearch_package::Signer;

use jurisearch_storage::backend::DbClientSource;

use crate::error::BuildError;
use crate::incremental::{IncrementalParams, build_incremental};
use crate::publish::{publish_package, publish_remote_manifest};
use crate::remote_manifest::{RemoteManifestParams, build_remote_manifest};

/// The enrichment outcome for this cycle (recorded, not run, by the cycle — see the module docs). The
/// CLI decides fail-closed-vs-skip when credentials are absent; the cycle never fabricates a "ran".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichmentMode {
    /// Proactive enrichment is off for this profile.
    Disabled,
    /// Enrichment ran before this cycle and refreshed `zones_enriched` decisions.
    Ran { zones_enriched: u64 },
    /// Enrichment was requested but upstream credentials were absent (a local/test profile).
    SkippedNoCredentials,
}

/// Inputs for one [`producer_cycle`] pass.
#[derive(Debug, Clone)]
pub struct ProducerCycleConfig {
    pub incremental_params: IncrementalParams,
    pub remote_manifest_params: RemoteManifestParams,
    pub enrichment: EnrichmentMode,
}

/// What one cycle produced.
#[derive(Debug, Clone)]
pub struct ProducerCycleReport {
    pub corpus: String,
    /// The incremental package id built this cycle, or `None` if the outbox window was empty (no-op).
    pub built_incremental: Option<String>,
    pub remote_manifest_path: PathBuf,
    pub enrichment: EnrichmentMode,
}

/// Run one producer cycle: build the next incremental from the outbox (if any changes), publish it,
/// then rebuild + publish the corpus's signed remote manifest from the published artifacts. The active
/// baseline must already be published (the operator publishes it once via `package publish`). `build_dir`
/// is a scratch directory for the transient incremental artifact before it is published.
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure.
pub fn producer_cycle(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    build_dir: &Path,
    signer: &dyn Signer,
    config: &ProducerCycleConfig,
) -> Result<ProducerCycleReport, BuildError> {
    // 1. Build the next incremental from the frozen outbox window (None if the window is empty).
    std::fs::create_dir_all(build_dir)?;
    let incremental = build_incremental(
        producer,
        corpus,
        build_dir,
        signer,
        &config.incremental_params,
    )?;

    // 2. Publish the new incremental (if any) before the manifest references it.
    let built_incremental = if let Some(report) = &incremental {
        publish_package(published_root, corpus, &report.package_id, build_dir)?;
        Some(report.package_id.clone())
    } else {
        None
    };

    // 3. Rebuild + publish the signed remote manifest from the published artifacts.
    let signed = build_remote_manifest(
        producer,
        corpus,
        published_root,
        signer,
        &config.remote_manifest_params,
    )?;
    let remote_manifest_path = publish_remote_manifest(published_root, corpus, &signed)?;

    Ok(ProducerCycleReport {
        corpus: corpus.to_owned(),
        built_incremental,
        remote_manifest_path,
        enrichment: config.enrichment.clone(),
    })
}
