//! The operated-producer cycle seam (plan P9): one ingest→build→sign→publish→refresh-manifest pass,
//! callable by tests/CLI now and by a cron/daemon later (the CADENCE is the ops boundary).
//!
//! Proactive enrichment ORCHESTRATION (running the existing `enrich-zones` / `build-zone-units` /
//! `embed-zone-units` steps before packaging) is the CALLER's responsibility — those steps live in the
//! producer CLI, and the enriched `decision_zones` / `zone_units` flow through the outbox into the
//! package automatically. The cycle RECORDS the enrichment outcome the caller supplies so a published
//! manifest never silently claims enrichment that did not run.
//!
//! ## Exactly-once / no-partial publish (the acceptance gate)
//!
//! A built incremental is materialised into the per-corpus STAGING slot
//! ([`staged_pending_dir`]) BEFORE its catalog row exists, so the durable artifact and the `"built"`
//! catalog row are both on disk before any publish is attempted. Publish then copies the staged artifact
//! to the served `packages/<id>` (atomic, idempotent) and only THEN marks the catalog row `"published"`.
//! If a cycle dies anywhere between cataloging and a visible artifact, the next [`producer_cycle`] RESUMES
//! the staged artifact — re-publishing the SAME `package_id` (never building a new one) and marking it
//! published — so the manifest only ever advances over an artifact that exists at the served root.

use std::path::Path;
use std::path::PathBuf;

use jurisearch_package::Signer;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;

use jurisearch_storage::backend::DbClientSource;
use jurisearch_storage::package_catalog::catalog_rows_for_corpus;
use jurisearch_storage::runtime::StorageError;

use crate::error::BuildError;
use crate::incremental::{IncrementalParams, build_incremental};
use crate::publish::{publish_package, publish_remote_manifest, staged_pending_dir};
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
    /// The incremental package id PUBLISHED this cycle (a freshly built one OR a resumed staged one), or
    /// `None` if the outbox window was empty and nothing needed resuming (a no-op).
    pub built_incremental: Option<String>,
    /// The published package-sequence head AFTER this cycle (the newest cataloged+published package), if
    /// any catalog row exists. Feeds the producer's `PackageHighWaterMark` checkpoint.
    pub head_sequence: Option<u64>,
    /// The frozen outbox `change_seq` window-high of the published head, if any. Unchanged across an
    /// empty cycle (it reflects the latest published incremental's window).
    pub included_change_seq_high: Option<u64>,
    pub remote_manifest_path: PathBuf,
    pub enrichment: EnrichmentMode,
}

/// A TEST-ONLY fault seam: where (if anywhere) to inject a simulated publish-phase failure, so a gated
/// test can prove the exactly-once / resumable publish contract without making the filesystem unwritable.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFault {
    /// Production: no injected fault.
    None,
    /// Fail AFTER a fresh incremental is staged + cataloged (`"built"`) but BEFORE it is published —
    /// reproducing the pre-publish crash window the acceptance gate forbids.
    AfterStageBeforePublish,
}

/// Run one producer cycle: resume any staged-but-unpublished incremental, build the next incremental from
/// the outbox (if any changes), publish it, then rebuild + publish the corpus's signed remote manifest
/// from the published artifacts. The active baseline must already be published (the operator publishes it
/// once via `package publish`). Built incrementals are staged under the served root's `.staging/pending`
/// slot, so a crashed publish is recoverable on the next pass.
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure.
pub fn producer_cycle(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    config: &ProducerCycleConfig,
) -> Result<ProducerCycleReport, BuildError> {
    producer_cycle_faulted(
        producer,
        corpus,
        published_root,
        signer,
        config,
        PublishFault::None,
    )
}

