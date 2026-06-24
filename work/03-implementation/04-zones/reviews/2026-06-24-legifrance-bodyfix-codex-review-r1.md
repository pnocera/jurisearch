# Code Review - Legifrance /search Request Body Fix

## Findings

### WARN - New request body collapses archived Legifrance request fingerprints

`enrich_legislation_citations_payload` now sends `legifrance_code_search_body(canonical_query)` into `legifrance_search_exchange` (`crates/jurisearch-cli/src/main.rs:4747-4748`) and still records `exchange.request_fingerprint` on each resolution row (`crates/jurisearch-cli/src/main.rs:4781-4787`). However, `legifrance_search_exchange` still computes that fingerprint from a legacy top-level `query` field (`crates/jurisearch-official-api/src/lib.rs:397-400`). The new real-contract body intentionally has no top-level `query`, so every enrichment attempt now archives the same fingerprint: `legifrance-search:`.

That regresses the resolution-recording/audit semantics even though the HTTP request body is fixed: operators can no longer distinguish which canonical query produced a given resolution row from the fingerprint column, and retries/errors become much harder to audit.

Concrete fix: update the Legifrance exchange fingerprint builder to support both body shapes, for example:

- prefer `body["query"]` for legacy callers while they still exist;
- otherwise read the real-contract criterion value at `recherche.champs[*].criteres[*].valeur`;
- if no query value exists, include a stable hash of the serialized request body rather than an empty suffix.

Add a regression test using the new Legifrance body shape that asserts the fingerprint is non-empty and contains, or is otherwise stably derived from, the canonical query.

### WARN - `cite --online` still uses the known-bad `{query,pageSize}` body

`apply_online_citation_confirmation` still calls `/dila/legifrance/lf-engine-app/search` with:

```rust
json!({
    "query": query,
    "pageSize": 1,
})
```

at `crates/jurisearch-cli/src/main.rs:11683-11688`. The review brief says this exact shape was live-validated to return HTTP 500 from the Legifrance engine. This path is separate from the enrichment loop, but it is still user-facing through `cite --online` (`cite_payload` calls it at `crates/jurisearch-cli/src/main.rs:5227-5228`). Because this path uses `legifrance_search` directly, the known-bad body will surface as an online-check failure instead of producing the intended online summary.

Concrete fix: factor the request-body builder so both enrichment and `cite --online` use the real `/search` contract. If `cite --online` needs a different `pageSize`, make the helper accept `page_size`; if `cite --online` can probe non-code statutory citations, either name/scope the helper accordingly or choose `fond` from the parsed target before sending. Add a unit test around the shared helper or the online path's constructed body so the bogus top-level `query` shape cannot reappear.

## Non-Finding Notes

- The enrichment path itself is correctly wired to the new body: the old inline `{query,pageSize}` body is gone from `enrich_legislation_citations_payload`, and the loop still archives every `OfficialApiExchange` before mapping `Ok` to `ok`/`not_found` and errors to `parse_error`/`upstream_error`.
- `legifrance_response_has_results` still matches the response shape described in the brief: it treats `totalResultNumber > 0` as authoritative and falls back to a non-empty `results` array.
- `fond = "CODE_DATE"` is acceptable for this enrichment pass because the collected candidates are canonical code-article citations. Leaving out a date filter means "current version"; that is a defensible v1 behavior as long as historical/as-of validation is not being claimed.
- The new unit test verifies the important real-contract fields and the absence of the legacy top-level `query`. It would be stronger if paired with the fingerprint regression test above.

VERDICT: FIXES_REQUIRED
