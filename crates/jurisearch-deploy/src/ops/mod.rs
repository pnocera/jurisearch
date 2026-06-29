//! M4 operator commands (plan `01-makeitsimpletodeploy` Phases 2/4/5/6): site doctor, install,
//! systemd lifecycle, trust bootstrap, catch-up, readiness, and the bge-m3 embedder operator surface.
//!
//! Every command WRAPS the already-built primitives — it never reimplements them:
//! - trust / catch-up / status come from `jurisearch-syncd`
//!   (`install_trust_anchor`/`load_package_verifier`/`install_verified_license_token`/`check_entitlement`,
//!   `fetch_verify_manifest`/`plan_catchup`/`run_catchup`/`DirectoryCatchupSource`, `corpus_status`);
//! - DB provisioning / migrations / readiness come from `jurisearch-storage`
//!   (`run_migrations_on`, `provision_roles` (SITE profile), `load_query_readiness_with_client`);
//! - embedder health / fingerprint come from `jurisearch-embed`
//!   (`OpenAiCompatibleClient`, `EmbeddingConfig::storage_embedding_fingerprint`).
//!
//! The DECISION logic that the acceptance gates pin (doctor state classification, refuse-to-start,
//! trust-not-silently-replaced, catch-up-not-green-when-behind, fingerprint compatibility) lives here as
//! PURE functions over injected/fixture states, so it is unit-testable with no live DB / systemd.

pub mod catchup;
pub mod connection;
pub mod demo;
pub mod doctor;
pub mod embed;
pub mod fixture;
pub mod lifecycle;
pub mod provision;
pub mod readiness;
pub mod smoke;
pub mod trust;
pub mod watchdog;

use serde::Serialize;

/// The status of one operator diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// The check passed.
    Ok,
    /// An ADVISORY, non-blocking observation (e.g. "not yet bootstrapped / not yet caught up"): the
    /// deployment can still progress, but the operator should know. Pre-serving advisories never gate.
    Warn,
    /// A blocking failure — this prerequisite is wrong and must be fixed before serving clients.
    Fail,
    /// The check needs infrastructure (a live DB, a running endpoint, systemd) that is absent in this
    /// run, so it was not performed. A skip is never a pass and never a failure.
    Skipped,
}

/// One operator diagnostic with a STABLE machine `code` (distinct per failure class — never a generic
/// "not ready"), a human `message`, and a concrete next-command `suggestion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    pub code: &'static str,
    pub status: CheckStatus,
    pub message: String,
    pub suggestion: String,
}

impl CheckResult {
    pub fn ok(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            status: CheckStatus::Ok,
            message: message.into(),
            suggestion: String::new(),
        }
    }

    pub fn warn(
        code: &'static str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            code,
            status: CheckStatus::Warn,
            message: message.into(),
            suggestion: suggestion.into(),
        }
    }

    pub fn fail(
        code: &'static str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            code,
            status: CheckStatus::Fail,
            message: message.into(),
            suggestion: suggestion.into(),
        }
    }

    pub fn skipped(
        code: &'static str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            code,
            status: CheckStatus::Skipped,
            message: message.into(),
            suggestion: suggestion.into(),
        }
    }
}

/// An ordered collection of diagnostics, e.g. the output of `site doctor` or `embed doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DiagnosticReport {
    pub checks: Vec<CheckResult>,
}

impl DiagnosticReport {
    pub fn push(&mut self, check: CheckResult) {
        self.checks.push(check);
    }

    /// `true` when no check FAILED (advisory `Warn`/`Skipped` do not block — a pre-bootstrap doctor can
    /// be green with advisory "not yet caught up" statuses; the hard serving gates are readiness/embed).
    #[must_use]
    pub fn is_green(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail)
    }

    /// `0` when green, `1` when any check failed — the process exit code.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.is_green())
    }

    /// A stable, machine-readable JSON view (`site doctor --json`, runbooks/CI).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "green": self.is_green(),
            "checks": self.checks,
        })
    }

    /// Render the human-readable lines (one per check) for terminal output.
    #[must_use]
    pub fn to_lines(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            let tag = match check.status {
                CheckStatus::Ok => "OK  ",
                CheckStatus::Warn => "WARN",
                CheckStatus::Fail => "FAIL",
                CheckStatus::Skipped => "SKIP",
            };
            out.push_str(&format!("[{tag}] {} — {}", check.code, check.message));
            if !check.suggestion.is_empty() && check.status != CheckStatus::Ok {
                out.push_str(&format!(" (suggestion: {})", check.suggestion));
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_report_exits_zero_and_warn_skip_do_not_block() {
        let mut report = DiagnosticReport::default();
        report.push(CheckResult::ok("a.ok", "fine"));
        report.push(CheckResult::warn(
            "b.advisory",
            "not yet caught up",
            "run catch-up",
        ));
        report.push(CheckResult::skipped(
            "c.live",
            "no DB configured",
            "set up the DB",
        ));
        assert!(report.is_green());
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn a_single_fail_makes_the_report_red() {
        let mut report = DiagnosticReport::default();
        report.push(CheckResult::ok("a.ok", "fine"));
        report.push(CheckResult::fail("b.bad", "broken", "fix it"));
        assert!(!report.is_green());
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn json_view_carries_green_flag_and_distinct_codes() {
        let mut report = DiagnosticReport::default();
        report.push(CheckResult::fail("db.unreachable", "down", "start pg"));
        let json = report.to_json();
        assert_eq!(json["green"], serde_json::Value::Bool(false));
        assert_eq!(json["checks"][0]["code"], "db.unreachable");
        assert_eq!(json["checks"][0]["status"], "fail");
    }
}
