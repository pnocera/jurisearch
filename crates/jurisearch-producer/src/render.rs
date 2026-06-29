//! Deterministic systemd `.service` + `.timer` rendering and `jurisearch-producer install` (M3 Phase 3).
//!
//! One service+timer pair per fetch group (`jurisearch-producer-<group>.service` / `.timer`). Every path
//! in a rendered unit is ABSOLUTE — systemd does not expand `$HOME`/env in unit file paths (the same
//! discipline as the M1-A renderer) — and every timer carries `Persistent=true` so a window missed while
//! the host was asleep runs on the next boot. The render functions are pure (string in → string out) so
//! they are unit-testable with no filesystem; [`install`] writes them.

use std::path::{Path, PathBuf};

use crate::config::{FetchGroupConfig, ProducerConfig};
use crate::error::ProducerError;

/// The service unit file name for a group.
#[must_use]
pub fn service_unit_name(group: &str) -> String {
    format!("jurisearch-producer-{group}.service")
}

/// The timer unit file name for a group.
#[must_use]
pub fn timer_unit_name(group: &str) -> String {
    format!("jurisearch-producer-{group}.timer")
}

/// Render the `oneshot` service unit that runs `update --group <group>` once. All paths are absolute;
/// PISTE/OpenRouter creds come from an `EnvironmentFile`, never inline. Hardened to match the existing
/// `deploy/systemd` units (dedicated user, `ProtectSystem`, `ReadWritePaths`, low IO/CPU priority so a
/// ~1 GB baseline pull does not starve the box).
#[must_use]
pub fn render_service(config: &ProducerConfig, group: &FetchGroupConfig) -> String {
    let install = &config.install;
    let binary = install.binary_path.display();
    let cfg = install.config_path.display();
    let user = &install.service_user;
    let env_file = install.environment_file.display();
    // The DB-mutating run writes the served root, the DILA mirror, and the local state dir.
    let corpora = config.producer.corpora_dir.display();
    let archives = config.producer.archives_dir.display();
    let state = config.producer.state_dir.display();
    format!(
        "# JuriSearch update-server producer — fetch group `{group_name}` (work/10 M3). Rendered by\n\
         # `jurisearch-producer install`; all paths are ABSOLUTE (systemd does not expand env in unit\n\
         # paths). A oneshot driven by the `{timer}` timer; it holds the single `update-core` flock\n\
         # across ingest -> enrich -> embed -> publish so the two group timers never mutate the DB\n\
         # concurrently.\n\
         [Unit]\n\
         Description=JuriSearch producer update ({group_name})\n\
         Documentation=https://github.com/pnocera/jurisearch\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         User={user}\n\
         Group={user}\n\
         EnvironmentFile={env_file}\n\
         ExecStart={binary} update --config {cfg} --group {group_name}\n\
         # A ~1 GB baseline pull must not starve interactive work on the host.\n\
         Nice=10\n\
         IOSchedulingClass=best-effort\n\
         IOSchedulingPriority=6\n\
         # Hardening (matches deploy/systemd/*.service).\n\
         NoNewPrivileges=true\n\
         ProtectSystem=strict\n\
         ProtectHome=true\n\
         PrivateTmp=true\n\
         ReadWritePaths={corpora} {archives} {state}\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
        group_name = group.name,
        timer = timer_unit_name(&group.name),
        user = user,
        env_file = env_file,
        binary = binary,
        cfg = cfg,
        corpora = corpora,
        archives = archives,
        state = state,
    )
}

/// Render the `.timer` for a group: a daily `OnCalendar` (per-group default or override) with
/// `Persistent=true` (recover a missed window) and `RandomizedDelaySec` (do not all fire on the hour).
#[must_use]
pub fn render_timer(config: &ProducerConfig, group: &FetchGroupConfig) -> String {
    format!(
        "# JuriSearch producer timer — fetch group `{group_name}` (work/10 M3). Daily; `Persistent=true`\n\
         # recovers a window missed while the host was asleep. Jurisprudence runs daily and no-ops the\n\
         # sources with nothing new, so CASS's weekly drop and JADE's daily drop are both caught promptly.\n\
         [Unit]\n\
         Description=JuriSearch producer update timer ({group_name})\n\
         \n\
         [Timer]\n\
         OnCalendar={on_calendar}\n\
         RandomizedDelaySec={delay}\n\
         Persistent=true\n\
         Unit={service}\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n",
        group_name = group.name,
        on_calendar = group.on_calendar(),
        delay = config.install.randomized_delay_secs,
        service = service_unit_name(&group.name),
    )
}

