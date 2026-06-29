//! Unit tests for the deploy layer: strict parse, validation (incl. loopback-only embedder guard),
//! golden env/unit rendering, bind translation, redaction, and secret-file permissions. Pure logic —
//! no DB, no network; filesystem tests use `tempfile`.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::bind::{BindAddress, TcpExposure, parse_bind};
use crate::config::{SITE_CONFIG_EXAMPLE, SiteConfig, TrustPurpose};
use crate::error::DeployError;
use crate::scaffold::{InitOutcome, init_site_config};
use crate::secret::{self, SecretString, redact};

fn example() -> SiteConfig {
    SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, Path::new("site.toml"))
        .expect("example site.toml must strict-parse")
}

// ---------------------------------------------------------------------------------------------
// Strict parse
// ---------------------------------------------------------------------------------------------

#[test]
fn config_example_round_trips_through_parse_and_validate() {
    let config = example();
    config
        .validate()
        .expect("the shipped config-example must validate");
}

#[test]
fn unknown_key_is_a_strict_parse_error() {
    let mut text = SITE_CONFIG_EXAMPLE.to_owned();
    text.push_str("\n[site.surprise]\nnope = true\n");
    let error = SiteConfig::parse_str(&text, Path::new("site.toml")).unwrap_err();
    assert!(matches!(error, DeployError::Parse { .. }), "got {error:?}");
}

#[test]
fn unknown_inline_key_is_rejected() {
    let text = SITE_CONFIG_EXAMPLE.replace("workers = 8", "workers = 8\nbogus = 1");
    let error = SiteConfig::parse_str(&text, Path::new("site.toml")).unwrap_err();
    assert!(matches!(error, DeployError::Parse { .. }), "got {error:?}");
}

#[test]
fn missing_required_field_is_a_parse_error() {
    let text = SITE_CONFIG_EXAMPLE.replace("workers = 8\n", "");
    let error = SiteConfig::parse_str(&text, Path::new("site.toml")).unwrap_err();
    assert!(matches!(error, DeployError::Parse { .. }), "got {error:?}");
}

// ---------------------------------------------------------------------------------------------
// Loopback-only embedder guard
// ---------------------------------------------------------------------------------------------

#[test]
fn loopback_embedder_is_accepted() {
    let config = example();
    assert!(config.validate().is_ok());
}

#[test]
fn external_embedder_url_is_rejected() {
    let mut config = example();
    config.embedder.base_url = "https://openrouter.ai/api/v1".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "embedder.base_url.not_loopback"),
        "expected loopback rejection, got {errors}"
    );
}

#[test]
fn non_loopback_ip_embedder_is_rejected() {
    let mut config = example();
    config.embedder.base_url = "http://192.168.0.50:8081".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "embedder.base_url.not_loopback")
    );
}

#[test]
fn embedder_port_mismatch_with_base_url_is_rejected() {
    let mut config = example();
    config.embedder.port = 9000; // base_url says 8081
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "embedder.port.mismatch"),
        "got {errors}"
    );
}

#[test]
fn base_url_without_explicit_port_diverging_from_port_is_rejected() {
    // WARN regression: `http://127.0.0.1` (scheme default port 80) with port = 8081 used to pass
    // because only an EXPLICIT URL port was compared. It must now be rejected.
    let mut config = example();
    config.embedder.base_url = "http://127.0.0.1".to_owned();
    // embedder.port stays 8081 (from the example).
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "embedder.port.mismatch"),
        "got {errors}"
    );
}

// ---------------------------------------------------------------------------------------------
// Render-safety / injection boundary (BLOCKER)
// ---------------------------------------------------------------------------------------------

#[test]
fn model_name_with_newline_injecting_hosted_base_url_is_rejected() {
    // The exact loopback-guard bypass from the review: a newline in model_name attempts to append a
    // SECOND JURISEARCH_EMBED_BASE_URL line pointing at a hosted provider. Validation MUST fail
    // before any render happens.
    let mut config = example();
    config.embedder.model_name =
        "bge-m3\nJURISEARCH_EMBED_BASE_URL=https://openrouter.ai/api/v1".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.value.control_char"),
        "got {errors}"
    );
}

