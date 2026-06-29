//! `site install|uninstall|restart|stop|logs|status` (plan `01` Phase 4): systemd lifecycle wrappers
//! over the M1-A-rendered units, plus the NON-NEGOTIABLE refuse-to-start gate.
//!
//! The unit NAMES are fixed (absolute) from the M1-A render, so the wrappers never guess. The install
//! gate — `jurisearch-site` must not be started until readiness AND embed doctor are green, unless forced
//! — is a pure decision ([`may_start_site`]) unit-tested without systemd. The systemctl/journalctl
//! invocations are built as pure argv vectors ([`systemctl_argv`] etc.) and executed by a thin runner, so
//! the command shape is testable and the tests never call the real `systemctl`.

use crate::error::DeployError;

/// The three managed unit names (absolute, fixed by the M1-A render file names).
pub const UNIT_BGE_M3: &str = "jurisearch-bge-m3.service";
pub const UNIT_SYNCD: &str = "jurisearch-syncd.service";
pub const UNIT_SITE: &str = "jurisearch-site.service";

/// The prerequisite units, in dependency order (started before the query service).
pub const PREREQUISITE_UNITS: [&str; 2] = [UNIT_BGE_M3, UNIT_SYNCD];

/// All managed units, in dependency order.
pub const ALL_UNITS: [&str; 3] = [UNIT_BGE_M3, UNIT_SYNCD, UNIT_SITE];

/// The inputs to the refuse-to-start decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartGate {
    /// `site readiness` exited green.
    pub readiness_green: bool,
    /// The site has an embedder configured (always true for the current site schema, kept explicit).
    pub embedder_configured: bool,
    /// `embed doctor` exited green.
    pub embed_doctor_green: bool,
    /// The operator passed `--force`.
    pub force: bool,
}

/// Why starting `jurisearch-site` was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartRefusal {
    ReadinessNotGreen,
    EmbedDoctorNotGreen,
}

impl StartRefusal {
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            StartRefusal::ReadinessNotGreen => {
                "refusing to start jurisearch-site: `site readiness` is not green (no active, \
                 readiness-stamped, fingerprint-compatible corpus). Re-run catch-up, or pass --force."
            }
            StartRefusal::EmbedDoctorNotGreen => {
                "refusing to start jurisearch-site: `embed doctor` is not green (the local bge-m3 \
                 endpoint is unhealthy or fingerprint-incompatible). Fix the embedder, or pass --force."
            }
        }
    }
}

/// PURE: may `jurisearch-site` be started for clients? Allowed iff forced, OR readiness is green AND (the
/// embedder is unconfigured OR embed doctor is green). Readiness is checked first so its diagnostic wins.
///
/// # Errors
/// [`StartRefusal`] when a hard gate is not met and `--force` was not passed.
pub fn may_start_site(gate: &StartGate) -> Result<(), StartRefusal> {
    if gate.force {
        return Ok(());
    }
    if !gate.readiness_green {
        return Err(StartRefusal::ReadinessNotGreen);
    }
    if gate.embedder_configured && !gate.embed_doctor_green {
        return Err(StartRefusal::EmbedDoctorNotGreen);
    }
    Ok(())
}

/// Build the `systemctl` argv for an action over a set of units (pure; the runner prepends nothing).
#[must_use]
pub fn systemctl_argv(action: &str, units: &[&str]) -> Vec<String> {
    let mut argv = vec![action.to_owned()];
    argv.extend(units.iter().map(|unit| (*unit).to_owned()));
    argv
}

/// Build the `journalctl` argv for following one unit's logs.
#[must_use]
pub fn journalctl_argv(unit: &str, follow: bool, lines: u32) -> Vec<String> {
    let mut argv = vec![
        "-u".to_owned(),
        unit.to_owned(),
        "-n".to_owned(),
        lines.to_string(),
    ];
    if follow {
        argv.push("-f".to_owned());
    }
    argv
}

/// Run `systemctl <argv...>`. Thin wrapper isolated so callers (and tests) never shell out implicitly.
pub fn run_systemctl(argv: &[String]) -> Result<(), DeployError> {
    run_program("systemctl", argv)
}

/// Run `journalctl <argv...>` (inherits stdio so logs stream to the operator's terminal).
pub fn run_journalctl(argv: &[String]) -> Result<(), DeployError> {
    run_program("journalctl", argv)
}

fn run_program(program: &str, argv: &[String]) -> Result<(), DeployError> {
    let status = std::process::Command::new(program)
        .args(argv)
        .status()
        .map_err(|source| DeployError::Write {
            path: std::path::PathBuf::from(program),
            source,
        })?;
    if status.success() {
        Ok(())
    } else {
        let mut errors = crate::error::ValidationErrors::default();
        errors.push(
            "systemd.command_failed",
            format!("`{program} {}` failed: {status}", argv.join(" ")),
            "inspect the unit with `journalctl -u <unit>`",
        );
        Err(DeployError::Validation(errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forced_start_is_always_allowed() {
        let gate = StartGate {
            readiness_green: false,
            embedder_configured: true,
            embed_doctor_green: false,
            force: true,
        };
        assert!(may_start_site(&gate).is_ok());
    }

    #[test]
    fn start_is_refused_until_readiness_is_green() {
        let gate = StartGate {
            readiness_green: false,
            embedder_configured: true,
            embed_doctor_green: true,
            force: false,
        };
        assert_eq!(may_start_site(&gate), Err(StartRefusal::ReadinessNotGreen));
    }

    #[test]
    fn start_is_refused_until_embed_doctor_is_green() {
        let gate = StartGate {
            readiness_green: true,
            embedder_configured: true,
            embed_doctor_green: false,
            force: false,
        };
        assert_eq!(
            may_start_site(&gate),
            Err(StartRefusal::EmbedDoctorNotGreen)
        );
    }

    #[test]
    fn start_is_allowed_when_both_gates_are_green() {
        let gate = StartGate {
            readiness_green: true,
            embedder_configured: true,
            embed_doctor_green: true,
            force: false,
        };
        assert!(may_start_site(&gate).is_ok());
    }

    #[test]
    fn argv_builders_have_the_expected_shape() {
        assert_eq!(
            systemctl_argv("enable", &PREREQUISITE_UNITS),
            vec![
                "enable",
                "jurisearch-bge-m3.service",
                "jurisearch-syncd.service"
            ]
        );
        assert_eq!(
            systemctl_argv("start", &[UNIT_SITE]),
            vec!["start", "jurisearch-site.service"]
        );
        assert_eq!(
            journalctl_argv(UNIT_SITE, true, 200),
            vec!["-u", "jurisearch-site.service", "-n", "200", "-f"]
        );
    }

    #[test]
    fn units_are_ordered_with_site_last() {
        assert_eq!(*ALL_UNITS.last().unwrap(), UNIT_SITE);
        assert!(!PREREQUISITE_UNITS.contains(&UNIT_SITE));
    }
}
