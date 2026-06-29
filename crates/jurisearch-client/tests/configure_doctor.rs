//! M5-A — acceptance for the RESERVED client-local verbs on the SHIPPED binary: `configure` persists a
//! site URL to an XDG `client.toml`, and `doctor` then resolves that persisted URL (no flag/env) and runs
//! a live `status` handshake. Drives the real binary with an isolated `$XDG_CONFIG_HOME` tempdir and a
//! loopback site server, asserting exit codes + diagnostics. Does NOT require a live production site.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::ffi::OsStrExt;
use std::process::Command;
use std::thread::{self, JoinHandle};

use jurisearch_core::envelope::ProtocolResponseEnvelope;
use jurisearch_core::session::SessionResponse;
use jurisearch_transport::encode_site_response_envelope_line;
use serde_json::json;

/// Bind a loopback TCP server that accepts ONE connection, consumes the request line, and replies with a
/// single VERSIONED `status` OK envelope. Returns the bound address + the server thread.
fn serve_one_status_ok() -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            let response = SessionResponse::ok(
                Some(json!("jurisearch-client-doctor")),
                json!({ "service": "jurisearch-site", "active_corpora": [] }),
            );
            let reply =
                encode_site_response_envelope_line(&ProtocolResponseEnvelope::new(response));
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });
    (addr, handle)
}

/// A client `Command` with an ISOLATED config home and no inherited site-URL env, so each test sees a
/// clean resolution chain.
fn client_in(config_home: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_jurisearch-client"));
    cmd.env("XDG_CONFIG_HOME", config_home)
        .env_remove("JURISEARCH_SITE_URL");
    cmd
}

#[test]
fn configure_persists_a_url_that_doctor_then_resolves_and_probes() {
    let home = tempfile::tempdir().expect("tempdir");
    let (addr, server) = serve_one_status_ok();
    let url = format!("tcp://{addr}");

    // 1) configure writes the XDG config (exit 0) and reports the path.
    let configured = client_in(home.path())
        .args(["configure", "--server", &url])
        .output()
        .expect("run configure");
    assert!(
        configured.status.success(),
        "configure should exit 0; stderr={}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let config_file = home.path().join("jurisearch").join("client.toml");
    assert!(config_file.exists(), "configure must write {config_file:?}");

    // 2) doctor resolves the PERSISTED url (no --server, no env) and the live status handshake is green.
    let doctored = client_in(home.path())
        .arg("doctor")
        .output()
        .expect("run doctor");
    server.join().expect("server");
    assert!(
        doctored.status.success(),
        "doctor should exit 0 against a healthy configured server; stdout={} stderr={}",
        String::from_utf8_lossy(&doctored.stdout),
        String::from_utf8_lossy(&doctored.stderr)
    );
    let stdout = String::from_utf8(doctored.stdout).unwrap();
    assert!(stdout.contains("all checks passed"), "report:\n{stdout}");
    assert!(
        stdout.contains(&url),
        "the report names the configured endpoint:\n{stdout}"
    );
}

#[test]
fn doctor_without_config_or_selection_fails_with_an_actionable_diagnostic() {
    let home = tempfile::tempdir().expect("tempdir");
    let output = client_in(home.path())
        .arg("doctor")
        .output()
        .expect("run doctor");
    assert_eq!(output.status.code(), Some(2), "no config/flag/env → exit 2");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("configure"),
        "doctor must tell the user to run configure:\n{stdout}"
    );
}

#[test]
fn doctor_reports_an_unreachable_configured_server() {
    let home = tempfile::tempdir().expect("tempdir");
    // Configure a well-formed but closed endpoint (port 1), then doctor must FAIL the handshake leg.
    let configured = client_in(home.path())
        .args(["configure", "--server", "tcp://127.0.0.1:1"])
        .output()
        .expect("run configure");
    assert!(configured.status.success());

    let output = client_in(home.path())
        .arg("doctor")
        .output()
        .expect("run doctor");
    assert_eq!(output.status.code(), Some(2), "unreachable server → exit 2");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("cannot reach"),
        "doctor names the unreachable service:\n{stdout}"
    );
}

