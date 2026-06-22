Phase C API retry/backoff review, round 2

Findings

None.

Reviewed notes

- BLOCKER resolved: `retry_after_from_error` now matches retryable status responses where `code == 429 || (500..=599).contains(code)` and reads `Retry-After` for both 429 and 5xx.
- The cap path is intact: `send_with_retry` still feeds the parsed header into `retry_delay`, and `retry_delay` returns `after.min(policy.max_delay)`.
- Non-retryable statuses still return `None` from `retry_after_from_error`; the new test covers 404 with a `Retry-After` header so that behavior does not regress.
- 500 without `Retry-After` still falls back to exponential backoff because the helper returns `None`.
- No retry-path regression found in the reviewed diff. The prior notes about attempt counting, request rebuilding, non-retryable status handling, and immediate test policy still hold.

Verification

- `cargo test -p jurisearch-official-api retry_after_from_error_reads_header_for_429_and_5xx` passed.
- `cargo test -p jurisearch-official-api` passed: 16 unit tests and 0 doctests.

VERDICT: GO
