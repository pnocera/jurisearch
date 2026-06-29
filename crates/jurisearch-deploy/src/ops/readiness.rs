//! `site readiness` (plan `01` Phase 5): prove the site can answer — an active, readiness-stamped corpus
//! whose embedding fingerprint is compatible with the configured LOOPBACK-ONLY query embedder.
//!
//! Wraps `jurisearch-syncd::corpus_status` (active topology) + `jurisearch-storage`'s writer-owned
//! readiness stamp lookup (`load_query_readiness_with_client`). The distinct readiness CLASSES (no active
//! corpus / never-stamped / stale / malformed) are surfaced from WHERE the storage lookup fails, not by
//! parsing message text — `corpus_status` first tells us whether any corpus is active, so a readiness
//! error WITH an active corpus is a writer/apply stale-stamp fault, and WITHOUT one is the advisory "not
//! yet caught up". The embedder fingerprint compatibility check ([`fingerprint_compatible`]) is pure.

use jurisearch_storage::backend::{ReadHandle, WriterConnection};
use jurisearch_storage::ingest_accounting::load_query_readiness_with_client;
use jurisearch_syncd::{CorpusStatus, corpus_status};

use crate::config::SiteConfig;

use super::{CheckResult, DiagnosticReport};

/// PURE: the configured query embedder is fingerprint-compatible with an active corpus iff the embedder's
/// EFFECTIVE storage fingerprint (model + dimension + normalization + provider/base_url class + pooling,
/// computed by `jurisearch-embed`) equals the corpus's stamped `embedding_fingerprint`. A site whose
/// local bge-m3 differs from the corpus the producer embedded with would return wrong vectors, so this is
/// a deployment failure, not a silent skip.
#[must_use]
pub fn fingerprint_compatible(config: &SiteConfig, corpus_fingerprint: &str) -> bool {
    config
        .embedder
        .to_embedding_config()
        .storage_embedding_fingerprint()
        == corpus_fingerprint
}

/// The configured query embedder's effective storage fingerprint (for diagnostics).
#[must_use]
pub fn embedder_storage_fingerprint(config: &SiteConfig) -> String {
    config
        .embedder
        .to_embedding_config()
        .storage_embedding_fingerprint()
}

/// PURE: classify the readiness outcome into a DISTINCT, stable diagnostic. `active` is the active
/// corpora (from `corpus_status`); `readiness_ok` is whether the writer-owned stamp lookup succeeded.
/// A readiness error WITH active corpora is a stale/never-stamped writer fault (FAIL); WITHOUT active
/// corpora it is the advisory "not yet caught up" (WARN, never green-gating). Fingerprint mismatch on any
/// active corpus is its own FAIL.
#[must_use]
pub fn classify_readiness(
    config: &SiteConfig,
    active: &[CorpusStatus],
    readiness_ok: bool,
) -> CheckResult {
    if active.is_empty() {
        return CheckResult::warn(
            "readiness.no_active_corpus",
            "no active corpus installed — the site has nothing to serve yet",
            "run `jurisearchctl site catch-up --config <path> --wait`",
        );
    }
    classify_active_readiness(config, active, readiness_ok)
}

/// PURE: the SERVING-GATE readiness classification used by `site readiness` AND the `site install`
/// start gate ([`super::lifecycle::may_start_site`]). Unlike the doctor-advisory [`classify_readiness`],
/// NO ACTIVE CORPUS is a hard FAIL here — a site with nothing to serve must never satisfy the start gate
/// (acceptance: readiness/catch-up is not green pre-catch-up). The stale-stamp class (an active corpus
/// whose writer-owned readiness stamp is missing, i.e. the applied cursor is BEHIND the verified
/// query-ready head) and the fingerprint-mismatch class are likewise hard FAILs.
#[must_use]
pub fn classify_serving_readiness(
    config: &SiteConfig,
    active: &[CorpusStatus],
    readiness_ok: bool,
) -> CheckResult {
    if active.is_empty() {
        return CheckResult::fail(
            "readiness.no_active_corpus",
            "no active corpus installed — the site has nothing to serve yet, so the serving gate is \
             NOT green",
            "run `jurisearchctl site catch-up --config <path> --wait`, then re-check `site readiness`",
        );
    }
    classify_active_readiness(config, active, readiness_ok)
}

/// PURE: the shared stale-stamp + fingerprint tail (assumes at least one active corpus). A stale stamp
/// means the applied cursor is behind the verified query-ready head; a fingerprint mismatch means the
/// local embedder would produce wrong vectors. Both are FAILs in BOTH the advisory and serving paths.
fn classify_active_readiness(
    config: &SiteConfig,
    active: &[CorpusStatus],
    readiness_ok: bool,
) -> CheckResult {
    if !readiness_ok {
        return CheckResult::fail(
            "readiness.stale",
            "an active corpus exists but the writer-owned query_readiness stamp is missing/stale \
             (the applied cursor is behind the verified query-ready head — a writer/apply fault)",
            "re-run catch-up; if it persists, the apply did not stamp readiness for the active topology",
        );
    }
    for corpus in active {
        if !fingerprint_compatible(config, &corpus.embedding_fingerprint) {
            return CheckResult::fail(
                "readiness.fingerprint_mismatch",
                format!(
                    "corpus `{}` was embedded with fingerprint `{}` but the configured query embedder \
                     produces `{}` — hybrid queries would be wrong",
                    corpus.corpus,
                    corpus.embedding_fingerprint,
                    embedder_storage_fingerprint(config),
                ),
                "align embedder.model_name/dimension/normalize with the corpus the producer published",
            );
        }
    }
    CheckResult::ok(
        "readiness.ready",
        format!(
            "{} active corpus(es) are readiness-stamped and fingerprint-compatible",
            active.len()
        ),
    )
}

