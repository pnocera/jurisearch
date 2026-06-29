//! `demo up|url|smoke|down` (plan `01` Phase 7/8, M5-B): stand up a LOCAL single-host demo using the
//! REAL site/syncd binaries + the signed FIXTURE corpus, prove it from a client, and tear it down.
//!
//! The demo reuses the M4 operator ops verbatim (provision → render/install → bootstrap-trust → catch-up
//! → readiness) pointed at the bundled fixture published root, so it exercises the SAME path an operator
//! runs — not a mock. Its smoke runs the REAL status/fetch/search legs ([`super::smoke::run_smoke`]).
//!
//! The one demo-specific DECISION pinned by the acceptance gate ("demo exercises real status/fetch/search
//! and skips hybrid ONLY with an explicit recorded reason when model/tokenizer assets are absent") lives
//! here as a PURE function: [`hybrid_plan`] turns "are the local embedder model + tokenizer assets
//! present?" into either a hybrid-enabled plan or a fixture plan whose hybrid leg is a RECORDED skip. The
//! asset probe ([`model_tokenizer_assets_present`]) is the only IO, and the URL derivation
//! ([`site_url`]) is pure over the parsed `site.bind`.

use jurisearch_client::SiteEndpoint;

use crate::bind::{BindAddress, parse_bind};
use crate::config::SiteConfig;
use crate::error::DeployError;

use super::fixture;
use super::smoke::SmokePlan;

/// The explicit reason recorded for the hybrid leg when the local embedder assets are absent.
pub const HYBRID_ASSETS_ABSENT_REASON: &str = "hybrid search skipped: the local bge-m3 model and/or tokenizer assets are absent, so the loopback \
     query embedder cannot run (run `jurisearchctl embed doctor` / `embed fetch-assets` to provision)";

/// PURE: derive the client-facing site URL from the parsed `site.bind`. A TCP bind becomes
/// `tcp://host:port`; a Unix bind becomes `unix:///absolute/path` — exactly the URL forms the thin
/// client's `--server` accepts, so `demo url` output is copy-pasteable into `jurisearch-client`.
///
/// # Errors
/// [`DeployError::Validation`] when `site.bind` cannot be parsed.
pub fn site_url(config: &SiteConfig) -> Result<String, DeployError> {
    match parse_bind(&config.site.bind) {
        Ok(BindAddress::Tcp { host_port, .. }) => Ok(format!("tcp://{host_port}")),
        Ok(BindAddress::Unix { path }) => Ok(format!("unix://{path}")),
        Err(error) => {
            let mut errors = crate::error::ValidationErrors::default();
            errors.push(
                "demo.bind",
                format!(
                    "site.bind `{}` is not a valid bind: {error}",
                    config.site.bind
                ),
                "set site.bind to tcp://host:port or unix:///absolute/path",
            );
            Err(DeployError::Validation(errors))
        }
    }
}

/// Resolve the demo's client endpoint (the [`site_url`] parsed into a thin-client [`SiteEndpoint`]).
///
/// # Errors
/// [`DeployError::Validation`] when the bind/url cannot be parsed.
pub fn site_endpoint(config: &SiteConfig) -> Result<SiteEndpoint, DeployError> {
    let url = site_url(config)?;
    jurisearch_client::parse_endpoint(&url).map_err(|error| {
        let mut errors = crate::error::ValidationErrors::default();
        errors.push(
            "demo.endpoint",
            format!("could not resolve the demo endpoint from `{url}`: {error}"),
            "check site.bind",
        );
        DeployError::Validation(errors)
    })
}

/// IO probe: are BOTH the local embedder model weights AND tokenizer present? This is the fact the
/// hybrid-vs-skip decision turns on (the loopback bge-m3 cannot embed a query without them).
#[must_use]
pub fn model_tokenizer_assets_present(config: &SiteConfig) -> bool {
    config.embedder.model_path.exists() && config.embedder.tokenizer_json.exists()
}

/// PURE: the demo smoke plan. When the embedder assets are present the HYBRID leg runs; when they are
/// absent the hybrid leg is recorded as a skip carrying [`HYBRID_ASSETS_ABSENT_REASON`] — NEVER silently
/// dropped. The status/fetch/BM25/negative legs always run (they need no embedder).
#[must_use]
pub fn hybrid_plan(assets_present: bool) -> SmokePlan {
    if assets_present {
        fixture::fixture_smoke_plan_with_hybrid()
    } else {
        fixture::fixture_smoke_plan_without_hybrid(HYBRID_ASSETS_ABSENT_REASON)
    }
}

/// The demo smoke plan for `config`, deciding the hybrid leg from the on-disk embedder assets.
#[must_use]
pub fn demo_smoke_plan(config: &SiteConfig) -> SmokePlan {
    hybrid_plan(model_tokenizer_assets_present(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SITE_CONFIG_EXAMPLE;
    use std::path::Path;

    fn example() -> SiteConfig {
        SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, Path::new("site.toml")).unwrap()
    }

    #[test]
    fn site_url_derives_a_client_pasteable_url_from_the_bind() {
        let mut config = example();
        config.site.bind = "tcp://127.0.0.1:8099".to_owned();
        assert_eq!(site_url(&config).unwrap(), "tcp://127.0.0.1:8099");
        config.site.bind = "unix:///run/jurisearch/site.sock".to_owned();
        assert_eq!(
            site_url(&config).unwrap(),
            "unix:///run/jurisearch/site.sock"
        );
    }

    #[test]
    fn a_bad_bind_is_a_loud_error_not_a_guess() {
        let mut config = example();
        config.site.bind = "http://nope".to_owned();
        assert!(site_url(&config).is_err());
    }

    #[test]
    fn hybrid_runs_when_assets_present_and_is_a_recorded_skip_when_absent() {
        let present = hybrid_plan(true);
        assert!(present.hybrid_enabled, "assets present → hybrid leg runs");

        let absent = hybrid_plan(false);
        assert!(!absent.hybrid_enabled, "assets absent → hybrid leg skipped");
        assert!(
            !absent.hybrid_skip_reason.is_empty(),
            "the hybrid skip MUST carry an explicit recorded reason (never silent)"
        );
        assert!(absent.hybrid_skip_reason.contains("hybrid"));
    }

    #[test]
    fn the_non_hybrid_legs_are_always_present_in_the_demo_plan() {
        // The status/fetch/BM25/negative legs reference the stable fixture id regardless of assets.
        let plan = hybrid_plan(false);
        assert_eq!(plan.known_id, fixture::FIXTURE_DOC_ID);
        assert_eq!(plan.missing_id, fixture::FIXTURE_MISSING_ID);
    }
}
