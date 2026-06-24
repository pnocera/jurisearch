# Code Review: Zone rollout slice 2 - Legifrance legislation enrichment v17

Reviewed commit: `9ca529b1dd4d18a2afdc3288029c39ab363bd98a`

## Findings

### WARN: Missing client-id path bypasses the durable Legifrance exchange archive

`enrich_legislation_citations_payload` returns `dependency_unavailable` before opening the index, paging pending citation resolutions, calling `PisteClient::legifrance_search_exchange`, archiving the exchange, or updating `legislation_citation_resolutions` whenever `OfficialApiConfig::from_env().legifrance_client_id.is_none()` (`crates/jurisearch-cli/src/main.rs:4676`). That contradicts the archivable exchange contract added in this slice: `legifrance_search_exchange` explicitly converts missing OAuth credentials or token acquisition failure into an `OfficialApiExchange` with `outcome = UpstreamError` (`crates/jurisearch-official-api/src/lib.rs:390`, `crates/jurisearch-official-api/src/lib.rs:401`) so the CLI can archive the attempt with `archive_exchange` (`crates/jurisearch-cli/src/main.rs:4722`, `crates/jurisearch-cli/src/main.rs:4723`) and record the resolution status (`crates/jurisearch-cli/src/main.rs:4755`). Because of the early return, a run with no client id leaves all pending citation rows untouched and writes no `official_api_responses` evidence row, even though token failures and missing client-secret errors follow the durable path.

Concrete fix: remove the preflight client-id guard and let `legifrance_search_exchange` own missing-credential/token-failure classification for every pending citation. If the operator-facing error is still useful, derive it from the archived `upstream_error` rows in the command summary instead of short-circuiting before durable accounting. Add a CLI/storage contract test that seeds one pending citation, clears both Legifrance OAuth env vars, runs `ingest enrich-legislation-citations --limit 1`, and asserts one archived `provider='legifrance'` `outcome='upstream_error'` row plus a matching `legislation_citation_resolutions.legifrance_status = 'upstream_error'`.

## Notes

- Citation extraction is panic-safe for the reviewed real visa shape: the URL `query` parameter path is preferred, HTML fallback remains conservative, byte slicing is only at the ASCII `code` match boundary, and non-code legislation is skipped.
- Dedup integrity is structurally sound: occurrence inserts are parameterized and idempotent on `(decision_document_id, visa_index, citation_key)`, resolution upsert does not reset resolved rows, and `finalize_citation_occurrence_counts` recomputes counts from occurrences.
- The v17 DDL is contiguous, the requested FK delete behavior is present, read-side cursor predicates are escaped via `sql_string_literal`, and write paths use parameterized `postgres` clients.
- I did not rerun the cargo suites during this review; I inspected the diff, current source, CodeGraph context, and the added storage/parser tests.

VERDICT: FIXES_REQUIRED
