//! Raw-BYTE parity tests for the local JSONL + one-shot surfaces.
//!
//! The other contract tests parse stdout back into a `serde_json::Value` and assert selected fields,
//! which would NOT catch a compact-vs-pretty change, a field-order change, a missing/extra trailing
//! newline, or altered error text. These assert the **exact bytes**, locking the local byte-parity
//! invariant the work/09 P1 re-wiring (onto `jurisearch-transport` + `jurisearch-render`) must
//! preserve.

use assert_cmd::Command;

/// Run the CLI and return its exact stdout bytes, asserting success + empty stderr so these tests
/// fail on an exit-status or stderr regression, not only a stdout one.
fn run(args: &[&str], stdin: &str) -> Vec<u8> {
    let assert = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(args)
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .write_stdin(stdin)
        .assert()
        .success();
    let output = assert.get_output();
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout.clone()
}

#[test]
fn session_exit_emits_exact_compact_bytes() {
    let out = run(
        &["session", "--jsonl"],
        "{\"id\":\"x\",\"command\":\"exit\"}\n",
    );
    assert_eq!(
        out,
        b"{\"id\":\"x\",\"ok\":true,\"result\":{\"bye\":true}}\n"
    );
}

// The exact serde-detail + `suggestions` array embedded in a malformed-frame reply. Asserting the
// FULL bytes (not a prefix) locks the complete error shape: the `malformed JSONL request: ` prefix is
// not doubled with the codec's own `malformed JSONL frame:` text, the serde detail is preserved, and
// the trailing `ErrorObject` fields keep their order. (A serde_json upgrade that reworded the detail
// would intentionally trip this — the message is part of our emitted bytes.)
const SESSION_MALFORMED_LINE: &str = r#"{"ok":false,"error":{"code":"bad_input","message":"malformed JSONL request: expected ident at line 1 column 2","suggestions":["Run `jurisearch help agent` for accepted commands and flags."]}}"#;
const EXIT_NO_ID_LINE: &str = r#"{"ok":true,"result":{"bye":true}}"#;

#[test]
fn session_malformed_line_emits_exact_bytes_then_continues() {
    let out = run(
        &["session", "--jsonl"],
        "not json\n{\"command\":\"exit\"}\n",
    );
    // Non-fatal: the malformed line emits one compact error, then the trailing `exit` is processed.
    let expected = format!("{SESSION_MALFORMED_LINE}\n{EXIT_NO_ID_LINE}\n");
    assert_eq!(String::from_utf8(out).unwrap(), expected);
}

#[test]
fn batch_fatal_stops_after_a_malformed_line_with_exact_bytes() {
    // With --fatal, a malformed line writes ONE compact error then stops, so the trailing `exit`
    // command is never read. Identical message bytes to the session path.
    let out = run(
        &["batch", "--jsonl", "--fatal"],
        "not json\n{\"command\":\"exit\"}\n",
    );
    let expected = format!("{SESSION_MALFORMED_LINE}\n");
    assert_eq!(String::from_utf8(out).unwrap(), expected);
}

#[test]
fn one_shot_help_schema_json_is_pretty_with_a_single_trailing_newline() {
    // The one-shot path renders through `jurisearch-render::render_value_pretty`: PRETTY (2-space
    // indent) + exactly one trailing newline. Compact session/serve output and pretty one-shot output
    // are the two byte shapes P1 must keep distinct.
    let out = run(&["help", "schema", "--json"], "");
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("schema_version"),
        "expected the compiled schema: {text:.120?}"
    );
    assert!(
        text.contains("\n  "),
        "one-shot output must be pretty-printed"
    );
    assert!(
        text.ends_with("}\n"),
        "exactly one trailing newline expected"
    );
    assert!(!text.ends_with("}\n\n"), "no double trailing newline");
    serde_json::from_str::<serde_json::Value>(text.trim_end()).expect("valid JSON");
}
