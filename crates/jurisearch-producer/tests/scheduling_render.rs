//! No-infra acceptance gates for M3 Phase 3 scheduling: the rendered timers carry `Persistent=true`,
//! every unit path is ABSOLUTE (no unsupported env expansion), and `install` writes one service+timer
//! per fetch group. A documented cron equivalent is generated for non-systemd hosts.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use jurisearch_producer::PRODUCER_CONFIG_EXAMPLE;
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::render::{
    cron_equivalent, install, render_all, service_unit_name, timer_unit_name,
};

/// The example config rewired so unit_dir + binary/config/state paths land under `root` (still absolute).
fn config_under(root: &Path) -> ProducerConfig {
    let secrets = root.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    for name in ["postgres-admin-password", "jurisearch-write-password"] {
        let p = secrets.join(name);
        std::fs::write(&p, "x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let seed = secrets.join("producer-signing.seed");
    std::fs::write(&seed, "00".repeat(32)).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o600)).unwrap();

    let toml = PRODUCER_CONFIG_EXAMPLE
        .replace("/etc/jurisearch/secrets", secrets.to_str().unwrap())
        .replace("/etc/systemd/system", root.join("units").to_str().unwrap())
        .replace(
            "/var/lib/jurisearch-producer",
            root.join("state").to_str().unwrap(),
        );
    let config = ProducerConfig::parse_str(&toml, Path::new("producer.toml")).unwrap();
    config.validate().unwrap();
    config
}

#[test]
fn every_timer_is_persistent_and_every_unit_path_is_absolute() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let units = render_all(&config);
    // Two groups in the example ⇒ four units (service + timer each).
    assert_eq!(units.len(), 4, "{:?}", units);

    let timers: Vec<_> = units
        .iter()
        .filter(|u| u.file_name.ends_with(".timer"))
        .collect();
    assert_eq!(timers.len(), 2);
    for timer in &timers {
        assert!(
            timer.contents.contains("Persistent=true"),
            "missed-window recovery requires Persistent=true:\n{}",
            timer.contents
        );
        assert!(timer.contents.contains("OnCalendar="));
        assert!(timer.contents.contains("RandomizedDelaySec="));
        // The unit file path is absolute.
        assert!(timer.path.is_absolute());
    }

    for unit in &units {
        // No unsupported env expansion in the rendered unit body (ExecStart/paths are absolute).
        assert!(
            !unit.contents.contains("$HOME") && !unit.contents.contains("${"),
            "unit must not rely on env expansion in paths:\n{}",
            unit.contents
        );
    }
}

#[test]
fn the_service_runs_update_for_its_group_with_absolute_binary_and_config() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let units = render_all(&config);
    let svc = units
        .iter()
        .find(|u| u.file_name == service_unit_name("legislation"))
        .expect("legislation service rendered");
    assert!(svc.contents.contains("Type=oneshot"));
    assert!(
        svc.contents.contains("update --config /"),
        "{}",
        svc.contents
    );
    assert!(svc.contents.contains("--group legislation"));
    // Hardening + absolute ReadWritePaths.
    assert!(svc.contents.contains("ProtectSystem=strict"));
    assert!(svc.contents.contains("ReadWritePaths=/"));
    // The legislation timer uses the after-the-drop default window (22:30).
    let tmr = units
        .iter()
        .find(|u| u.file_name == timer_unit_name("legislation"))
        .unwrap();
    assert!(tmr.contents.contains("OnCalendar=*-*-* 22:30:00"));
    // Jurisprudence runs daily (NOT weekly) so JADE/CAPP/INCA are caught promptly.
    let juri = units
        .iter()
        .find(|u| u.file_name == timer_unit_name("jurisprudence"))
        .unwrap();
    assert!(juri.contents.contains("OnCalendar=*-*-* 23:30:00"));
}

#[test]
fn install_writes_one_service_and_timer_per_group_and_dry_run_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let unit_dir = config.install.unit_dir.clone();

    // Dry-run writes nothing.
    let dry = install(&config, true).unwrap();
    assert!(dry.dry_run);
    assert!(!unit_dir.exists() || std::fs::read_dir(&unit_dir).unwrap().count() == 0);

    // Real install writes all four units.
    let report = install(&config, false).unwrap();
    assert_eq!(report.written.len(), 4);
    assert_eq!(report.timers.len(), 2);
    for name in &report.written {
        assert!(unit_dir.join(name).exists(), "missing {name}");
    }
}

#[test]
fn cron_equivalent_lists_a_daily_line_per_group() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let cron = cron_equivalent(&config);
    // `M H * * *` daily lines, one per group, calling `update --group <name>`.
    assert!(cron.contains("30 22 * * *"), "{cron}");
    assert!(cron.contains("30 23 * * *"), "{cron}");
    assert!(cron.contains("--group legislation"));
    assert!(cron.contains("--group jurisprudence"));
}
