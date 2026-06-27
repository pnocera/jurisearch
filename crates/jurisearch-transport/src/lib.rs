//! JSONL line codec for the session protocol — the single **dependency-light** framing authority
//! shared by the local `serve` loop and the (future) thin-client `JsonlClient`.
//!
//! It owns exactly the framing concerns: newline framing, the max-line bound (producing
//! [`TransportError::Oversize`]), and **both directions** of encode/decode for the two wire shapes,
//! so neither the server accept loop nor the thin client grows its own ad-hoc codec —
//!
//! * **bare** (version-free) — the LOCAL `session` / `batch` / `serve` surfaces and the thin client's
//!   local fallback: `encode_bare_request_line` / `decode_bare_request_line` (client→server) and
//!   `encode_bare_response_line` / `decode_bare_response_line` (server→client). Version-free **by
//!   design** (existing agent workflows send a bare `{id, command, args}`);
//! * **versioned** site frames — `encode_site_envelope_line` / `decode_site_envelope_line` carry a
//!   [`ProtocolEnvelope`] so a skewed or unversioned peer is rejected **loudly** (`TransportError`),
//!   never silently degraded. The site *response* is a bare `SessionResponse` (decoded with
//!   `decode_bare_response_line`); the version rides on the request envelope, which is sufficient to
//!   detect skew in either direction.
//!
//! Listener binding, idle/read/write timeouts, server-owned context binding (`index_dir` stripping),
//! and dispatch all stay in the **server composition layer**, NOT in this codec.

use std::io::{self, BufRead};

use jurisearch_core::envelope::{PROTOCOL_VERSION, ProtocolEnvelope};
use jurisearch_core::session::{SessionRequest, SessionResponse};

/// Max bytes for one request line; an oversize line is rejected and the caller closes the
/// connection, so a client cannot exhaust memory with an unbounded line. (Formerly
/// `jurisearch-cli`'s `serve::SERVE_MAX_REQUEST_BYTES`; the framing bound now lives with the codec.)
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// The fallback line emitted when a `SessionResponse` somehow fails to serialize (never expected for
/// a well-formed response). Byte-compatible with the prior inline `serve.rs` fallback.
const ENCODE_FALLBACK: &str =
    r#"{"ok":false,"error":{"code":"internal","message":"failed to encode response"}}"#;

/// Canonical transport/framing errors. Distinct from a session [`jurisearch_core::error::ErrorObject`]
/// (a *handler* outcome) and from a work/08 package `Reject` (a *package* outcome): this is purely the
/// wire layer failing to produce a valid frame.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The line was not valid JSON for the expected shape.
    #[error("malformed JSONL frame: {0}")]
    Malformed(String),
    /// The line exceeded [`MAX_LINE_BYTES`]. The Display text matches the historical framing message
    /// so the local `serve` oversize reply stays byte-identical.
    #[error("request line exceeds the size limit")]
    Oversize,
    /// A site frame carried no `proto` field — an unversioned/legacy frame is not accepted on the
    /// site path (it would be accepted on the *local* path, which uses the bare decoder).
    #[error(
        "site frame is missing the protocol version (an unversioned/legacy frame is rejected on the site path)"
    )]
    Unversioned,
    /// A site frame carried a `proto` this build does not speak.
    #[error("unsupported protocol version {got}: this peer speaks {supported}")]
    UnsupportedVersion { got: u32, supported: u32 },
    /// A genuine I/O error while reading a framed line (connection reset, broken pipe, …) — distinct
    /// from a framing/protocol failure.
    #[error("transport I/O error: {0}")]
    Io(#[from] io::Error),
}

/// Read one newline-terminated line, bounded to `max` bytes. `Ok(None)` at EOF; an oversize line is
/// a [`TransportError::Oversize`] (the caller replies and closes); an underlying read failure is a
/// [`TransportError::Io`]. The framing the local `serve` loop used inline, lifted here so the codec
/// owns framing and an oversize line is a *typed* transport error rather than a bare `io::Error`.
pub fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    max: usize,
) -> Result<Option<String>, TransportError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte)? {
            0 => {
                return Ok(if buf.is_empty() {
                    None
                } else {
                    Some(String::from_utf8_lossy(&buf).into_owned())
                });
            }
            _ => {
                if byte[0] == b'\n' {
                    return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
                }
                buf.push(byte[0]);
                if buf.len() > max {
                    return Err(TransportError::Oversize);
                }
            }
        }
    }
}