/// Write a MALFORMED `client.toml` under the isolated XDG home, so the forward path would error if it
/// (wrongly) consulted the config when a higher-priority selector is present.
fn write_malformed_config(config_home: &std::path::Path) {
    let dir = config_home.join("jurisearch");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("client.toml"), "this is not = valid = toml").expect("write config");
}

#[test]
fn an_explicit_server_forwards_despite_a_malformed_low_priority_config() {
    // A stale/malformed persisted config must NOT make an explicit `--server` forward fail (regression).
    let home = tempfile::tempdir().expect("tempdir");
    write_malformed_config(home.path());
    let (addr, server) = serve_one_status_ok();
    let url = format!("tcp://{addr}");

    let output = client_in(home.path())
        .args(["--server", &url, "status"])
        .output()
        .expect("run client");
    server.join().expect("server");
    assert!(
        output.status.success(),
        "an explicit --server must outrank (and not even read) a malformed config; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn an_env_url_forwards_despite_a_malformed_low_priority_config() {
    // `$JURISEARCH_SITE_URL` outranks the config too: a malformed file must not block the env endpoint.
    let home = tempfile::tempdir().expect("tempdir");
    write_malformed_config(home.path());
    let (addr, server) = serve_one_status_ok();
    let url = format!("tcp://{addr}");

    let output = client_in(home.path())
        .env("JURISEARCH_SITE_URL", &url)
        .arg("status")
        .output()
        .expect("run client");
    server.join().expect("server");
    assert!(
        output.status.success(),
        "$JURISEARCH_SITE_URL must outrank (and not even read) a malformed config; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_malformed_env_url_is_an_endpoint_error_and_never_falls_through_to_config() {
    // A PRESENT-but-malformed env var is an endpoint error — it must NOT silently fall through to a
    // (here perfectly valid) persisted config. Persist a good URL, then set a bad env var.
    let home = tempfile::tempdir().expect("tempdir");
    let configured = client_in(home.path())
        .args(["configure", "--server", "tcp://127.0.0.1:9"])
        .output()
        .expect("run configure");
    assert!(configured.status.success());

    let output = client_in(home.path())
        .env("JURISEARCH_SITE_URL", "tcp://localhost") // missing :port
        .arg("status")
        .output()
        .expect("run client");
    assert_eq!(
        output.status.code(),
        Some(2),
        "malformed env → endpoint error"
    );
    assert!(
        String::from_utf8(output.stderr).unwrap().contains(":port"),
        "the malformed env var is the error, not a fall-through to config"
    );
}

#[test]
fn doctor_treats_a_missing_config_as_advisory_when_env_resolves_the_endpoint() {
    // `$JURISEARCH_SITE_URL` set + NO config file → the missing config is ADVISORY, and a green handshake
    // exits 0 (it must not FAIL solely because no file was persisted).
    let home = tempfile::tempdir().expect("tempdir");
    let (addr, server) = serve_one_status_ok();
    let url = format!("tcp://{addr}");

    let output = client_in(home.path())
        .env("JURISEARCH_SITE_URL", &url)
        .arg("doctor")
        .output()
        .expect("run doctor");
    server.join().expect("server");
    assert!(
        output.status.success(),
        "env-resolved doctor with a green handshake must exit 0; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("all checks passed"), "report:\n{stdout}");
    // The missing-config line is advisory ([ok ]), not a [FAIL].
    assert!(
        !stdout.contains("[FAIL]"),
        "missing config must not FAIL when env resolves:\n{stdout}"
    );
}

#[test]
fn doctor_with_no_config_directory_and_env_endpoint_is_advisory_and_exits_zero() {
    // WARN 1 — when NO config directory resolves (neither $XDG_CONFIG_HOME nor $HOME) but
    // `$JURISEARCH_SITE_URL` selects the endpoint and the handshake is green, the missing-config leg is
    // ADVISORY (not `[FAIL]`) and doctor exits 0. The endpoint must never depend on config-home presence.
    let (addr, server) = serve_one_status_ok();
    let url = format!("tcp://{addr}");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_jurisearch-client"));
    cmd.env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env("JURISEARCH_SITE_URL", &url)
        .arg("doctor");
    let output = cmd.output().expect("run doctor");
    server.join().expect("server");
    assert!(
        output.status.success(),
        "no config dir + env endpoint + green handshake must exit 0; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("all checks passed"), "report:\n{stdout}");
    assert!(
        !stdout.contains("[FAIL]"),
        "a missing config directory must be advisory when env resolves:\n{stdout}"
    );
}

#[test]
fn a_non_unicode_env_url_is_an_endpoint_error_and_never_falls_through_to_config_forward() {
    // WARN 2 (forward path) — a PRESENT but non-Unicode `$JURISEARCH_SITE_URL` is an endpoint error and
    // must NOT fall through to a perfectly valid persisted `client.toml`. Persist a good URL, then set a
    // non-UTF-8 env var.
    let home = tempfile::tempdir().expect("tempdir");
    let configured = client_in(home.path())
        .args(["configure", "--server", "tcp://127.0.0.1:9"])
        .output()
        .expect("run configure");
    assert!(configured.status.success());

    // A non-UTF-8 env value (Unix): raw bytes that are not valid UTF-8.
    let bad = OsStr::from_bytes(b"tcp://\xff\xfe:8099");
    let output = client_in(home.path())
        .env("JURISEARCH_SITE_URL", bad)
        .arg("status")
        .output()
        .expect("run client");
    assert_eq!(
        output.status.code(),
        Some(2),
        "non-Unicode env → endpoint error (no fall-through to config)"
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("not valid UTF-8"),
        "the non-Unicode env var is the error, not a fall-through to config"
    );
}

#[test]
fn a_non_unicode_env_url_is_an_endpoint_error_and_never_falls_through_to_config_doctor() {
    // WARN 2 (doctor path) — doctor is handed a valid loaded config, yet a PRESENT non-Unicode
    // `$JURISEARCH_SITE_URL` must still be an endpoint error (exit 2), never using the configured URL.
    let home = tempfile::tempdir().expect("tempdir");
    let configured = client_in(home.path())
        .args(["configure", "--server", "tcp://127.0.0.1:9"])
        .output()
        .expect("run configure");
    assert!(configured.status.success());

    let bad = OsStr::from_bytes(b"tcp://\xff\xfe:8099");
    let output = client_in(home.path())
        .env("JURISEARCH_SITE_URL", bad)
        .arg("doctor")
        .output()
        .expect("run doctor");
    assert_eq!(
        output.status.code(),
        Some(2),
        "non-Unicode env in doctor → endpoint error (no fall-through to config)"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("not valid UTF-8"),
        "doctor must report the malformed env var, not resolve the configured URL:\n{stdout}"
    );
    // The endpoint-resolution leg must FAIL on the env var, never fall through to the configured URL
    // (which would otherwise print a green `resolved site endpoint tcp://127.0.0.1:9`).
    assert!(
        !stdout.contains("resolved site endpoint"),
        "doctor must NOT fall through to the configured endpoint:\n{stdout}"
    );
}

#[test]
fn configure_rejects_a_malformed_url_before_persisting() {
    let home = tempfile::tempdir().expect("tempdir");
    let output = client_in(home.path())
        .args(["configure", "--server", "tcp://localhost"]) // missing :port
        .output()
        .expect("run configure");
    assert_eq!(output.status.code(), Some(2), "bad URL → exit 2");
    assert!(
        String::from_utf8(output.stderr).unwrap().contains(":port"),
        "the diagnostic explains the missing port"
    );
    assert!(
        !home.path().join("jurisearch").join("client.toml").exists(),
        "a rejected URL must not be persisted"
    );
}
