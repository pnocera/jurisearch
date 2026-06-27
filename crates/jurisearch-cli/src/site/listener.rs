//! work/09 P4 (4A) — the site listener: a UDS/loopback accept loop that frames versioned site requests,
//! dispatches them through the [`SiteDispatcher`], and writes back compact `SessionResponse` JSONL. The
//! skeleton serves connections sequentially over ONE pooled read-role connection (the bounded worker and
//! read pools arrive with 4B). Decoding — and its loud rejection of unversioned/skewed frames — happens
//! BEFORE dispatch, so a malformed frame never reaches a handler.

use std::io::{BufRead, Write};

use jurisearch_core::error::ErrorObject;
use jurisearch_core::session::SessionResponse;
use jurisearch_transport::{
    decode_site_envelope_line, encode_bare_response_line, read_bounded_line,
};

use super::dispatcher::{ServerContext, SiteDispatcher};

/// The maximum site request line, matching the local session loop's bound.
pub(crate) const MAX_SITE_LINE_BYTES: usize = 1 << 20;

/// Serve one site connection to completion: read versioned request lines, dispatch each, write one
/// response line per request. A framing/version error yields a single error response (the request id is
/// not recoverable from an undecodable frame), and an oversize/read error ends the connection. Returns
/// when the peer hangs up (EOF).
pub(crate) fn serve_site_connection<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    dispatcher: &SiteDispatcher,
    ctx: &ServerContext,
) -> std::io::Result<()> {
    loop {
        match read_bounded_line(&mut reader, MAX_SITE_LINE_BYTES) {
            Ok(None) => break, // EOF — peer closed.
            Ok(Some(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match decode_site_envelope_line(trimmed) {
                    Ok(envelope) => {
                        let response = dispatcher.dispatch(ctx, &envelope.request);
                        writer.write_all(encode_bare_response_line(&response).as_bytes())?;
                        writer.flush()?;
                    }
                    // Framing-level failure (unversioned/skewed/malformed): the frame never reaches the
                    // dispatcher and the id is not recoverable, so we write ONE null-id error response and
                    // CLOSE the connection — a frame after a framing failure is never served (the stream
                    // is no longer trustworthy).
                    Err(transport_error) => {
                        let response = SessionResponse::err(
                            None,
                            ErrorObject::bad_input(format!(
                                "malformed or unversioned site request frame: {transport_error}"
                            )),
                        );
                        let _ = writer.write_all(encode_bare_response_line(&response).as_bytes());
                        let _ = writer.flush();
                        break;
                    }
                }
            }
            Err(transport_error) => {
                // Oversize line or read failure: report once, then end the connection.
                let response = SessionResponse::err(
                    None,
                    ErrorObject::bad_input(format!("site request frame error: {transport_error}")),
                );
                let _ = writer.write_all(encode_bare_response_line(&response).as_bytes());
                let _ = writer.flush();
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_core::envelope::ProtocolEnvelope;
    use jurisearch_core::operation::Operation;
    use jurisearch_core::session::SessionRequest;
    use jurisearch_storage::query::{QueryStore, ReadSnapshot};
    use jurisearch_storage::runtime::StorageError;
    use jurisearch_transport::{encode_bare_request_line, encode_site_envelope_line};
    use serde_json::{Value, json};
    use std::io::Cursor;

    use super::super::dispatcher::OperationHandler;

    struct UnusedStore;
    impl QueryStore for UnusedStore {
        fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, StorageError> {
            panic!("no request should be dispatched in this test");
        }
    }
    struct PanicHandler;
    impl OperationHandler for PanicHandler {
        fn handle(&self, _ctx: &ServerContext, _args: &Value) -> Result<Value, ErrorObject> {
            panic!("a frame following a framing failure must NOT be dispatched");
        }
    }

    /// work/09 P4 (4A): a framing failure CLOSES the connection — a valid versioned frame sent AFTER an
    /// unversioned one is never read or dispatched, and exactly one (error) response line is written.
    #[test]
    fn a_framing_failure_closes_the_connection() {
        let mut dispatcher = SiteDispatcher::new();
        dispatcher.register(Operation::Fetch, Box::new(PanicHandler));
        let ctx = ServerContext {
            store: &UnusedStore,
        };

        // Line 1: an UNVERSIONED bare frame (a framing failure). Line 2: a valid versioned `fetch` that
        // must never be served because the connection closes after line 1.
        let unversioned = encode_bare_request_line(&SessionRequest {
            id: Some(json!("a")),
            command: "fetch".to_owned(),
            args: json!({"ids": ["x"]}),
        });
        let versioned = encode_site_envelope_line(&ProtocolEnvelope::new(SessionRequest {
            id: Some(json!("b")),
            command: "fetch".to_owned(),
            args: json!({"ids": ["x"]}),
        }));
        let input = format!("{}\n{}\n", unversioned.trim(), versioned.trim());

        let mut output: Vec<u8> = Vec::new();
        serve_site_connection(Cursor::new(input), &mut output, &dispatcher, &ctx)
            .expect("serve completes without panicking on the second frame");

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "exactly one (error) line is written: {text}"
        );
        let response: SessionResponse = serde_json::from_str(lines[0]).unwrap();
        assert!(!response.is_ok(), "the single line is the framing error");
    }
}