/// The full readiness report (active-topology + stamp + fingerprint) for the SERVING gate. Live: opens
/// the writer connection (active topology) and the read connection (the real read-path stamp lookup).
/// Drives both `site readiness` and the install start gate, so no active corpus is NOT green.
pub fn readiness_report(
    conn: &dyn WriterConnection,
    read: &ReadHandle,
    config: &SiteConfig,
) -> Result<DiagnosticReport, String> {
    let active = corpus_status(conn).map_err(|error| format!("corpus_status failed: {error}"))?;
    let readiness_ok = {
        let mut client = read
            .client()
            .map_err(|error| format!("read-role connection failed: {error}"))?;
        load_query_readiness_with_client(&mut client).is_ok()
    };
    let mut report = DiagnosticReport::default();
    report.push(classify_serving_readiness(config, &active, readiness_ok));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SITE_CONFIG_EXAMPLE;
    use std::path::Path;

    fn example() -> SiteConfig {
        SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, Path::new("site.toml")).unwrap()
    }

    fn corpus(name: &str, fingerprint: &str) -> CorpusStatus {
        CorpusStatus {
            corpus: name.to_owned(),
            active_generation: "g1".to_owned(),
            sequence: 1,
            baseline_id: "b".to_owned(),
            schema_version: 1,
            embedding_fingerprint: fingerprint.to_owned(),
            builder_versions: serde_json::json!({}),
            last_package_id: None,
            last_package_digest: None,
            applied_at: None,
        }
    }

    #[test]
    fn the_configured_embedder_fingerprint_is_stable_and_matchable() {
        let config = example();
        let fp = embedder_storage_fingerprint(&config);
        assert!(fingerprint_compatible(&config, &fp));
        assert!(!fingerprint_compatible(&config, "something-else"));
    }

    #[test]
    fn no_active_corpus_is_an_advisory_warn_for_the_doctor_variant() {
        // `site doctor` keeps the pre-catch-up advisory (Warn): a structural doctor is still meaningful.
        let config = example();
        let result = classify_readiness(&config, &[], false);
        assert_eq!(result.code, "readiness.no_active_corpus");
        assert_eq!(result.status, super::super::CheckStatus::Warn);
    }

    #[test]
    fn the_serving_gate_is_not_green_with_no_active_corpus() {
        // The serving gate (`site readiness` + the install start gate) must HARD FAIL with no active
        // corpus so an empty site can never be started for clients.
        let config = example();
        let result = classify_serving_readiness(&config, &[], false);
        assert_eq!(result.code, "readiness.no_active_corpus");
        assert_eq!(result.status, super::super::CheckStatus::Fail);
        let mut report = DiagnosticReport::default();
        report.push(result);
        assert!(
            !report.is_green(),
            "serving gate must NOT be green with no active corpus"
        );
    }

    #[test]
    fn the_serving_gate_is_not_green_when_the_cursor_is_behind_the_verified_head() {
        // An active corpus whose writer-owned readiness stamp is missing/stale means the applied cursor
        // is BEHIND the verified query-ready head — the serving gate must NOT be green.
        let config = example();
        let active = vec![corpus("core", &embedder_storage_fingerprint(&config))];
        let result = classify_serving_readiness(&config, &active, false);
        assert_eq!(result.code, "readiness.stale");
        assert_eq!(result.status, super::super::CheckStatus::Fail);
        let mut report = DiagnosticReport::default();
        report.push(result);
        assert!(!report.is_green());
    }

    #[test]
    fn the_serving_gate_is_green_for_a_ready_fingerprint_compatible_corpus() {
        let config = example();
        let active = vec![corpus("core", &embedder_storage_fingerprint(&config))];
        let result = classify_serving_readiness(&config, &active, true);
        assert_eq!(result.code, "readiness.ready");
        assert_eq!(result.status, super::super::CheckStatus::Ok);
    }

    #[test]
    fn active_corpus_with_a_missing_stamp_is_a_distinct_stale_fail() {
        let config = example();
        let active = vec![corpus("core", &embedder_storage_fingerprint(&config))];
        let result = classify_readiness(&config, &active, false);
        assert_eq!(result.code, "readiness.stale");
        assert_eq!(result.status, super::super::CheckStatus::Fail);
    }

    #[test]
    fn a_fingerprint_mismatch_is_a_distinct_fail() {
        let config = example();
        let active = vec![corpus("core", "wrong-fingerprint")];
        let result = classify_readiness(&config, &active, true);
        assert_eq!(result.code, "readiness.fingerprint_mismatch");
        assert_eq!(result.status, super::super::CheckStatus::Fail);
    }

    #[test]
    fn a_ready_fingerprint_compatible_corpus_is_green() {
        let config = example();
        let active = vec![corpus("core", &embedder_storage_fingerprint(&config))];
        let result = classify_readiness(&config, &active, true);
        assert_eq!(result.code, "readiness.ready");
        assert_eq!(result.status, super::super::CheckStatus::Ok);
    }
}