#[test]
fn corpus_name_with_whitespace_or_newline_is_rejected() {
    let mut config = example();
    config.sync.corpora = vec!["core extra".to_owned()];
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.identifier.invalid"),
        "whitespace corpus must be rejected, got {errors}"
    );

    config.sync.corpora = vec!["core\n--corpus injected".to_owned()];
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.identifier.invalid"),
        "newline corpus must be rejected, got {errors}"
    );
}

#[test]
fn path_with_newline_is_rejected() {
    let mut config = example();
    config.sync.source_root = "/srv/jurisearch/packages\nReadWritePaths=/etc".into();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.path.control_char"),
        "got {errors}"
    );
}

#[test]
fn service_user_with_newline_is_rejected() {
    let mut config = example();
    config.system.service_user = "jurisearch\nExecStartPre=/bin/sh -c id".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.identifier.invalid"),
        "got {errors}"
    );
}

#[test]
fn db_user_with_control_char_is_rejected() {
    let mut config = example();
    config.database.writer_user = "jurisearch_write\u{0007}".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.identifier.invalid"),
        "got {errors}"
    );
}

#[test]
fn db_host_with_newline_is_rejected() {
    let mut config = example();
    config.database.host = "127.0.0.1\nJURISEARCH_DB_NAME=evil".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.value.control_char"),
        "got {errors}"
    );
}

#[test]
fn db_host_with_whitespace_forging_argv_flag_is_rejected() {
    // The exact review case: a space in `database.host` would be split by systemd's `${VAR}`
    // expansion into extra `ExecStart` argv words, forging a flag. Validation MUST fail.
    let mut config = example();
    config.database.host = "127.0.0.1 --max-concurrent-embeds 9999".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.value.argv_unsafe"),
        "space-bearing db host must be rejected, got {errors}"
    );
}

#[test]
fn model_path_with_whitespace_is_rejected() {
    // `embedder.model_path` is expanded as `${JURISEARCH_BGE_M3_MODEL}` in the bge-m3 ExecStart;
    // a space would add an extra argv token, so it must be a single token.
    let mut config = example();
    config.embedder.model_path = "/srv/jurisearch/models/bge m3.gguf".into();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.path.argv_unsafe"),
        "space-bearing model_path must be rejected, got {errors}"
    );
}

#[test]
fn unix_socket_bind_with_whitespace_is_rejected() {
    // `site.bind` is expanded as `${JURISEARCH_SITE_BIND}` in the site ExecStart; a space in the
    // unix socket path would split into extra argv words.
    let mut config = example();
    config.site.bind = "unix:///run/jurisearch/jurisearch site.sock".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.value.argv_unsafe"),
        "space-bearing unix bind must be rejected, got {errors}"
    );
}

#[test]
fn value_with_dollar_triggering_expansion_is_rejected() {
    // A `$` in an argv-bound value would let systemd perform a nested `${VAR}`/`$VAR` expansion.
    let mut config = example();
    config.database.host = "127.0.0.1${EVIL}".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.value.argv_unsafe"),
        "value containing `$` must be rejected, got {errors}"
    );
}

#[test]
fn install_dir_with_whitespace_is_rejected() {
    // `system.install_dir` is inlined as the `ExecStart` binary command word (`{dir}/jurisearch`);
    // whitespace would split the executable path into separate argv words.
    let mut config = example();
    config.system.install_dir = "/usr/local/evil bin".into();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "render.path.argv_unsafe"),
        "space-bearing install_dir must be rejected, got {errors}"
    );
}

#[test]
fn normal_config_still_validates_and_renders_unchanged() {
    // Positive control for the central encoding boundary: a clean config still validates and its
    // golden bytes are intact (the boundary rejects nothing legitimate).
    let config = example();
    config.validate().expect("clean config must validate");
    let rendered = config.render().unwrap();
    assert_eq!(rendered.site_env, GOLDEN_SITE_ENV);
    assert_eq!(rendered.syncd_env, GOLDEN_SYNCD_ENV);
    assert_eq!(rendered.bge_m3_env, GOLDEN_BGE_M3_ENV);
    assert_eq!(rendered.site_unit, GOLDEN_SITE_UNIT);
    assert_eq!(rendered.syncd_unit, GOLDEN_SYNCD_UNIT);
    assert_eq!(rendered.bge_m3_unit, GOLDEN_BGE_M3_UNIT);
}

