//! No-infra acceptance gate for the M3 Phase 4 alert-hook seam: a configured command runs (with the
//! class/group/run-id passed as env vars) ONLY for trigger classes — a `publish-failed` pages, a
//! `no-op`/`skipped-lock-held` does not. No provider is hardcoded.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use jurisearch_producer::PRODUCER_CONFIG_EXAMPLE;
use jurisearch_producer::alert::{AlertEvent, class_triggers, fire_if_triggered};
use jurisearch_producer::config::ProducerConfig;

fn base_config(root: &Path) -> ProducerConfig {
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
    let toml =
        PRODUCER_CONFIG_EXAMPLE.replace("/etc/jurisearch/secrets", secrets.to_str().unwrap());
    ProducerConfig::parse_str(&toml, Path::new("producer.toml")).unwrap()
}

/// A hook script that records the class it was invoked with into `marker_dir/<class>`.
fn write_hook(dir: &Path, marker_dir: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(marker_dir).unwrap();
    let script = dir.join("hook.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$JURISEARCH_ALERT_GROUP\" > \"{}/$JURISEARCH_ALERT_CLASS\"\n",
            marker_dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[test]
fn the_hook_fires_on_a_failure_class_and_not_on_a_benign_one() {
    let dir = tempfile::tempdir().unwrap();
    let markers = dir.path().join("markers");
    let script = write_hook(dir.path(), &markers);

    let mut config = base_config(dir.path());
    config.alert.hook_command = vec![script.to_string_lossy().into_owned()];
    // Leave on_classes empty ⇒ the default failure set applies.

    // A publish failure is a default trigger ⇒ the hook runs and records the class + group.
    let fired = fire_if_triggered(
        &config,
        &AlertEvent {
            exit_class: "publish-failed",
            group: "legislation",
            run_id: "legislation-1",
            message: "boom",
        },
    )
    .unwrap();
    assert!(fired, "publish-failed must trigger the hook");
    let recorded = std::fs::read_to_string(markers.join("publish-failed")).unwrap();
    assert_eq!(recorded.trim(), "legislation");

    // A benign no-op / expected lock contention is NOT in the default set ⇒ the hook does not run.
    assert!(!class_triggers(&config, "no-op"));
    assert!(!class_triggers(&config, "skipped-lock-held"));
    let fired = fire_if_triggered(
        &config,
        &AlertEvent {
            exit_class: "skipped-lock-held",
            group: "legislation",
            run_id: "legislation-2",
            message: "contended",
        },
    )
    .unwrap();
    assert!(!fired, "lock contention must not page by default");
    assert!(!markers.join("skipped-lock-held").exists());
}

#[test]
fn no_hook_configured_is_a_silent_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let config = base_config(dir.path()); // empty hook_command
    let fired = fire_if_triggered(
        &config,
        &AlertEvent {
            exit_class: "publish-failed",
            group: "legislation",
            run_id: "x",
            message: "y",
        },
    )
    .unwrap();
    assert!(!fired, "no hook configured ⇒ nothing runs");
}

#[test]
fn explicit_on_classes_overrides_the_default_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = base_config(dir.path());
    config.alert.on_classes = vec!["no-op".to_owned()];
    // Now `no-op` triggers but `publish-failed` (not listed) does not.
    assert!(class_triggers(&config, "no-op"));
    assert!(!class_triggers(&config, "publish-failed"));
}
