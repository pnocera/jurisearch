# Re-review - P1 Contract / Codec / Render Foundation

## Findings

### WARN - The malformed-byte tests are still prefix tests, not byte-parity tests

The r2 changes are meant to close the local `session`/`batch`/`serve` byte-parity gap, including exact malformed-error bytes, but the new malformed cases still assert only prefixes. `crates/jurisearch-cli/tests/cli_byte_parity.rs:45` through `crates/jurisearch-cli/tests/cli_byte_parity.rs:57` only require the session error to start with `malformed JSONL request: `, `crates/jurisearch-cli/tests/cli_byte_parity.rs:70` through `crates/jurisearch-cli/tests/cli_byte_parity.rs:80` only require the batch fatal error to start with the `bad_input` code, and `crates/jurisearch-cli/src/serve.rs:210` through `crates/jurisearch-cli/src/serve.rs:219` only prefix-check the direct `serve_jsonl` malformed response.

Those tests would still pass if the decoder regression this patch is trying to guard against came back as `malformed JSONL request: malformed JSONL frame: expected ...`, if the serde detail after the prefix was dropped or changed, or if the trailing `ErrorObject` fields were reordered/removed, as long as the prefix, compactness, and final newline checks held. That is still a false-green hole for the exact malformed-error text called out in the previous review.

Concrete fix: make each malformed raw-byte case compare the complete emitted bytes. For the current behavior, the session/batch malformed line should include `malformed JSONL request: expected ident at line 1 column 2` plus the existing `suggestions` array; the direct `serve_jsonl` malformed line should include `malformed request: expected ident at line 1 column 2` plus the same suggestions. Replace the `starts_with` assertions with full `assert_eq!(out, ...)` expectations for the whole stdout/socket string, including subsequent valid response lines and trailing newlines. Also have the `cli_byte_parity` helper assert `.success()` and empty stderr so the raw-byte tests fail on exit-status or stderr regressions, not only stdout regressions.

## Resolved / Checked

- The previous transport-authority warning is resolved in source: `decode_bare_response_line` is now public and tested, and `read_bounded_line` returns `TransportError::Oversize` rather than a local `io::Error`.
- The local/source wiring remains correctly split: `session`/`batch` and local `serve` decode bare frames, while `decode_site_envelope_line` rejects unversioned site frames and unsupported protocol versions.
- The dependency-cone direction remains clean: moved retrieval request vocabulary is owned by `jurisearch-core`, re-exported from storage, and the base-crate dependency-cone test covers `jurisearch-core`, `jurisearch-transport`, and `jurisearch-render`.
- I did not find a current output-byte regression in the implementation; the issue above is that the r2 tests still do not prove all of the bytes they claim to lock.

## Verification Run

- `cargo test -p jurisearch-core -p jurisearch-transport -p jurisearch-render`
- `cargo test -p jurisearch-cli --test cli_byte_parity --bins`
- `cargo test -p jurisearch-cli --test cli_session_contract --test cli_help_contract --test cli_status_contract`
- `cargo fmt --check`

VERDICT: FIXES_REQUIRED