#[test]
fn non_cls_pooling_is_rejected() {
    let mut config = example();
    config.embedder.pooling = "mean".to_owned();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "embedder.pooling.unsupported")
    );
}

// ---------------------------------------------------------------------------------------------
// Network exposure
// ---------------------------------------------------------------------------------------------

#[test]
fn lan_bind_without_allow_lan_is_rejected() {
    let mut config = example();
    config.site.allow_lan = false;
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "site.bind.lan.not_allowed")
    );
}

#[test]
fn wildcard_bind_requires_wildcard_flag() {
    let mut config = example();
    config.site.bind = "tcp://0.0.0.0:8099".to_owned();
    config.site.allow_lan = true;
    config.site.allow_wildcard_lan = false;
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "site.bind.wildcard.not_allowed")
    );

    config.site.allow_wildcard_lan = true;
    assert!(config.validate().is_ok());
}

#[test]
fn public_bind_is_rejected() {
    let mut config = example();
    config.site.bind = "tcp://8.8.8.8:8099".to_owned();
    config.site.allow_lan = true;
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "site.bind.public")
    );
}

// ---------------------------------------------------------------------------------------------
// Database roles + trust anchors
// ---------------------------------------------------------------------------------------------

#[test]
fn non_distinct_roles_rejected_unless_unsafe_flag() {
    let mut config = example();
    config.database.writer_user = config.database.read_user.clone();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "database.roles.not_distinct")
    );

    config.database.unsafe_single_role = true;
    assert!(config.validate().is_ok());
}

#[test]
fn missing_package_anchor_is_rejected() {
    let mut config = example();
    config
        .trust
        .anchor
        .retain(|anchor| anchor.purpose != TrustPurpose::Package);
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "trust.package_anchor.missing")
    );
}

#[test]
fn configured_license_requires_license_anchor() {
    let mut config = example();
    config
        .trust
        .anchor
        .retain(|anchor| anchor.purpose != TrustPurpose::License);
    assert!(config.license.is_some());
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "trust.license_anchor.missing")
    );
}

#[test]
fn relative_paths_are_rejected() {
    let mut config = example();
    config.sync.source_root = "relative/packages".into();
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "sync.source_root.relative")
    );
}

// ---------------------------------------------------------------------------------------------
// Bind translation
// ---------------------------------------------------------------------------------------------

#[test]
fn bind_translation_tcp_loopback() {
    let bind = parse_bind("tcp://127.0.0.1:8099").unwrap();
    match bind {
        BindAddress::Tcp {
            host_port,
            port,
            exposure,
            ..
        } => {
            assert_eq!(host_port, "127.0.0.1:8099");
            assert_eq!(port, 8099);
            assert_eq!(exposure, TcpExposure::Loopback);
        }
        other => panic!("expected tcp, got {other:?}"),
    }
}

#[test]
fn bind_translation_ipv6_loopback() {
    let bind = parse_bind("tcp://[::1]:8099").unwrap();
    match bind {
        BindAddress::Tcp {
            host_port,
            exposure,
            ..
        } => {
            assert_eq!(host_port, "[::1]:8099");
            assert_eq!(exposure, TcpExposure::Loopback);
        }
        other => panic!("expected tcp, got {other:?}"),
    }
}

#[test]
fn bind_translation_unix_socket() {
    let bind = parse_bind("unix:///run/jurisearch/jurisearch-site.sock").unwrap();
    assert_eq!(
        bind,
        BindAddress::Unix {
            path: "/run/jurisearch/jurisearch-site.sock".to_owned()
        }
    );
}

#[test]
fn bind_unknown_scheme_is_error() {
    assert!(parse_bind("http://127.0.0.1:8099").is_err());
}

#[test]
fn unix_bind_renders_socket_flag_and_no_allow_lan() {
    let mut config = example();
    config.site.bind = "unix:///run/jurisearch/jurisearch-site.sock".to_owned();
    let rendered = config.render().unwrap();
    assert!(
        rendered
            .site_unit
            .contains("--socket ${JURISEARCH_SITE_BIND}")
    );
    assert!(!rendered.site_unit.contains("--allow-lan"));
    assert!(
        rendered
            .site_env
            .contains("JURISEARCH_SITE_BIND=/run/jurisearch/jurisearch-site.sock")
    );
}