/// A rendered unit destined for `unit_dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedUnit {
    pub file_name: String,
    pub path: PathBuf,
    pub contents: String,
}

/// Render every group's service+timer pair (deterministic, ordered by group). Pure — no filesystem.
#[must_use]
pub fn render_all(config: &ProducerConfig) -> Vec<RenderedUnit> {
    let unit_dir = &config.install.unit_dir;
    let mut units = Vec::with_capacity(config.fetch_groups.len() * 2);
    for group in &config.fetch_groups {
        let svc = service_unit_name(&group.name);
        let tmr = timer_unit_name(&group.name);
        units.push(RenderedUnit {
            path: unit_dir.join(&svc),
            file_name: svc,
            contents: render_service(config, group),
        });
        units.push(RenderedUnit {
            path: unit_dir.join(&tmr),
            file_name: tmr,
            contents: render_timer(config, group),
        });
    }
    units
}

/// What `install` did (or, with `dry_run`, WOULD do).
#[derive(Debug, Clone)]
pub struct InstallReport {
    pub dry_run: bool,
    pub unit_dir: PathBuf,
    pub written: Vec<String>,
    pub timers: Vec<String>,
}

/// Render + install the producer service/timer units into `[install].unit_dir`. With `dry_run`, nothing
/// is written. Does NOT start/enable anything (systemctl is the operator's `--now` step) — keeping the
/// renderer side-effect-light and testable; the printed timer list tells the operator what to enable.
pub fn install(config: &ProducerConfig, dry_run: bool) -> Result<InstallReport, ProducerError> {
    let units = render_all(config);
    let unit_dir = config.install.unit_dir.clone();
    if !dry_run {
        std::fs::create_dir_all(&unit_dir).map_err(|source| ProducerError::Io {
            path: unit_dir.clone(),
            source,
        })?;
        for unit in &units {
            std::fs::write(&unit.path, unit.contents.as_bytes()).map_err(|source| {
                ProducerError::Io {
                    path: unit.path.clone(),
                    source,
                }
            })?;
        }
    }
    let written = units.iter().map(|u| u.file_name.clone()).collect();
    let timers = config
        .fetch_groups
        .iter()
        .map(|g| timer_unit_name(&g.name))
        .collect();
    Ok(InstallReport {
        dry_run,
        unit_dir,
        written,
        timers,
    })
}

/// The documented cron equivalent for NON-systemd hosts. Generated from the configured groups so it
/// always matches the installed schedule; the producer's own `update-core` flock (not cron) provides the
/// mutual exclusion, so overlapping cron firings serialize safely (a timed-out wait is a recorded
/// `skipped-lock-held`, not a crash).
#[must_use]
pub fn cron_equivalent(config: &ProducerConfig) -> String {
    let binary = config.install.binary_path.display();
    let cfg = config.install.config_path.display();
    let mut out = String::from(
        "# JuriSearch producer — cron equivalent for non-systemd hosts (work/10 M3).\n\
         # The `update-core` flock inside the binary serializes overlapping runs, so these lines are\n\
         # safe even if two fire close together: the second waits, then proceeds, or records a\n\
         # `skipped-lock-held` if the wait times out. Install into the dedicated service user's crontab.\n\
         # Source PISTE/OPENROUTER creds in the user's environment (e.g. via a sourced profile).\n",
    );
    for group in &config.fetch_groups {
        // Translate the default daily HH:MM windows into cron's `M H * * *`.
        let (hour, minute) = cron_hhmm_for(group);
        out.push_str(&format!(
            "{minute} {hour} * * *  {binary} update --config {cfg} --group {name}\n",
            minute = minute,
            hour = hour,
            binary = binary,
            cfg = cfg,
            name = group.name,
        ));
    }
    out
}

/// Best-effort `(hour, minute)` for the cron line, parsed from the group's `OnCalendar` daily form
/// (`*-*-* HH:MM[:SS]`); falls back to 22:00 if the expression is not the simple daily shape.
fn cron_hhmm_for(group: &FetchGroupConfig) -> (u32, u32) {
    let expr = group.on_calendar();
    expr.rsplit(' ')
        .next()
        .and_then(|hms| {
            let mut parts = hms.split(':');
            let h = parts.next()?.parse::<u32>().ok()?;
            let m = parts.next()?.parse::<u32>().ok()?;
            (h < 24 && m < 60).then_some((h, m))
        })
        .unwrap_or((22, 0))
}

/// True if `path` looks absolute (the unit-path discipline). Small helper reused by tests.
#[must_use]
pub fn is_absolute(path: &Path) -> bool {
    path.is_absolute()
}
