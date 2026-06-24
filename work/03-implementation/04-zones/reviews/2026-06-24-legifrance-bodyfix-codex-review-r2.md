# Code Review: Legifrance Body/Fingerprint Fix R2

## Findings

No findings.

The change fixes the two reviewed regressions without introducing a new archive/audit contract issue that I could reproduce from the current source:

- `legifrance_search_exchange` now computes `request_fingerprint` from the exact serialized POST body before both the missing-credential and sent-request paths archive it. The fingerprint remains an opaque text field in `official_api_responses` and `legislation_citation_resolutions`; I found no consumer that parses the old `legifrance-search:<query>` suffix.
- `enrich_legislation_citations_payload` still archives the full `OfficialApiExchange` first, then stores the same `exchange.request_fingerprint` on the resolution row, preserving the row-to-archive audit link.
- `cite --online` no longer sends the old top-level `{query,pageSize}` body. It now calls the same `legifrance_code_search_body` helper used by legislation enrichment, and the CLI contract test verifies the request body seen by a local HTTP server.
- `sanitize_legifrance_query` collapses control/whitespace runs and truncates through `.chars().take(...)`, so the length cap is char-boundary safe and clean citations remain unchanged.

## Verification

Reviewed:

- `git show 260b034`
- `crates/jurisearch-cli/src/main.rs`
- `crates/jurisearch-cli/tests/cli_contract.rs`
- `crates/jurisearch-official-api/src/lib.rs`
- `crates/jurisearch-storage/src/official_api_archive.rs`
- `crates/jurisearch-storage/src/legislation_citations.rs`
- `crates/jurisearch-storage/src/migrations.rs`

Ran:

```text
cargo test -p jurisearch-official-api -p jurisearch-cli -p jurisearch-storage
```

Result: passed. The run reported 175 passing tests and 5 ignored tests across the selected packages and their integration/doc test targets.

VERDICT: GO