#[test]
fn wildcard_bind_renders_both_allow_flags() {
    let mut config = example();
    config.site.bind = "tcp://0.0.0.0:8099".to_owned();
    config.site.allow_lan = true;
    config.site.allow_wildcard_lan = true;
    let rendered = config.render().unwrap();
    assert!(rendered.site_unit.contains("--allow-lan"));
    assert!(rendered.site_unit.contains("--allow-wildcard-lan"));
}

// ---------------------------------------------------------------------------------------------
// Golden rendering
// ---------------------------------------------------------------------------------------------

const GOLDEN_SITE_ENV: &str = "\
# Generated by `jurisearchctl site render` from site.toml. DO NOT EDIT — regenerate instead.
JURISEARCH_SITE_BIND=100.100.20.30:8099
JURISEARCH_SITE_WORKERS=8
JURISEARCH_DB_HOST=127.0.0.1
JURISEARCH_DB_PORT=5432
JURISEARCH_DB_NAME=jurisearch
JURISEARCH_READ_USER=jurisearch_read
JURISEARCH_EMBED_PROVIDER=openai_compatible
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8081
JURISEARCH_EMBED_MODEL=bge-m3
JURISEARCH_EMBED_DIMENSION=1024
JURISEARCH_EMBED_NORMALIZE=true
JURISEARCH_EMBED_POOLING=cls
JURISEARCH_EMBED_TOKENIZER_JSON=/srv/jurisearch/models/bge-m3-tokenizer.json
";

const GOLDEN_SYNCD_ENV: &str = "\
# Generated by `jurisearchctl site render` from site.toml. DO NOT EDIT — regenerate instead.
JURISEARCH_DB_HOST=127.0.0.1
JURISEARCH_DB_PORT=5432
JURISEARCH_DB_NAME=jurisearch
JURISEARCH_WRITER_USER=jurisearch_write
JURISEARCH_READ_USER=jurisearch_read
JURISEARCH_OWNER_ROLE=jurisearch_owner
JURISEARCH_SOURCE_ROOT=/srv/jurisearch/packages
JURISEARCH_INTERVAL_SECS=30
";

const GOLDEN_BGE_M3_ENV: &str = "\
# Generated by `jurisearchctl site render` from site.toml. DO NOT EDIT — regenerate instead.
JURISEARCH_BGE_M3_MODEL=/srv/jurisearch/models/bge-m3-Q8_0.gguf
JURISEARCH_BGE_M3_PORT=8081
JURISEARCH_BGE_M3_POOLING=cls
";

const GOLDEN_SITE_UNIT: &str = "\
# Generated by `jurisearchctl site render` from site.toml. DO NOT EDIT — regenerate instead.
# JuriSearch site query service (read-only; versioned site protocol).
[Unit]
Description=JuriSearch site query service (read-only; versioned site protocol)
Documentation=https://github.com/pnocera/jurisearch
After=network-online.target postgresql.service jurisearch-bge-m3.service
Wants=network-online.target jurisearch-bge-m3.service

[Service]
Type=simple
User=jurisearch
Group=jurisearch
EnvironmentFile=/etc/jurisearch/generated/site.env
ExecStart=/usr/local/bin/jurisearch serve-site \\
  --tcp ${JURISEARCH_SITE_BIND} \\
  --allow-lan \\
  --db-host ${JURISEARCH_DB_HOST} \\
  --db-port ${JURISEARCH_DB_PORT} \\
  --db-name ${JURISEARCH_DB_NAME} \\
  --db-user ${JURISEARCH_READ_USER} \\
  --workers ${JURISEARCH_SITE_WORKERS}
Restart=on-failure
RestartSec=5
KillSignal=SIGTERM
TimeoutStopSec=30
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
RuntimeDirectory=jurisearch

[Install]
WantedBy=multi-user.target
";

