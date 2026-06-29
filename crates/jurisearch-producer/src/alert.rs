//! The fail-closed ALERT-HOOK seam (M3 Phase 4).
//!
//! Consistent with the project's no-hidden-egress stance, the producer makes NO built-in external calls.
//! Instead it invokes an operator-configured command (`[alert].hook_command`, an argv — no shell) when a
//! run's exit class is in the trigger set (`[alert].on_classes`, or the default failure set). The class,
//! group, run id, and message are passed as `JURISEARCH_ALERT_*` environment variables so the operator's
//! script can route to whatever provider they choose. An empty `hook_command` disables alerting.

use std::process::Command;

use crate::config::ProducerConfig;
use crate::error::ProducerError;
use crate::exit::DEFAULT_ALERT_CLASSES;

/// The context handed to an alert hook.
#[derive(Debug, Clone)]
pub struct AlertEvent<'a> {
    pub exit_class: &'a str,
    pub group: &'a str,
    pub run_id: &'a str,
    pub message: &'a str,
}

/// Whether `class` should trigger the alert hook under `config`: the configured `on_classes`, or — when
/// that is empty — the default failure set ([`DEFAULT_ALERT_CLASSES`]).
#[must_use]
pub fn class_triggers(config: &ProducerConfig, class: &str) -> bool {
    if config.alert.on_classes.is_empty() {
        DEFAULT_ALERT_CLASSES.contains(&class)
    } else {
        config.alert.on_classes.iter().any(|c| c == class)
    }
}

/// Fire the alert hook for `event` IF a hook is configured AND the event's class is a trigger. Returns
/// `Ok(true)` if the hook was invoked (and exited 0), `Ok(false)` if no hook ran (none configured or the
/// class is not a trigger). A non-zero hook exit or spawn failure is surfaced as an error so a wrapper
/// can log it — but it is best-effort and never masks the original run outcome (the caller decides).
pub fn fire_if_triggered(
    config: &ProducerConfig,
    event: &AlertEvent<'_>,
) -> Result<bool, ProducerError> {
    let argv = &config.alert.hook_command;
    if argv.is_empty() || !class_triggers(config, event.exit_class) {
        return Ok(false);
    }
    let program = &argv[0];
    let status = Command::new(program)
        .args(&argv[1..])
        .env("JURISEARCH_ALERT_CLASS", event.exit_class)
        .env("JURISEARCH_ALERT_GROUP", event.group)
        .env("JURISEARCH_ALERT_RUN_ID", event.run_id)
        .env("JURISEARCH_ALERT_MESSAGE", event.message)
        .status()
        .map_err(|source| ProducerError::AlertHook {
            command: program.clone(),
            message: format!("failed to spawn: {source}"),
        })?;
    if status.success() {
        Ok(true)
    } else {
        Err(ProducerError::AlertHook {
            command: program.clone(),
            message: format!("hook exited with {status}"),
        })
    }
}
