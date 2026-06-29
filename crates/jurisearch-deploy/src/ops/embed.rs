//! `embed doctor` / `embed render-service` / `embed fetch-assets` (plan `01` Phase 6): make the local
//! bge-m3 query-embedding prerequisite testable and (later) installable.
//!
//! WRAPS `jurisearch-embed` (`OpenAiCompatibleClient`, `EmbeddingConfig::storage_embedding_fingerprint`)
//! and reuses the M1-A render for the systemd unit — it never reimplements the embedder. The structural
//! checks + the fingerprint-compatibility decision are pure; the endpoint dimension probe is live.

use std::path::Path;

use jurisearch_embed::{BaseUrlClass, OpenAiCompatibleClient, base_url_class};
use jurisearch_storage::backend::WriterConnection;
use jurisearch_syncd::{CorpusStatus, corpus_status};

use crate::config::SiteConfig;
use crate::error::DeployError;
use crate::render::RenderedFile;

use super::readiness::{embedder_storage_fingerprint, fingerprint_compatible};
use super::{CheckResult, DiagnosticReport};

/// PURE/disk: the embedder structural prerequisites — `llama-server` executable, model + tokenizer files
/// present, and the base_url loopback (the confidentiality boundary). Shared with `site doctor`.
#[must_use]
pub fn structural_checks(config: &SiteConfig) -> Vec<CheckResult> {
    let embedder = &config.embedder;
    let mut results = Vec::new();

    results.push(file_check(
        "embed.llama_server.missing",
        "llama-server",
        &embedder.llama_server,
        true,
    ));
    results.push(file_check(
        "embed.model.missing",
        "bge-m3 model",
        &embedder.model_path,
        false,
    ));
    results.push(file_check(
        "embed.tokenizer.missing",
        "bge-m3 tokenizer",
        &embedder.tokenizer_json,
        false,
    ));

    // LOOPBACK-ONLY (confidentiality). Validation already enforces this; re-assert in the doctor so a
    // hand-edited config that bypassed `site validate` is still caught before the endpoint is trusted.
    if base_url_class(&embedder.base_url) == BaseUrlClass::LocalLoopback {
        results.push(CheckResult::ok(
            "embed.loopback",
            format!(
                "query embedder base_url `{}` is loopback-only",
                embedder.base_url
            ),
        ));
    } else {
        results.push(CheckResult::fail(
            "embed.base_url.not_loopback",
            format!(
                "query embedder base_url `{}` is NOT loopback — customer query text must not leave the host",
                embedder.base_url
            ),
            "point embedder.base_url at 127.0.0.1 / ::1 (the local bge-m3 endpoint)",
        ));
    }
    results
}

fn file_check(code: &'static str, name: &str, path: &Path, require_exec: bool) -> CheckResult {
    let meta = std::fs::metadata(path);
    let ok = match meta {
        Ok(meta) if meta.is_file() => {
            if require_exec {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            } else {
                true
            }
        }
        _ => false,
    };
    if ok {
        CheckResult::ok(
            "embed.file.present",
            format!("{name} present at {}", path.display()),
        )
    } else {
        CheckResult::fail(
            code,
            format!(
                "{name} is missing{} at {}",
                if require_exec {
                    " or not executable"
                } else {
                    ""
                },
                path.display()
            ),
            format!("provide {name} (see `jurisearchctl embed fetch-assets` for model/tokenizer)"),
        )
    }
}

/// PURE: the endpoint returned `actual` dims vs the `expected` configured dimension.
#[must_use]
pub fn classify_dimension(expected: usize, actual: usize) -> CheckResult {
    if expected == actual {
        CheckResult::ok(
            "embed.dimension.ok",
            format!("embedder returned the configured dimension ({expected})"),
        )
    } else {
        CheckResult::fail(
            "embed.dimension.mismatch",
            format!("embedder returned dimension {actual}, expected {expected}"),
            "fix the served model / embedder.dimension so they agree",
        )
    }
}

/// PURE: fingerprint compatibility against the active corpora. Before catch-up (no active corpus) this is
/// "no active corpus to compare" (WARN) — NOT a model-endpoint failure (plan `01` Phase 6).
#[must_use]
pub fn classify_fingerprint(config: &SiteConfig, active: &[CorpusStatus]) -> CheckResult {
    if active.is_empty() {
        return CheckResult::warn(
            "embed.fingerprint.no_active_corpus",
            "no active corpus to compare the embedder fingerprint against",
            "run catch-up first; fingerprint compatibility is checked once a corpus is active",
        );
    }
    for corpus in active {
        if !fingerprint_compatible(config, &corpus.embedding_fingerprint) {
            return CheckResult::fail(
                "embed.fingerprint.mismatch",
                format!(
                    "corpus `{}` fingerprint `{}` != embedder fingerprint `{}`",
                    corpus.corpus,
                    corpus.embedding_fingerprint,
                    embedder_storage_fingerprint(config)
                ),
                "align the local bge-m3 model/dimension/normalize with the corpus the producer published",
            );
        }
    }
    CheckResult::ok(
        "embed.fingerprint.compatible",
        "embedder fingerprint is compatible with every active corpus",
    )
}