const GOLDEN_SYNCD_UNIT: &str = "\
# Generated by `jurisearchctl site render` from site.toml. DO NOT EDIT — regenerate instead.
# JuriSearch syncd (consumer corpus sync daemon).
[Unit]
Description=JuriSearch syncd (consumer corpus sync daemon)
Documentation=https://github.com/pnocera/jurisearch
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=jurisearch
Group=jurisearch
EnvironmentFile=/etc/jurisearch/generated/syncd.env
ExecStart=/usr/local/bin/jurisearch-syncd \\
  --server-host ${JURISEARCH_DB_HOST} \\
  --server-port ${JURISEARCH_DB_PORT} \\
  --server-db ${JURISEARCH_DB_NAME} \\
  --writer-user ${JURISEARCH_WRITER_USER} \\
  --read-role ${JURISEARCH_READ_USER} \\
  --owner-role ${JURISEARCH_OWNER_ROLE} \\
  run \\
  --corpus core \\
  --source-root ${JURISEARCH_SOURCE_ROOT} \\
  --interval-secs ${JURISEARCH_INTERVAL_SECS}
KillSignal=SIGTERM
TimeoutStopSec=120
Restart=on-failure
RestartSec=10
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadOnlyPaths=/srv/jurisearch/packages

[Install]
WantedBy=multi-user.target
";

const GOLDEN_BGE_M3_UNIT: &str = "\
# Generated by `jurisearchctl site render` from site.toml. DO NOT EDIT — regenerate instead.
# JuriSearch local embedding endpoint (bge-m3 via llama.cpp).
[Unit]
Description=JuriSearch local embedding endpoint (bge-m3 via llama.cpp)
Documentation=https://github.com/pnocera/jurisearch
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=jurisearch
Group=jurisearch
EnvironmentFile=/etc/jurisearch/generated/bge-m3.env
ExecStart=/usr/local/bin/llama-server \\
  --model ${JURISEARCH_BGE_M3_MODEL} \\
  --embeddings \\
  --pooling ${JURISEARCH_BGE_M3_POOLING} \\
  --host 127.0.0.1 \\
  --port ${JURISEARCH_BGE_M3_PORT}
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
";

#[test]
fn golden_env_files() {
    let rendered = example().render().unwrap();
    assert_eq!(rendered.site_env, GOLDEN_SITE_ENV);
    assert_eq!(rendered.syncd_env, GOLDEN_SYNCD_ENV);
    assert_eq!(rendered.bge_m3_env, GOLDEN_BGE_M3_ENV);
}

#[test]
fn golden_unit_files() {
    let rendered = example().render().unwrap();
    assert_eq!(rendered.site_unit, GOLDEN_SITE_UNIT);
    assert_eq!(rendered.syncd_unit, GOLDEN_SYNCD_UNIT);
    assert_eq!(rendered.bge_m3_unit, GOLDEN_BGE_M3_UNIT);
}

#[test]
fn rendering_is_deterministic() {
    let config = example();
    assert_eq!(config.render().unwrap(), config.render().unwrap());
}

#[test]
fn generated_units_use_absolute_paths_only() {
    let rendered = example().render().unwrap();
    // ReadOnlyPaths and EnvironmentFile are absolute literals, never env-expanded.
    assert!(
        rendered
            .syncd_unit
            .contains("ReadOnlyPaths=/srv/jurisearch/packages")
    );
    assert!(!rendered.syncd_unit.contains("ReadOnlyPaths=${"));
    assert!(
        rendered
            .site_unit
            .contains("EnvironmentFile=/etc/jurisearch/generated/site.env")
    );
    assert!(!rendered.site_unit.contains("EnvironmentFile=${"));
}

// ---------------------------------------------------------------------------------------------
// Multi-corpus rendering
// ---------------------------------------------------------------------------------------------

#[test]
fn multiple_corpora_render_one_flag_each() {
    let mut config = example();
    config.sync.corpora = vec!["core".to_owned(), "inpi".to_owned()];
    let rendered = config.render().unwrap();
    assert!(rendered.syncd_unit.contains("--corpus core"));
    assert!(rendered.syncd_unit.contains("--corpus inpi"));
}

// ---------------------------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------------------------

#[test]
fn redact_returns_placeholder() {
    assert_eq!(redact("hunter2"), "[redacted]");
}

#[test]
fn secret_string_does_not_leak_in_debug_or_display() {
    let secret = SecretString::new("super-secret-token");
    assert_eq!(format!("{secret}"), "[redacted]");
    assert_eq!(format!("{secret:?}"), "SecretString([redacted])");
    assert!(!format!("{secret:?}").contains("super-secret-token"));
    // expose() is the only path to the raw bytes.
    assert_eq!(secret.expose(), "super-secret-token");
}

// ---------------------------------------------------------------------------------------------
// Secret-file permissions
// ---------------------------------------------------------------------------------------------

