# Code Review: Judilibre Zone Enrichment r2

Scope reviewed: the updated working-tree diff for `fetch --part --online`, the new `decision_zones` storage helper/table, Judilibre client helpers, and the r1 WARN fixes. This was a static review only; I did not run the migration, `fetch --online`, mutate any index, or run tests/builds.

## Findings

### WARN: Duplicate requested IDs can lose the official-zone block on later copies

`fetch_documents_json` preserves every requested ID ordinal, including duplicates, by building `requested(document_id, ordinal)` and aggregating matched rows `ORDER BY m.ordinal` at `crates/jurisearch-storage/src/retrieval.rs:775` and `crates/jurisearch-storage/src/retrieval.rs:839`. The new annotation path, however, stores official-zone responses in a `HashMap<String, Value>` keyed only by `document_id` at `crates/jurisearch-cli/src/main.rs:3479`, then consumes the entry with `official.remove(document_id)` at `crates/jurisearch-cli/src/main.rs:3500`.

If a caller requests the same Cassation decision ID twice with an official zone available, only the first copy receives the official Judilibre block. The second copy falls through to the heuristic/unavailable fallback because the single map entry was removed. That makes identical fetched documents in one response report different `part` provenance and can incorrectly tell the caller official zones are absent for one copy.

Concrete fix: keep the official lookup result reusable for every matching document, for example by using `official.get(document_id).cloned()` instead of `remove`, or by annotating through a per-document vector aligned with the response array. Add a focused unit test or JSON-level test that fetch annotation produces identical `part` blocks for duplicate requested IDs.

## r1 WARN Resolution

- Cache TTL enforcement is now correct in the reviewed state machine. `zone_cache_action` honors fresh `ok` rows, falls back without network for fresh negative rows, refreshes only expired/missing rows under `--online && source == "cass"`, and `cache_zone_status` gives `upstream_error` an explicit short TTL instead of a permanent `NULL` expiry.
- The pourvoi/date guard is now correct for the r1 concern. `find_matching_judilibre_id` requires an exact remote date whenever the local decision date is known, and only uses number-only fallback when the local date is absent.

## Verified Areas

- Fresh `ok` cache rows with an empty requested zone now fall back without re-fetching, which matches the stated contract that an `ok` row contains all zones for that decision.
- Fresh `not_found`/`unsupported`/`invalid_offsets`/`upstream_error` rows now suppress repeat network calls until their TTL expires.
- Expired rows do not trigger network unless the request is online and the source is Cassation.
- `fetch --part --online` still does not attempt Judilibre enrichment for `summary`, `visa`, non-decision documents, or non-`cass` decisions.
- The new `decision_zones` table remains a per-decision overlay and does not mutate canonical decision records.
- Judilibre `/search` and `/decision` query parameters are passed through `ureq` query APIs rather than manual URL concatenation.

VERDICT: FIXES_REQUIRED