// ---- Local (bare, version-free) path ----------------------------------------------------------

/// Encode a **bare** `SessionRequest` into a framed (compact JSON + trailing newline) line — what a
/// client (thin-client local fallback, or a test harness) sends to a `serve`/`session` peer.
pub fn encode_bare_request_line(request: &SessionRequest) -> String {
    let json = serde_json::to_string(request).unwrap_or_else(|_| ENCODE_FALLBACK.to_owned());
    format!("{json}\n")
}

/// Decode a **bare** `SessionRequest` line — the local `session`/`batch`/`serve` shape. No version is
/// required or consulted; a malformed line is a [`TransportError::Malformed`].
pub fn decode_bare_request_line(line: &str) -> Result<SessionRequest, TransportError> {
    serde_json::from_str(line).map_err(|error| TransportError::Malformed(error.to_string()))
}

/// Encode a `SessionResponse` into a framed (compact JSON + trailing newline) line — the shape the
/// local JSONL surfaces emit. Infallible: a serialization failure yields [`ENCODE_FALLBACK`] rather
/// than panicking, matching the prior `serve.rs` behaviour.
pub fn encode_bare_response_line(response: &SessionResponse) -> String {
    let json = serde_json::to_string(response).unwrap_or_else(|_| ENCODE_FALLBACK.to_owned());
    format!("{json}\n")
}

/// Decode a **bare** `SessionResponse` line — what the thin client reads back from the service (both
/// the local and the site paths return a bare response). This is the response-side codec authority,
/// so the `JsonlClient` (work/09 P6) reuses it instead of an ad-hoc `serde_json::from_str`.
pub fn decode_bare_response_line(line: &str) -> Result<SessionResponse, TransportError> {
    serde_json::from_str(line).map_err(|error| TransportError::Malformed(error.to_string()))
}

// ---- Site (versioned) path --------------------------------------------------------------------

/// Decode a **versioned site** frame. Rejects (in order): malformed JSON, a missing `proto` field
/// (an unversioned/legacy frame — [`TransportError::Unversioned`]), and a `proto` this build does not
/// speak ([`TransportError::UnsupportedVersion`]). This is what makes "fail loudly on skew" real.
pub fn decode_site_envelope_line(line: &str) -> Result<ProtocolEnvelope, TransportError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|error| TransportError::Malformed(error.to_string()))?;
    if value.get("proto").is_none() {
        return Err(TransportError::Unversioned);
    }
    let envelope: ProtocolEnvelope = serde_json::from_value(value)
        .map_err(|error| TransportError::Malformed(error.to_string()))?;
    if envelope.proto != PROTOCOL_VERSION {
        return Err(TransportError::UnsupportedVersion {
            got: envelope.proto.0,
            supported: PROTOCOL_VERSION.0,
        });
    }
    Ok(envelope)
}

