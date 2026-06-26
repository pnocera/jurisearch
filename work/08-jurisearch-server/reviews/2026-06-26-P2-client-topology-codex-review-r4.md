# Code Review: P2 Client Storage Topology r4

## Findings

### BLOCKER: `fetch --part --online` still resolves Judilibre metadata from `public`

`official_decision_part` correctly reads the `decision_zones` cache through `decision_zones_json`, so that cache lookup follows the active generation (`crates/jurisearch-cli/src/enrichment/decision_part.rs:137`). But on a cache miss/expired row, it calls `enrich_decision_from_judilibre` (`crates/jurisearch-cli/src/enrichment/decision_part.rs:148-150`). That wrapper opens a raw `postgres::Client` with the default search path and immediately delegates to `enrich_decision_from_judilibre_with_client` (`crates/jurisearch-cli/src/enrichment/judilibre_zones.rs:94-101`). The core then calls `decision_resolution_metadata_with_client` (`crates/jurisearch-cli/src/enrichment/judilibre_zones.rs:113-116`), whose SQL reads unqualified `documents` (`crates/jurisearch-storage/src/decision_zones.rs:91-108`).

That means this production client path still reads `public.documents` for the local source UID, ECLI, decision date, and pourvoi. On a client where the decision exists only in the active generation and `public` is empty or stale, the outer `fetch` can serve the document from the generation, but `--part --online` cannot resolve the pourvoi needed for Judilibre. It will see JSON `null`, treat the decision as unsupported, and write a negative cache row to `public`, returning the heuristic/unavailable part instead of the official zone. This is the same split-brain class as the r3 cite issue, now in the online fetch-part surface.

Concrete fix: make the client-facing wrapper resolve metadata through the read role before entering the producer-style write flow. For example, have `enrich_decision_from_judilibre` call the existing `decision_resolution_metadata_json(postgres, document_id)` (the ManagedPostgres variant now uses `execute_read_sql`) and plumb that metadata JSON into a shared helper that still uses the raw client for the archive/cache writes. Keep the batch/backfill `_with_client` variant on its caller-owned public connection. Do not simply set the shared write client's search path to the active generation unless all subsequent `decision_zones` and archive writes are reset or explicitly qualified back to `public`.

Add a regression that cuts and activates a generation containing a Cassation decision with parser-valid pourvoi, empties `public.documents`, mocks the Judilibre search/decision endpoints, and runs the real CLI:

`jurisearch fetch cass:<id> --part motivations --online`

The assertion should prove the response contains the official Judilibre zone. It should fail if the metadata lookup reads stale/empty `public`.

## Confirmed

- The r3 citation blocker is resolved for the direct cite path: `citation_lookup_json` now calls `execute_read_sql` (`crates/jurisearch-storage/src/citation.rs:103-109`), and its union arms read only replicated `documents` / `legi_metadata_roots` plus public-resolvable functions.
- The new storage-level and CLI-level cite regressions cover active-generation lookup after `public` is emptied (`crates/jurisearch-storage/tests/generations.rs:589-641`, `crates/jurisearch-cli/tests/cli_retrieval_contract.rs:407-480`).
- The expected fetch/search/context/related/stats/versions/eval/zone/status read functions are routed through `execute_read_sql` or the readiness `apply_read_search_path` path.
- The remaining direct `execute_sql` production callers I found are producer/enrichment writes or producer-side derivation reads, consistent with the stated P2 boundary.
- Keeping online `decision_zones` cache write-back on `public` is acceptable as a P2 correctness boundary, but after `decision_zones_json` reads the active generation it is no longer a cross-call cache optimization for generation-backed clients unless a later phase gives client cache writes a generation/app-local home.

## Validation

I did not run the test suite for this review; this pass was a static source and working-tree review.

VERDICT: FIXES_REQUIRED