#[test]
fn write_secret_file_is_0600() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.env");
    secret::write_secret_file(&path, b"PASSWORD=abc\n").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o600, "got {mode:o}");
    assert!(!secret::is_world_or_group_accessible(&path).unwrap());
}

#[test]
fn write_unit_file_is_0644() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unit.service");
    secret::write_file_with_mode(&path, b"[Unit]\n", 0o644).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o644, "got {mode:o}");
    assert!(secret::is_world_or_group_accessible(&path).unwrap());
}

#[test]
fn rendered_site_writes_env_0600_and_units_0644() {
    let dir = tempfile::tempdir().unwrap();
    let rendered = example().render().unwrap();
    rendered.write_to(dir.path()).unwrap();
    for file in rendered.files() {
        let path = dir.path().join(file.relative_path);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        let expected = if file.secret { 0o600 } else { 0o644 };
        assert_eq!(mode, expected, "{}: got {mode:o}", file.relative_path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), file.contents);
    }
}

// ---------------------------------------------------------------------------------------------
// Install layout: units land where `systemctl enable <unit>` resolves them; env stays under config
// ---------------------------------------------------------------------------------------------

#[test]
fn install_places_units_in_the_systemd_unit_dir_and_env_under_config_dir() {
    // `site install` reports "install units <names> into <systemd_unit_dir>" and "render env into
    // <config_dir>/generated"; this proves those reported locations match what install actually does,
    // and that the units are written by BARE name (no `systemd/` prefix) so `systemctl enable
    // jurisearch-*.service` resolves them.
    let unit_root = tempfile::tempdir().unwrap();
    let config_root = tempfile::tempdir().unwrap();
    let rendered = example().render().unwrap();

    let written = rendered.install_units(unit_root.path()).unwrap();
    rendered.write_env_files(config_root.path()).unwrap();

    // Units: exactly the three managed units, by bare name, at 0644, in the systemd unit dir.
    let expected_units = [
        "jurisearch-bge-m3.service",
        "jurisearch-syncd.service",
        "jurisearch-site.service",
    ];
    for unit in expected_units {
        let path = unit_root.path().join(unit);
        assert!(
            path.is_file(),
            "unit must be installed at {}",
            path.display()
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o644, "{unit}: got {mode:o}");
    }
    assert_eq!(written.len(), expected_units.len());
    // No `systemd/` subdir under the unit dir — units are bare names systemctl can find.
    assert!(!unit_root.path().join("systemd").exists());

    // Env: only the generated/*.env files under config_dir (0600); no units written there.
    for env in ["site.env", "syncd.env", "bge-m3.env"] {
        let path = config_root.path().join("generated").join(env);
        assert!(path.is_file(), "env must be at {}", path.display());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600, "{env}: got {mode:o}");
    }
    assert!(
        !config_root.path().join("systemd").exists(),
        "install must NOT leave units under config_dir/systemd"
    );
}

// ---------------------------------------------------------------------------------------------
// Password-file permission validation
// ---------------------------------------------------------------------------------------------

#[test]
fn world_readable_password_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pgpass");
    std::fs::write(&path, b"secret").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

    let mut config = example();
    config.database.admin_password_file = Some(path);
    let errors = config.validate().unwrap_err();
    assert!(
        errors
            .diagnostics
            .iter()
            .any(|d| d.code == "database.password_file.world_readable"),
        "got {errors}"
    );
}

#[test]
fn owner_only_password_file_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pgpass");
    secret::write_secret_file(&path, b"secret").unwrap();

    let mut config = example();
    config.database.admin_password_file = Some(path);
    assert!(config.validate().is_ok());
}

// ---------------------------------------------------------------------------------------------
// Scaffolding
// ---------------------------------------------------------------------------------------------

#[test]
fn init_writes_template_then_preserves_existing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("site.toml");

    assert_eq!(init_site_config(&path).unwrap(), InitOutcome::Created);
    assert!(path.exists());
    // The scaffolded template must itself parse + validate.
    SiteConfig::load(&path).expect("scaffolded template must load");

    // A second init does not clobber.
    std::fs::write(&path, "# operator edits\n").ok();
    assert_eq!(init_site_config(&path).unwrap(), InitOutcome::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "# operator edits\n"
    );
}