/// Encode a versioned site frame (compact JSON + trailing newline).
pub fn encode_site_envelope_line(envelope: &ProtocolEnvelope) -> String {
    let json = serde_json::to_string(envelope).unwrap_or_else(|_| ENCODE_FALLBACK.to_owned());
    format!("{json}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_core::envelope::ProtocolVersion;
    use jurisearch_core::error::ErrorObject;
    use serde_json::{Value, json};

    fn sample_request() -> SessionRequest {
        SessionRequest {
            id: Some(json!("req-1")),
            command: "search".to_owned(),
            args: json!({ "query": "article 1240" }),
        }
    }

    #[test]
    fn bare_request_round_trips() {
        let line = format!("{}\n", serde_json::to_string(&sample_request()).unwrap());
        let decoded = decode_bare_request_line(line.trim()).expect("decode");
        assert_eq!(decoded.command, "search");
        assert_eq!(decoded.id, Some(json!("req-1")));
    }

    #[test]
    fn bare_response_encodes_compact_with_newline() {
        let response = SessionResponse::ok(Some(json!("req-1")), json!({ "hits": [] }));
        let line = encode_bare_response_line(&response);
        assert!(line.ends_with('\n'));
        assert!(
            !line.trim_end().contains('\n'),
            "must be a single compact line"
        );
        // Compact, not pretty: no two-space indentation.
        assert!(!line.contains("\n  "));
        let parsed: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["id"], "req-1");
    }

    #[test]
    fn malformed_bare_line_is_rejected() {
        let err = decode_bare_request_line("not json").unwrap_err();
        assert!(matches!(err, TransportError::Malformed(_)));
    }

    #[test]
    fn site_envelope_round_trips_with_version() {
        let envelope = ProtocolEnvelope::new(sample_request());
        let line = encode_site_envelope_line(&envelope);
        let parsed: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["proto"], PROTOCOL_VERSION.0);
        let decoded = decode_site_envelope_line(line.trim()).expect("decode site frame");
        assert_eq!(decoded.proto, PROTOCOL_VERSION);
        assert_eq!(decoded.request.command, "search");
    }

    #[test]
    fn site_decoder_rejects_unversioned_bare_frame() {
        // A bare local frame (no `proto`) must be rejected on the SITE path...
        let bare = serde_json::to_string(&sample_request()).unwrap();
        let err = decode_site_envelope_line(&bare).unwrap_err();
        assert!(matches!(err, TransportError::Unversioned), "got {err:?}");
        // ...while the LOCAL decoder still accepts exactly that frame.
        assert!(decode_bare_request_line(&bare).is_ok());
    }

    #[test]
    fn site_decoder_rejects_skewed_version() {
        let mut envelope = ProtocolEnvelope::new(sample_request());
        envelope.proto = ProtocolVersion(PROTOCOL_VERSION.0 + 1);
        let line = encode_site_envelope_line(&envelope);
        let err = decode_site_envelope_line(line.trim()).unwrap_err();
        match err {
            TransportError::UnsupportedVersion { got, supported } => {
                assert_eq!(got, PROTOCOL_VERSION.0 + 1);
                assert_eq!(supported, PROTOCOL_VERSION.0);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    #[test]
    fn read_bounded_line_rejects_oversize() {
        let big = format!("{}\n", "x".repeat(64));
        let mut reader = io::BufReader::new(big.as_bytes());
        let err = read_bounded_line(&mut reader, 8).unwrap_err();
        assert!(matches!(err, TransportError::Oversize), "got {err:?}");
        // The Display must match the legacy framing message so the local serve reply is byte-stable.
        assert_eq!(err.to_string(), "request line exceeds the size limit");
    }

    #[test]
    fn bare_request_and_response_round_trip_both_directions() {
        // Client encodes a request; server decodes it.
        let req_line = encode_bare_request_line(&sample_request());
        assert!(req_line.ends_with('\n'));
        assert!(!req_line.trim_end().contains('\n'));
        let decoded_req = decode_bare_request_line(req_line.trim()).expect("decode request");
        assert_eq!(decoded_req.command, "search");

        // Server encodes a response; client decodes it.
        let response = SessionResponse::ok(Some(json!("req-1")), json!({ "hits": [1, 2] }));
        let resp_line = encode_bare_response_line(&response);
        let decoded_resp = decode_bare_response_line(resp_line.trim()).expect("decode response");
        assert!(decoded_resp.is_ok());
        assert_eq!(decoded_resp.result().unwrap()["hits"], json!([1, 2]));
    }

    #[test]
    fn malformed_bare_response_is_rejected() {
        let err = decode_bare_response_line("not json").unwrap_err();
        assert!(matches!(err, TransportError::Malformed(_)));
    }

    #[test]
    fn read_bounded_line_reads_then_eof() {
        let data = "first\nsecond\n";
        let mut reader = io::BufReader::new(data.as_bytes());
        assert_eq!(
            read_bounded_line(&mut reader, 64).unwrap().as_deref(),
            Some("first")
        );
        assert_eq!(
            read_bounded_line(&mut reader, 64).unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(read_bounded_line(&mut reader, 64).unwrap(), None);
    }

    #[test]
    fn site_error_is_not_a_session_error_object() {
        // Sanity: a TransportError is its own type, distinct from a handler ErrorObject.
        let _session_err = ErrorObject::bad_input("unrelated");
        let transport_err = TransportError::Oversize;
        assert_eq!(
            transport_err.to_string(),
            "request line exceeds the size limit"
        );
    }
}