/// The full `embed doctor` (plan `01` Phase 6). Structural checks always run; the endpoint dimension
/// probe is live (a configured-but-unreachable endpoint is a `Fail`); the fingerprint check needs a DB
/// and is `Skipped` when `writer` is `None`.
pub fn embed_doctor(
    config: &SiteConfig,
    writer: Option<&dyn WriterConnection>,
) -> DiagnosticReport {
    let mut report = DiagnosticReport::default();
    for check in structural_checks(config) {
        report.push(check);
    }

    // Live endpoint dimension probe.
    report.push(probe_endpoint(config));

    // Fingerprint compatibility (needs the DB).
    match writer {
        Some(conn) => match corpus_status(conn) {
            Ok(active) => report.push(classify_fingerprint(config, &active)),
            Err(error) => report.push(CheckResult::fail(
                "embed.fingerprint.topology_unreadable",
                format!("could not read the active corpus topology: {error}"),
                "check the writer role + jurisearch_control schema",
            )),
        },
        None => report.push(CheckResult::skipped(
            "embed.fingerprint.skipped",
            "fingerprint compatibility needs a reachable database (skipped)",
            "run `embed doctor` after the database is reachable",
        )),
    }
    report
}

/// Live: probe the bge-m3 endpoint for a valid embedding and check its dimension. A configured-but-down
/// endpoint is a `Fail`. Public so `site doctor` can include the SAME endpoint diagnostic as `embed
/// doctor` (without re-running the structural checks).
#[must_use]
pub fn probe_endpoint(config: &SiteConfig) -> CheckResult {
    let embedding_config = config.embedder.to_embedding_config();
    let expected = embedding_config.fingerprint();
    let client = match OpenAiCompatibleClient::new(embedding_config) {
        Ok(client) => client,
        Err(error) => {
            return CheckResult::fail(
                "embed.client.invalid",
                format!("could not build the embedder client: {error}"),
                "check embedder.base_url / tokenizer path",
            );
        }
    };
    match client.embed_query("readiness probe", &expected) {
        Ok(vector) => classify_dimension(config.embedder.dimension, vector.values.len()),
        Err(error) => CheckResult::fail(
            "embed.endpoint.unreachable",
            format!("the bge-m3 endpoint did not return a valid embedding: {error}"),
            "start the bge-m3 endpoint (`systemctl start jurisearch-bge-m3`) and re-run embed doctor",
        ),
    }
}

/// `embed render-service`: reuse the M1-A render and return JUST the bge-m3 unit + env file (plan `01`
/// Phase 6 — render the bge-m3 systemd service).
pub fn render_service(config: &SiteConfig) -> Result<Vec<RenderedFile>, DeployError> {
    let rendered = config.render()?;
    Ok(rendered
        .files()
        .into_iter()
        .filter(|file| file.relative_path.contains("bge-m3"))
        .collect())
}

/// `embed fetch-assets`: deferred in this release. Returns an explicit "not implemented" diagnostic with
/// a clear next action rather than silently doing nothing (plan `01` Phase 6 marks it optional).
pub fn fetch_assets_unimplemented() -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        "embed.fetch_assets.unimplemented",
        "`embed fetch-assets` (signed/checksummed model+tokenizer download) is not implemented in this \
         release",
        "provision embedder.model_path + embedder.tokenizer_json out-of-band, then run `embed doctor`",
    );
    DeployError::Validation(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SITE_CONFIG_EXAMPLE;

    fn example() -> SiteConfig {
        SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, Path::new("site.toml")).unwrap()
    }

    #[test]
    fn structural_checks_flag_missing_assets_but_pass_loopback() {
        let config = example();
        let checks = structural_checks(&config);
        // The example paths do not exist on disk → file checks fail, but loopback passes.
        assert!(
            checks
                .iter()
                .any(|c| c.code == "embed.llama_server.missing")
        );
        assert!(checks.iter().any(|c| c.code == "embed.loopback"));
    }

    #[test]
    fn a_non_loopback_base_url_is_a_distinct_fail() {
        let mut config = example();
        config.embedder.base_url = "http://api.openrouter.ai:443".to_owned();
        let checks = structural_checks(&config);
        assert!(
            checks
                .iter()
                .any(|c| c.code == "embed.base_url.not_loopback"
                    && c.status == super::super::CheckStatus::Fail)
        );
    }

    #[test]
    fn dimension_mismatch_is_a_distinct_fail() {
        assert_eq!(classify_dimension(1024, 1024).code, "embed.dimension.ok");
        let mismatch = classify_dimension(1024, 768);
        assert_eq!(mismatch.code, "embed.dimension.mismatch");
        assert_eq!(mismatch.status, super::super::CheckStatus::Fail);
    }

    #[test]
    fn no_active_corpus_fingerprint_is_warn_not_fail() {
        let config = example();
        let check = classify_fingerprint(&config, &[]);
        assert_eq!(check.code, "embed.fingerprint.no_active_corpus");
        assert_eq!(check.status, super::super::CheckStatus::Warn);
    }

    #[test]
    fn render_service_returns_only_the_bge_m3_unit_and_env() {
        let config = example();
        let files = render_service(&config).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.relative_path.contains("bge-m3")));
        assert!(files.iter().any(|f| f.relative_path.ends_with(".service")));
        assert!(files.iter().any(|f| f.relative_path.ends_with(".env")));
    }

    #[test]
    fn fetch_assets_is_an_explicit_unimplemented_diagnostic() {
        let error = fetch_assets_unimplemented();
        assert!(error.to_string().contains("not implemented"));
    }
}
