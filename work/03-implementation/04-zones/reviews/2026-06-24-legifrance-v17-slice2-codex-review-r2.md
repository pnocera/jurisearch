# Code Review: Zone rollout slice 2 r2 - Legifrance legislation enrichment v17

Reviewed fix commit: `8f4be82a4ff23485b6568c7ce021e53c9c8a40b3`
Reviewed range: `9ca529b..8f4be82`

## Findings

None.

## Review Notes

- The r1 warning is resolved. The preflight `OfficialApiConfig::from_env().legifrance_client_id.is_none()` guard is gone from `enrich_legislation_citations_payload`, so missing OAuth credentials no longer exit before durable accounting.
- The missing-credential path now flows through the same per-citation sequence as other Legifrance attempts: `PisteClient::legifrance_search_exchange` returns an archivable `OfficialApiOutcome::UpstreamError` exchange when `legifrance_bearer_token` reports missing credentials, `archive_exchange` inserts the `official_api_responses` row, and `update_citation_resolution_with_client` records `legifrance_status = 'upstream_error'` with the archived response id and request fingerprint.
- The per-citation loop still terminates and pages correctly. `load_pending_citation_resolutions_json` keysets by `citation_key`, returns `next_cursor` from the selected page, and the caller advances the cursor after processing the page; updating selected rows to `ok`, `not_found`, `parse_error`, or `upstream_error` does not cause the current run to revisit the same page.
- The operator `note` is scoped to the all-attempts-failed case (`considered > 0`, no ok/not-found results, and `errors == considered`). It preserves the archived `upstream_error` behavior and only adds a summary hint about checking Legifrance OAuth credentials/subscription.
- Credential-present behavior is unchanged by the r2 fix: once credentials exist, `legifrance_search_exchange` still obtains/reuses the bearer token and posts the same request body to the same Legifrance search endpoint before `build_exchange` classifies the HTTP result.
- The added official-api unit test covers the no-network missing-credential exchange shape (`provider`, method, `UpstreamError`, no HTTP status/response JSON, recorded error, request fingerprint). The added CLI integration test seeds one pending citation, clears the production Legifrance client-id env sources used by the default config, runs `ingest enrich-legislation-citations --limit 1`, and asserts one archived Legifrance `upstream_error` row plus `legislation_citation_resolutions.legifrance_status = 'upstream_error'`.

## Validation

- `cargo test -p jurisearch-official-api legifrance_search_exchange_archives_missing_credential -- --nocapture` passed.
- `cargo test -p jurisearch-cli enrich_legislation_citations_archives_missing_credential_attempt -- --nocapture` passed.

VERDICT: GO
