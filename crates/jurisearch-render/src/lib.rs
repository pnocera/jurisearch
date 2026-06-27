//! Byte-parity response rendering — the single **dependency-light** authority that turns a result
//! value / `SessionResponse` into the EXACT human / `--json` bytes the one-shot CLI emits today, so
//! the thin client (work/09 P6) and the one-shot CLI render **identically** without either pulling
//! the heavy stack.
//!
//! P1 scope is deliberately narrow: **byte-parity + response unwrapping only**, NOT a rich
//! per-operation renderer. The bytes to match:
//!
//! * one-shot success → pretty JSON body + one trailing newline (`serde_json::to_string_pretty` +
//!   `"\n"`), i.e. the body the CLI's `write_json(&result)` prints;
//! * one-shot / session error → the same pretty rendering of `{"ok": false, "error": …}`, i.e. what
//!   the CLI's `emit_error` prints.
//!
//! A `SessionResponse` (what the thin client receives from the site service) is unwrapped to those
//! same bytes: the `Ok` result body for success, the `{"ok": false, "error": …}` object for errors.

use jurisearch_core::session::SessionResponse;
use serde_json::{Value, json};

/// Pretty-print a JSON value with exactly one trailing newline — the one-shot CLI body bytes
/// (`serde_json::to_string_pretty(value)` + `"\n"`). This is the single formatting formula behind
/// `write_json` and artifact emission, so stdout, artifact files, and the thin client stay
/// byte-identical.
pub fn render_value_pretty(value: &Value) -> Result<String, serde_json::Error> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

/// Render a `SessionResponse` into the bytes the one-shot CLI would have emitted for that command:
/// the `Ok` result body (matching `write_json(&result)`), or pretty `{"ok": false, "error": …}` for
/// the `Err` variant (matching `emit_error`). This is the thin-client render path — same bytes, no
/// heavy deps.
pub fn render_session_response(response: &SessionResponse) -> Result<String, serde_json::Error> {
    match response {
        SessionResponse::Ok { .. } => {
            let result = response.result().expect("Ok variant has a result");
            render_value_pretty(result)
        }
        SessionResponse::Err { .. } => {
            let error = response.error().expect("Err variant has an error");
            render_value_pretty(&json!({ "ok": false, "error": error }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_core::error::ErrorObject;

    #[test]
    fn render_value_pretty_matches_pretty_plus_newline() {
        let value = json!({ "b": true, "a": [1, 2, 3] });
        let rendered = render_value_pretty(&value).unwrap();
        assert_eq!(
            rendered,
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
        );
        assert!(rendered.ends_with('\n'));
    }

    #[test]
    fn ok_response_renders_as_the_result_body() {
        let result = json!({ "hits": ["a", "b"], "total": 2 });
        let response = SessionResponse::ok(Some(json!("req-1")), result.clone());
        // The thin client must print exactly the body the one-shot CLI's `write_json(&result)` prints
        // — the id/ok envelope is unwrapped away.
        assert_eq!(
            render_session_response(&response).unwrap(),
            render_value_pretty(&result).unwrap()
        );
    }

    #[test]
    fn err_response_renders_as_emit_error_bytes() {
        let error = ErrorObject::bad_input("empty query");
        let response = SessionResponse::err(Some(json!("req-1")), error.clone());
        let expected = render_value_pretty(&json!({ "ok": false, "error": error })).unwrap();
        assert_eq!(render_session_response(&response).unwrap(), expected);
    }
}
