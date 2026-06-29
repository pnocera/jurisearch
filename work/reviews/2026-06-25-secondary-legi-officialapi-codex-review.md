# Codex Review: Secondary LEGI / official-api refactors

## Findings

- NIT: `crates/jurisearch-official-api/src/client.rs:7` widens `PisteClient`'s fields from private to `pub(crate)`, even though the moved implementation still only needs direct field access inside `client.rs`. This does not change the public `jurisearch_official_api::*` API, but it does broaden the internal mutation surface after a behavior-preserving split. Recommended fix: keep these fields private unless a sibling module has a concrete need to read or mutate them.

## Review Notes

- Reviewed `git diff fc512e4..HEAD -- crates/jurisearch-ingest crates/jurisearch-official-api`.
- LEGI parser production code is not split; `crates/jurisearch-ingest/src/legi/mod.rs:1690` now only wires `#[cfg(test)] mod tests;`, and `crates/jurisearch-ingest/src/legi/tests.rs` keeps the old test module as a child module using `super::{...}` imports.
- Checked the LEGI moved test fixtures against `fc512e4`: all 9 raw-string literal bodies are byte-identical in sequence after the move. The apparent indentation differences in the Rust diff are from raw-string-aware dedent preserving fixture bytes rather than dedenting raw-string contents.
- Official-api crate-root wiring is consistent with the intended split: `crates/jurisearch-official-api/src/lib.rs:16` keeps the submodules private, and `lib.rs:25` through `lib.rs:28` re-export exactly the expected public API symbols: `OfficialApiConfig`, `PisteEnvironment`, `PisteClient`, `OfficialApiExchange`, `OfficialApiOutcome`, `RetryPolicy`, and `OfficialApiError`.
- The official-api public structs/enums remain present in the split modules with the same externally visible surface: `RetryPolicy` in `retry.rs:14`, `PisteEnvironment` in `config.rs:39`, `OfficialApiConfig` in `config.rs:63`, `PisteClient` in `client.rs:6`, `OfficialApiOutcome` in `client.rs:308`, `OfficialApiExchange` in `client.rs:332`, and `OfficialApiError` in `error.rs:6`.
- Official-api moved tests differ from the old inlined test module only in the import path for the now-private helper `legifrance_search_fingerprint`, which moved from `super::{...}` to `crate::client::legifrance_search_fingerprint`; the test body content otherwise matches after wrapper/dedent/header removal.
- I did not rerun the cargo test/build commands because the review request only permitted writing the review file, and cargo would write build artifacts outside that file. The source/diff checks are consistent with the already-reported validation.

VERDICT: GO
