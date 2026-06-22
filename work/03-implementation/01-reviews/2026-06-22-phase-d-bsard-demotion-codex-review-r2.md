Review: Phase D BSARD demotion + retry hermeticity, round 2

Findings:
- None.

Verification notes:
- `RetryPolicy::from_env()` preserves the default `max_retries = 3`, accepts trimmed unsigned integer overrides including `0`, and falls back to the default for garbage, unset, empty, or negative values. `PisteClient::new()` reads that policy once at construction, while tests that need deterministic behavior can still override it with `with_retry_policy(...)`.
- The official-api tests that assert 429/5xx error mapping now explicitly use `RetryPolicy::immediate(0)`, so they no longer depend on ambient retry env. The retry-behavior tests provide enough mock requests for the configured retry counts.
- Disabling retries in the CLI `cite --online` 500 probe is the right contract-test choice. That test is exercising error mapping for a probe failure, not retry behavior; serving all retries would either slow the test under the production backoff or couple it to retry-count policy.
- I checked the other local mock server cases that can hit official API retry logic. The CLI 500 probe sets `JURISEARCH_PISTE_MAX_RETRIES=0`; the CLI success path has no transient status; official-api single-request 429/500 mapping tests override the retry policy; retry tests allocate enough requests; non-retryable 404 remains single-request and mapped.
- `jurisearch_command_without_embedding_env()` now removes `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK`, which closes the Phase A ambient-env leak for status/eval contract tests.
- The Phase D demotion logic remains correct: `external_expert_annotated_eval` is reported with `gating: false`, `claim_allowed` filters advisory checks out, and `france_legi_official_eval` stays gating with provenance, threshold, query-count, and evidence validation before it can pass.

Commands run:
- `git diff --check -- crates/jurisearch-cli/src/main.rs crates/jurisearch-core/src/schema.rs crates/jurisearch-official-api/src/lib.rs crates/jurisearch-cli/tests/cli_contract.rs` passed.
- `cargo test -p jurisearch-official-api` passed: 17 passed.
- `cargo test -p jurisearch-cli` passed: 13 unit tests passed; 45 integration tests passed; 2 ignored.

VERDICT: GO
