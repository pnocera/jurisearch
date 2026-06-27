//! work/09 P6 — acceptance for the SHIPPED `jurisearch-client` BINARY (not just the library): it speaks
//! the versioned site protocol against a real TCP server and the user-facing CLI contract holds — clap
//! `--server` + positional `command`/JSON-args parsing, one-shot-parity stdout rendering, stderr
//! diagnostics, and exit codes (0 = served OK, 1 = served error, 2 = client/usage failure).

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::thread::{self, JoinHandle};

use jurisearch_core::envelope::ProtocolResponseEnvelope;
use jurisearch_core::error::ErrorObject;
use jurisearch_core::session::SessionResponse;
use jurisearch_render::render_value_pretty;
use jurisearch_transport::encode_site_response_envelope_line;
use serde_json::json;

/// Bind a loopback TCP server that accepts ONE connection, consumes the request line, and replies with
/// one VERSIONED site response envelope. Returns the bound address + the server thread.
fn serve_one_versioned(response: SessionResponse) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request_line = String::new();
            let _ = reader.read_line(&mut request_line);
            let reply =
                encode_site_response_envelope_line(&ProtocolResponseEnvelope::new(response));
            let _ = stream.write_all(reply.as_bytes());
            let _ = stream.flush();
        }
    });
    (addr, handle)
}

fn client() -> Command {
    Command::new(env!("CARGO_BIN_EXE_jurisearch-client"))
}

#[test]
fn the_binary_renders_a_served_ok_response_to_stdout_and_exits_zero() {
    let result_body = json!({ "service": "jurisearch-site", "active_corpora": [] });
    let (addr, server) = serve_one_versioned(SessionResponse::ok(
        Some(json!("jurisearch-client")),
        result_body.clone(),
    ));
    let output = client()
        .args(["--server", &format!("tcp://{addr}"), "status"])
        .output()
        .expect("run client");
    server.join().expect("server");

    assert!(
        output.status.success(),
        "served OK → exit 0; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    // stdout is BYTE-IDENTICAL to the one-shot CLI's render of the result body.
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        render_value_pretty(&result_body).unwrap()
    );
}

#[test]
fn the_binary_renders_a_served_error_and_exits_one() {
    let make_error = || ErrorObject::bad_input("the document id does not exist");
    let (addr, server) = serve_one_versioned(SessionResponse::err(
        Some(json!("jurisearch-client")),
        make_error(),
    ));
    let output = client()
        .args([
            "--server",
            &format!("tcp://{addr}"),
            "fetch",
            r#"{"ids":["missing:doc"]}"#,
        ])
        .output()
        .expect("run client");
    server.join().expect("server");

    // A SERVED error response is rendered (one-shot parity) and reported via a non-zero exit.
    assert_eq!(output.status.code(), Some(1), "served error → exit 1");
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        render_value_pretty(&json!({ "ok": false, "error": make_error() })).unwrap()
    );
}

#[test]
fn the_binary_reports_an_unreachable_service_on_stderr_and_exits_two() {
    // Port 1 is reserved/closed → connection refused (no server thread).
    let output = client()
        .args(["--server", "tcp://127.0.0.1:1", "status"])
        .output()
        .expect("run client");
    assert_eq!(output.status.code(), Some(2), "client failure → exit 2");
    assert!(
        output.stdout.is_empty(),
        "no result is rendered on a client failure"
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot reach"),
        "the diagnostic names the unreachable service"
    );
}

#[test]
fn the_binary_rejects_a_malformed_url_with_a_clear_diagnostic() {
    let output = client()
        .args(["--server", "tcp://localhost", "status"]) // missing :port
        .output()
        .expect("run client");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr).unwrap().contains(":port"),
        "the diagnostic explains the missing port"
    );
}
