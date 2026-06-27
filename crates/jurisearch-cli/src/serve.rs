//! `serve` daemon transport: bind a TCP or Unix socket and run the JSONL request loop
//! (`serve_jsonl`) over each connection, delegating to `session::dispatch_session_request`
//! so socket results match the one-shot CLI. Single-client, sequential (the index holds an
//! advisory lock), with bounded request lines and idle read/write timeouts.

use std::fs;
use std::io::{self, BufRead, Write};
use std::net::ToSocketAddrs;
use std::path::Path;

use serde_json::{Value, json};

use jurisearch_core::error::ErrorObject;
use jurisearch_core::session::SessionResponse;
use jurisearch_transport::{
    MAX_LINE_BYTES, TransportError, decode_bare_request_line, encode_bare_response_line,
    read_bounded_line,
};

use crate::args::ServeArgs;
use crate::output::emit_error;
use crate::session::dispatch_session_request;

/// Inject the server's bound index dir into a request that doesn't specify one, so clients of a
/// daemon bound to one index can omit `index_dir`.
pub(crate) fn inject_server_index_dir(args: &mut Value, default_index_dir: &Option<String>) {
    let Some(dir) = default_index_dir else {
        return;
    };
    if !args.is_object() {
        *args = json!({});
    }
    if let Some(map) = args.as_object_mut() {
        map.entry("index_dir")
            .or_insert_with(|| Value::String(dir.clone()));
    }
}

