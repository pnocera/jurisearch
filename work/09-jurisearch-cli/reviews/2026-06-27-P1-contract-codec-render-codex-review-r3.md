# Re-review r3 - P1 Contract / Codec / Render Foundation

## Findings

No findings.

## Resolved / Checked

- Re-checked the r2 warning in `crates/jurisearch-cli/tests/cli_byte_parity.rs` and `crates/jurisearch-cli/src/serve.rs`: the malformed raw-byte cases now compare the complete emitted stdout/socket bytes, including the serde detail, the `suggestions` array, trailing newlines, and the subsequent valid response where applicable.
- The `cli_byte_parity` helper now asserts command success and empty stderr before returning stdout, so the raw-byte tests no longer pass through exit-status or stderr regressions.
- The local framing split remains correct: `session`/`batch` and local `serve` use the bare decoder/encoder, while the site envelope decoder rejects unversioned and skewed protocol frames.
- The transport authority remains centralized in `jurisearch-transport` for bounded line reads, bare request/response codecs, and site envelope codecs. `serve_jsonl` preserves the legacy malformed-message prefixes by stripping the codec's `TransportError::Malformed` wrapper before constructing the local `ErrorObject`.
- The render authority remains centralized in `jurisearch-render` for pretty one-shot JSON bytes, while session/socket JSONL responses continue through compact bare response encoding.
- The request-vocabulary move is still dependency-direction clean: `GroupBy`, `RetrievalMode`, and `RetrievalOptions` now live in `jurisearch-core`, `jurisearch-storage` re-exports them, and the dependency-cone test covers `jurisearch-core`, `jurisearch-transport`, and `jurisearch-render`.

## Verification Run

- `cargo fmt --check`
- `cargo test -p jurisearch-cli --test cli_byte_parity --bins`
- `cargo test -p jurisearch-core -p jurisearch-transport -p jurisearch-render`
- `cargo test -p jurisearch-cli --test cli_session_contract --test cli_help_contract --test cli_status_contract`
- `cargo test -p jurisearch-cli --test cli_retrieval_contract` (24 passed, 1 ignored live-embeddings test)
- `cargo clippy -p jurisearch-core -p jurisearch-transport -p jurisearch-render -- -D warnings`
- `cargo clippy -p jurisearch-cli --all-targets` completed with existing broader default clippy warnings in `jurisearch-official-api` and older `jurisearch-cli` code; no warning pointed at the Phase 1 files reviewed here.
- `git diff --check`

VERDICT: GO