/// [`producer_cycle`] with a test fault seam. Hidden from docs; production callers use [`producer_cycle`].
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure (or the injected fault).
#[doc(hidden)]
pub fn producer_cycle_faulted(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    config: &ProducerCycleConfig,
    fault: PublishFault,
) -> Result<ProducerCycleReport, BuildError> {
    let pending = staged_pending_dir(published_root, corpus);

    // 0. RESUME: a staged artifact left by a prior crashed cycle is published FIRST (idempotently) and
    //    marked, so the chain head is a published artifact before we consider building a new one.
    let resumed = resume_pending(producer, corpus, published_root, &pending)?;

    // 1. Build the next incremental DIRECTLY into the durable staging slot (None if the window is empty).
    //    Building under the served root means the artifact is on disk before `build_incremental` inserts
    //    the `"built"` catalog row — closing the catalog-advances-before-artifact-exists window.
    clean_dir(&pending)?;
    std::fs::create_dir_all(&pending)?;
    let incremental = build_incremental(
        producer,
        corpus,
        &pending,
        signer,
        &config.incremental_params,
    )?;

    // 2. Publish the freshly built incremental (if any), then mark it published — only AFTER the artifact
    //    is visible at the served root. The manifest (step 3) then advances over an existing artifact.
    let freshly_published = if let Some(report) = &incremental {
        if fault == PublishFault::AfterStageBeforePublish {
            // Reproduce a crash AFTER the catalog row + staged artifact, BEFORE publish.
            return Err(BuildError::Storage(StorageError::Generations {
                message: "injected publish fault (test seam): after stage, before publish"
                    .to_owned(),
            }));
        }
        publish_package(published_root, corpus, &report.package_id, &pending)?;
        mark_package_published(producer, corpus, &report.package_id)?;
        clean_dir(&pending)?; // staged artifact is now durably published
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

    // 4. Report the published head coordinates (for the producer high-water-mark checkpoint).
    let (head_sequence, included_change_seq_high) = published_head(producer, corpus)?;

    Ok(ProducerCycleReport {
        corpus: corpus.to_owned(),
        built_incremental: freshly_published.or(resumed),
        head_sequence,
        included_change_seq_high,
        remote_manifest_path,
        enrichment: config.enrichment.clone(),
    })
}

/// Resume a staged-but-unpublished incremental left in `pending` by a prior crashed cycle: publish its
/// SAME `package_id` (idempotent — a no-op if it was already published) and mark the catalog row
/// published, then clear the slot. Returns the resumed `package_id`, or `None` when nothing was staged.
fn resume_pending(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    pending: &Path,
) -> Result<Option<String>, BuildError> {
    let manifest_path = jurisearch_package::artifact::manifest_path(pending);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&manifest_path)?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes)?;
    let package_id = signed.payload.identity.package_id.clone();
    // `publish_package` is idempotent: it converges a staged artifact and no-ops an already-published one.
    publish_package(published_root, corpus, &package_id, pending)?;
    mark_package_published(producer, corpus, &package_id)?;
    clean_dir(pending)?;
    Ok(Some(package_id))
}

/// Mark a catalog row `"published"` (idempotent). Selection that must observe publish VISIBILITY can then
/// rely on the status; a re-mark of an already-published row is a no-op.
fn mark_package_published(
    producer: &impl DbClientSource,
    corpus: &str,
    package_id: &str,
) -> Result<(), BuildError> {
    let mut db = producer.client()?;
    db.execute(
        "UPDATE package_catalog SET status = 'published' WHERE corpus = $1 AND package_id = $2;",
        &[&corpus, &package_id],
    )?;
    Ok(())
}

/// The newest cataloged package's `(package_sequence, included_change_seq_high)` for `corpus`, or
/// `(None, None)` when the corpus has no catalog rows yet.
fn published_head(
    producer: &impl DbClientSource,
    corpus: &str,
) -> Result<(Option<u64>, Option<u64>), BuildError> {
    let mut db = producer.client()?;
    let rows = catalog_rows_for_corpus(&mut db, corpus)?;
    Ok(rows.last().map_or((None, None), |row| {
        (
            Some(u64::try_from(row.package_sequence).unwrap_or(0)),
            Some(u64::try_from(row.included_change_seq_high).unwrap_or(0)),
        )
    }))
}

/// Remove a directory and everything under it (a no-op if it does not exist).
fn clean_dir(dir: &Path) -> Result<(), BuildError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BuildError::Io(error)),
    }
}
