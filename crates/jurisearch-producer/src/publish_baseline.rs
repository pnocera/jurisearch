//! The `publish-baseline` operator command: publish the EXISTING in-DB `core` corpus (already ingested
//! and 100% embedded in the external producer PostgreSQL) as the producer's FIRST signed `core`
//! baseline, WITHOUT fetching DILA and WITHOUT re-embedding.
//!
//! This is the operator-native wrapper around [`jurisearch_package_build::bootstrap_first_baseline`]
//! (design consultation 20260630-080818, "GO with adjustments"). It runs under the SAME `update-core`
//! lock a timer/manual `update` takes, so the bootstrap can never race a normal run; the package-build
//! helper additionally takes the per-corpus package build advisory lock around the build/publish/catalog
//! span and self-verifies the served root before returning.
//!
//! MINIMAL SLICE: package + catalog + serve + manifest + self-verify only. The producer `state_dir`
//! fetch-cursor / adopted-baseline seeding (which would route the NEXT timer run incremental rather than
//! rebaseline) is a SEPARATE, audited adoption operation and is intentionally NOT done here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::Ed25519Verifier;
use jurisearch_package_build::{
    BaselineParams, BootstrapBaselineConfig, BootstrapBaselineReport, bootstrap_first_baseline,
};

use crate::config::ProducerConfig;
use crate::error::ProducerError;
use crate::lock::acquire_update_lock;
use crate::timestamp::now_rfc3339;

/// The DEFAULT stable, NON-clock-derived baseline id. Stable so a retry is idempotent (Codex adjustment
/// 6): the embedded-manifest digest / catalog identity must not change between attempts.
pub const DEFAULT_BASELINE_ID: &str = "core-bootstrap-v1";

/// The v1 LOCKED storage embedding fingerprint. The published baseline is unusable by a client whose
/// query embedder uses a different fingerprint, so a misconfigured `[embedding]` is rejected fail-closed
/// (and NEVER the `jurisearch-package` CLI `:cls:` default — Codex adjustment 4).
pub const EXPECTED_STORAGE_FINGERPRINT: &str = "bge-m3:1024:normalize:true";

/// Bounded wait for the `update-core` lock before the command reports `skipped-lock-held`.
const LOCK_WAIT: Duration = Duration::from_secs(900);

/// The result of a successful `publish-baseline`, mapped to a JSON report + exit class by the CLI.
#[derive(Debug, Clone)]
pub struct PublishBaselineReport {
    pub corpus: String,
    pub generation: String,
    pub baseline_id: String,
    pub package_id: String,
    pub sequence: u64,
    pub included_change_seq_high: u64,
    pub total_rows: u64,
    pub artifact_sha256: String,
    pub manifest_path: PathBuf,
    pub head_sequence: u64,
    pub artifacts_verified: usize,
    pub exit_class: &'static str,
}

/// Publish the existing in-DB `core` corpus as the first signed `core` baseline. `baseline_id` defaults
/// to [`DEFAULT_BASELINE_ID`] when `None`.
///
/// # Errors
/// [`ProducerError::ConfigInvalid`] on a misconfigured fingerprint/verifier; [`ProducerError::LockHeld`]
/// if the update lock is contended; [`ProducerError::Unprovisioned`] if the external DB has no schema;
/// [`ProducerError::Build`] on a failed preflight (schema/embedding/fingerprint/existing-baseline) or a
/// build/publish/manifest/verify failure.
pub fn run_publish_baseline(
    config: &ProducerConfig,
    baseline_id: Option<&str>,
) -> Result<PublishBaselineReport, ProducerError> {
    let baseline_id = baseline_id.unwrap_or(DEFAULT_BASELINE_ID).to_owned();

    // Fail closed BEFORE touching the DB or the lock: the storage fingerprint must be the v1 locked
    // format, or the published baseline would be query-incomplete under a client's bge-m3 query embedder.
    let fingerprint = config.storage_embedding_fingerprint();
    if fingerprint != EXPECTED_STORAGE_FINGERPRINT {
        return Err(ProducerError::ConfigInvalid(format!(
            "[embedding] storage fingerprint `{fingerprint}` != expected `{EXPECTED_STORAGE_FINGERPRINT}`; \
             refusing to publish a baseline clients cannot query"
        )));
    }

    // Ensure the served root exists + is writable before taking the lock (surfaces a permission error up
    // front rather than mid-publish).
    let served_root = config.producer.corpora_dir.clone();
    std::fs::create_dir_all(&served_root).map_err(|source| ProducerError::Io {
        path: served_root.clone(),
        source,
    })?;

    // Hold the single core update lock for the whole bootstrap so a timer/manual `update` can't race it.
    let _lock = acquire_update_lock(&config.producer.state_dir, LOCK_WAIT)?;
    let db = config.writer_handle()?;
    crate::update::ensure_provisioned(&db)?;

    let signer = config.signer()?;
    // The PUBLIC verifier a client would use, built from the SAME installed seed's trust anchor — so the
    // self-verification exercises the real signature path, not an accept-all stub.
    let verifier = Ed25519Verifier::from_anchors(&[signer.trust_anchor()]).map_err(|err| {
        ProducerError::ConfigInvalid(format!(
            "cannot build a verifier from the configured signing seed: {err}"
        ))
    })?;

    let cfg = BootstrapBaselineConfig {
        baseline_params: bootstrap_baseline_params(config, &baseline_id),
        remote_manifest_params: crate::update::remote_manifest_params(config, &signer),
    };
    let report: BootstrapBaselineReport = bootstrap_first_baseline(
        &db,
        &config.package.corpus,
        &served_root,
        &signer,
        &verifier,
        &cfg,
    )?;

    Ok(PublishBaselineReport {
        corpus: report.corpus,
        generation: report.generation,
        baseline_id: report.baseline_id,
        package_id: report.package_id,
        sequence: report.sequence,
        included_change_seq_high: report.included_change_seq_high,
        total_rows: report.total_rows,
        artifact_sha256: report.artifact_sha256,
        manifest_path: report.remote_manifest_path,
        head_sequence: report.head_sequence,
        artifacts_verified: report.artifacts_verified,
        // A bootstrap always publishes a package (the full first baseline) — a successful publish.
        exit_class: "published",
    })
}

/// The first-baseline build params, derived from the producer config. `embedding_fingerprint` is the
/// STORAGE fingerprint (provider `request_model` excluded); `baseline_id` is the STABLE operator label.
fn bootstrap_baseline_params(config: &ProducerConfig, baseline_id: &str) -> BaselineParams {
    let embedding = config.embedding_config();
    let mut builder_versions = BTreeMap::new();
    builder_versions.insert(
        "jurisearch-producer".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    BaselineParams {
        baseline_id: baseline_id.to_owned(),
        builder_run_id: format!("publish-baseline-{baseline_id}"),
        created_at: now_rfc3339(),
        embedding_fingerprint: embedding.storage_embedding_fingerprint(),
        embedding_model: embedding.model.clone(),
        embedding_dimension: embedding.dimension as u32,
        embedding_normalize: embedding.normalize,
        builder_versions,
        minimum_client_version: Version::new(0, 1, 0),
    }
}
