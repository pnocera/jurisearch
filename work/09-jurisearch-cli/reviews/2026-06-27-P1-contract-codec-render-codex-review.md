# Review - P1 Contract / Codec / Render Foundation

## Findings

### WARN - `jurisearch-transport` is not yet the full response codec authority

`crates/jurisearch-transport/src/lib.rs:4` says the crate owns request/response encode/decode for the two wire shapes, but the public API only decodes requests (`decode_bare_request_line`, `decode_site_envelope_line`) and only encodes responses/envelopes (`encode_bare_response_line`, `encode_site_envelope_line`) at `crates/jurisearch-transport/src/lib.rs:87` and `crates/jurisearch-transport/src/lib.rs:103`. There is no `SessionResponse` decoder for the thin-client side, and the advertised `TransportError::Oversize` at `crates/jurisearch-transport/src/lib.rs:40` is not produced by `read_bounded_line`, which returns a legacy `io::Error` instead at `crates/jurisearch-transport/src/lib.rs:74`.

That leaves the future `JsonlClient` likely to grow an ad hoc `serde_json::from_str::<SessionResponse>` and its own oversize mapping, which is exactly the duplicated codec authority this phase is supposed to prevent.

Recommended fix: add and test the response-side API now, e.g. `decode_bare_response_line` or a clearly named site response decoder if the site response shape is intentionally bare `SessionResponse`. Also either expose a bounded read helper that maps max-line violations to `TransportError::Oversize`, or keep the legacy `io::Error` helper private to the local serve adapter and add the transport-error-producing path for the site/client codec.

### WARN - The local byte-parity tests can pass after byte-level regressions

The existing local JSONL contract tests parse stdout back into `serde_json::Value` and assert selected fields, for example `crates/jurisearch-cli/tests/cli_session_contract.rs:63` through `crates/jurisearch-cli/tests/cli_session_contract.rs:77` and `crates/jurisearch-cli/tests/cli_session_contract.rs:98` through `crates/jurisearch-cli/tests/cli_session_contract.rs:133`. Those tests would still pass if the refactor changed field order, compact-vs-pretty formatting, exact malformed-error text, or omitted the final newline. They also do not exercise `serve_jsonl`'s raw socket bytes; `serve` is one of the surfaces the instructions called out as byte-identical.

The source inspection looks careful: local `session`/`batch` still use the bare decoder in `crates/jurisearch-cli/src/session.rs:100`, local `serve` uses the bare decoder and bare encoder in `crates/jurisearch-cli/src/serve.rs:65` and `crates/jurisearch-cli/src/serve.rs:82`, and one-shot rendering now delegates to the shared renderer in `crates/jurisearch-cli/src/output.rs:62`. But the tests do not prove the non-negotiable byte-parity invariant.

Recommended fix: add raw-byte assertions for representative `session --jsonl`, `batch --jsonl`, and `serve_jsonl` cases. At minimum cover a bare `exit` success, malformed input, `--fatal` batch behavior, one direct `serve_jsonl` invocation over in-memory buffers, and one command whose one-shot pretty output goes through `jurisearch-render`. Assert the complete byte string, including compact vs pretty rendering and exactly one trailing newline.

## Other Notes

I accept the deliberate `RequestDto` / `parse_args` deferral to P4. In this slice, `Operation::parse_command` is the only live site contract consumer, and adding a full unused DTO set now would create the parallel request-default/validation authority the consultation warned about.

The dependency direction is clean in the reviewed code: `jurisearch-storage` re-exports the moved retrieval request types from `jurisearch-core`, while the three base crates stay dependency-light. The version gate is also scoped correctly in source: local surfaces decode bare `SessionRequest` frames, and the versioned `ProtocolEnvelope` decoder is only exposed as the site path.

## Verification Run

- `cargo test -p jurisearch-core -p jurisearch-transport -p jurisearch-render`
- `cargo test -p jurisearch-cli --test cli_session_contract --test cli_help_contract --test cli_status_contract`
- `cargo test -p jurisearch-storage --lib`
- `cargo test -p jurisearch-cli --bins`

VERDICT: FIXES_REQUIRED