/// Serve the JSONL request protocol over one socket, sequentially (the index's advisory lock means
/// one request holds the index at a time). Reuses `dispatch_session_request` — the same transport-
/// neutral handler the warm session uses — so results are byte-identical to the one-shot CLI.
pub(crate) fn serve_jsonl<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    default_index_dir: &Option<String>,
) -> io::Result<()> {
    loop {
        let line = match read_bounded_line(&mut reader, MAX_LINE_BYTES) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            // An oversize line: reply (same bytes as the legacy framing message) and close so the
            // listener can accept the next client.
            Err(TransportError::Oversize) => {
                let response = SessionResponse::err(
                    None,
                    ErrorObject::bad_input(TransportError::Oversize.to_string()),
                );
                let _ = write!(writer, "{}", encode_bare_response_line(&response));
                break;
            }
            // A genuine read failure propagates as before.
            Err(TransportError::Io(error)) => return Err(error),
            // `read_bounded_line` yields only `Oversize`/`Io`; handle anything else defensively.
            Err(other) => {
                let response =
                    SessionResponse::err(None, ErrorObject::bad_input(other.to_string()));
                let _ = write!(writer, "{}", encode_bare_response_line(&response));
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // The local `serve` surface speaks the BARE (version-free) frame — its legacy compatibility
        // with existing agent workflows is preserved. The versioned site envelope is a separate
        // codec entry point used only by the site service (work/09 P4).
        let (response, should_exit) = match decode_bare_request_line(&line) {
            Ok(mut request) => {
                inject_server_index_dir(&mut request.args, default_index_dir);
                dispatch_session_request(request)
            }
            Err(TransportError::Malformed(inner)) => (
                SessionResponse::err(
                    None,
                    ErrorObject::bad_input(format!("malformed request: {inner}")),
                ),
                false,
            ),
            Err(error) => (
                SessionResponse::err(None, ErrorObject::bad_input(error.to_string())),
                false,
            ),
        };
        write!(writer, "{}", encode_bare_response_line(&response))?;
        writer.flush()?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

pub(crate) fn run_serve(args: ServeArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    let default_index_dir = index_dir.map(|path| path.display().to_string());
    match (args.tcp.as_deref(), args.socket.as_deref()) {
        (Some(_), Some(_)) | (None, None) => emit_error(ErrorObject::bad_input(
            "serve requires exactly one of --tcp or --socket",
        )),
        (Some(addr), None) => {
            // Resolve and refuse a non-loopback bind unless explicitly allowed: the protocol is
            // unauthenticated, so binding 0.0.0.0/a LAN address would expose the index off-host.
            let resolved = addr
                .to_socket_addrs()
                .map_err(|error| anyhow::anyhow!("invalid --tcp address {addr}: {error}"))?
                .next()
                .ok_or_else(|| anyhow::anyhow!("--tcp address {addr} did not resolve"))?;
            if !resolved.ip().is_loopback() && !args.allow_remote {
                return emit_error(ErrorObject::bad_input(format!(
                    "refusing to bind non-loopback address {resolved} without --allow-remote (the protocol is unauthenticated)"
                )));
            }
            let listener = std::net::TcpListener::bind(resolved)
                .map_err(|error| anyhow::anyhow!("failed to bind TCP {resolved}: {error}"))?;
            eprintln!(
                "jurisearch serve: listening on tcp://{resolved} (JSONL session protocol; single-client sequential)"
            );
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // Drop a slow/idle client instead of holding the single-client daemon forever.
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
                let reader = io::BufReader::new(stream.try_clone()?);
                let _ = serve_jsonl(reader, stream, &default_index_dir);
            }
            Ok(())
        }
        (None, Some(path)) => {
            use std::os::unix::fs::FileTypeExt;
            use std::os::unix::net::{UnixListener, UnixStream};
            // Only remove a CONFIRMED stale jurisearch socket — never a regular file/dir/symlink the
            // user mistyped, and not a live server's socket.
            if let Ok(meta) = fs::symlink_metadata(path) {
                if !meta.file_type().is_socket() {
                    return emit_error(ErrorObject::bad_input(format!(
                        "refusing to bind: {} exists and is not a socket",
                        path.display()
                    )));
                }
                if UnixStream::connect(path).is_ok() {
                    return emit_error(ErrorObject::bad_input(format!(
                        "a server is already listening on {}",
                        path.display()
                    )));
                }
                fs::remove_file(path).map_err(|error| {
                    anyhow::anyhow!("failed to remove stale socket {}: {error}", path.display())
                })?;
            }
            let listener = UnixListener::bind(path).map_err(|error| {
                anyhow::anyhow!("failed to bind socket {}: {error}", path.display())
            })?;
            eprintln!(
                "jurisearch serve: listening on unix://{} (JSONL session protocol; single-client sequential)",
                path.display()
            );
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // Match the TCP path: a read timeout drops a slow/idle client, and a write timeout
                // stops a client that sends a request then never drains the response from blocking
                // the single-client daemon.
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
                let reader = io::BufReader::new(stream.try_clone()?);
                let _ = serve_jsonl(reader, stream, &default_index_dir);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::serve_jsonl;

    /// Drive `serve_jsonl` over in-memory buffers and return the exact bytes it wrote to the socket.
    fn serve(input: &str) -> String {
        let mut output: Vec<u8> = Vec::new();
        serve_jsonl(input.as_bytes(), &mut output, &None).expect("serve_jsonl");
        String::from_utf8(output).expect("utf8 output")
    }

    #[test]
    fn serve_jsonl_exit_writes_exact_compact_bytes() {
        // Raw socket bytes: compact JSON, field order id/ok/result, exactly one trailing newline.
        let out = serve(
            r#"{"id":"x","command":"exit"}
"#,
        );
        assert_eq!(
            out,
            "{\"id\":\"x\",\"ok\":true,\"result\":{\"bye\":true}}\n"
        );
    }

    #[test]
    fn serve_jsonl_malformed_then_valid_emits_exact_bytes() {
        let out = serve("not json\n{\"id\":\"y\",\"command\":\"exit\"}\n");
        // Full socket bytes: the local serve malformed prefix is `malformed request: ` (NOT the
        // session path's `malformed JSONL request: ` and NOT doubled with the codec's own frame
        // text), the serde detail + `suggestions` are preserved, then the valid `exit` is acked.
        let malformed = r#"{"ok":false,"error":{"code":"bad_input","message":"malformed request: expected ident at line 1 column 2","suggestions":["Run `jurisearch help agent` for accepted commands and flags."]}}"#;
        let exit_ack = r#"{"id":"y","ok":true,"result":{"bye":true}}"#;
        assert_eq!(out, format!("{malformed}\n{exit_ack}\n"));
    }
}
