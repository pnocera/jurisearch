//! The classified exit-code taxonomy (M3 Phase 4) + the default alert-trigger set.
//!
//! Every run/command reports a stable, machine-readable **exit class** string (e.g. `published`,
//! `no-op`, `skipped-lock-held`, `publish-failed`). A timer/alert wrapper can branch on the class
//! WITHOUT parsing logs: a degraded-enrichment run is a warning, a `skipped-lock-held` is expected
//! contention, but a `publish-failed` is a page. The class also maps to a process exit code so a plain
//! `systemctl`/cron wrapper can react with no JSON parsing at all.
//!
//! The success classes (process exit `0`) are the four outcomes of a healthy run plus a dry run:
//! `published`, `published-enrich-degraded` (a warning, still a success), `no-op` (empty window, manifest
//! still refreshed), `rebaselined` (an adopted DILA baseline), and `dry-run`.

/// The exit classes that are a SUCCESS (process exit code 0). A degraded-enrichment publish is a
/// success-with-warning, an empty window is a successful no-op, and an adopted rebaseline is a success.
pub const SUCCESS_CLASSES: &[&str] = &[
    // A generic success for the admin/read commands (validate, install, status, ...).
    "ok",
    "published",
    "published-enrich-degraded",
    "no-op",
    "rebaselined",
    "dry-run",
];

/// Whether `class` is a successful outcome (process exit code 0).
#[must_use]
pub fn is_success(class: &str) -> bool {
    SUCCESS_CLASSES.contains(&class)
}

/// The process exit code for an exit class, in the BSD `sysexits.h` spirit so a cron/systemd wrapper can
/// branch without parsing JSON:
/// - `0` success classes;
/// - `75` (`EX_TEMPFAIL`) transient/retryable: lock contention + upstream unreachable / fetch failures;
/// - `65` (`EX_DATAERR`) a corrupt download / integrity failure;
/// - `69` (`EX_UNAVAILABLE`) the producer DB is not provisioned yet;
/// - `78` (`EX_CONFIG`) a config/usage error;
/// - `70` (`EX_SOFTWARE`) any other permanent failure (ingest/embed/publish/storage/needs-rebaseline).
#[must_use]
pub fn exit_code_for(class: &str) -> u8 {
    match class {
        c if is_success(c) => 0,
        // Transient / retryable — the next timer firing is expected to clear it.
        "skipped-lock-held" | "fetch-failed" | "upstream-unreachable" => 75,
        // Bad upstream bytes (the cursor did not advance; retry will re-download).
        "integrity-failed" => 65,
        "producer-db-unprovisioned" => 69,
        "config-invalid" => 78,
        // Everything else is a permanent failure that needs operator attention (a page).
        _ => 70,
    }
}

/// The DEFAULT set of exit classes that fire the alert hook when `[alert].on_classes` is left empty: the
/// hard failures an operator must see. Deliberately EXCLUDES `skipped-lock-held` (expected contention)
/// and `published-enrich-degraded` (a warning surfaced via `status`, not a page).
pub const DEFAULT_ALERT_CLASSES: &[&str] = &[
    "fetch-failed",
    "upstream-unreachable",
    "integrity-failed",
    "ingest-failed",
    "embed-failed",
    "enrich-degraded",
    "publish-failed",
    "storage-failed",
    "provision-failed",
    "producer-db-unprovisioned",
    "needs-rebaseline",
    "config-invalid",
    "io-failed",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_classes_exit_zero_and_failures_do_not() {
        for class in SUCCESS_CLASSES {
            assert_eq!(exit_code_for(class), 0, "{class} should be success");
        }
        assert_eq!(exit_code_for("skipped-lock-held"), 75);
        assert_eq!(exit_code_for("upstream-unreachable"), 75);
        assert_eq!(exit_code_for("integrity-failed"), 65);
        assert_eq!(exit_code_for("publish-failed"), 70);
        assert_eq!(exit_code_for("needs-rebaseline"), 70);
        assert_eq!(exit_code_for("producer-db-unprovisioned"), 69);
        assert_eq!(exit_code_for("config-invalid"), 78);
    }

    #[test]
    fn lock_contention_is_not_a_default_alert_but_publish_failure_is() {
        assert!(!DEFAULT_ALERT_CLASSES.contains(&"skipped-lock-held"));
        assert!(!DEFAULT_ALERT_CLASSES.contains(&"published-enrich-degraded"));
        assert!(DEFAULT_ALERT_CLASSES.contains(&"publish-failed"));
        assert!(DEFAULT_ALERT_CLASSES.contains(&"needs-rebaseline"));
    }
}
