# Review: work/09 P3C — multi-corpus fan-out r2

## Findings

No findings.

## Re-Review Checks

- The r1 zone-cursor issue is addressed in the request adapter, not only in storage: `zone_search_payload` parses the cursor as document-grouped, then immediately calls `reject_multi_corpus_zone_cursor(&after_cursor)` before query normalization, index resolution, `open_index`, or any snapshot/database work (`crates/jurisearch-cli/src/retrieval/zone.rs:125-144`). A parsed `ParsedSearchCursor::MultiCorpus` therefore cannot reach `zone_candidates_in_snapshot`, where the single-corpus SQL predicate would ignore it.
- The added zone cursor unit test uses a real `mc:document:0.01639344:core:cass:D1` cursor through `parse_search_cursor`, verifies it becomes `ParsedSearchCursor::MultiCorpus`, and verifies the zone guard rejects it (`crates/jurisearch-cli/src/retrieval/zone.rs:386-400`). That covers the cursor shape that caused the r1 replay risk.
- The multi-corpus `--zone` topology deferral remains fail-closed: `reject_multi_corpus_zone` rejects snapshots with more than one active corpus (`crates/jurisearch-cli/src/retrieval/zone.rs:61-68`), and the test exercises both the single-corpus pass case and the two-corpus reject case (`crates/jurisearch-cli/src/retrieval/zone.rs:403-447`).
- The r1 pagination-depth nit is addressed by the revised storage test. It seeds six decisions per corpus, pages the real `hybrid_candidates_in_snapshot` path with page size 2, carries each emitted `mc:` cursor into the next request, and asserts all 12 decisions are returned exactly once (`crates/jurisearch-storage/tests/query_fanout_p3c.rs:188-275`). A fixed first-page arm depth would exhaust before local rank 6 and fail the final unique/length assertions.
- The implementation under test derives fan-out arm depth from the parsed multi-corpus cursor score (`implied_rank(cursor.score) + page + 1`) and runs each arm with `after_cursor: None` plus the derived deep limit (`crates/jurisearch-storage/src/retrieval/hybrid.rs:263-285`), then applies the cross-corpus keyset in Rust (`crates/jurisearch-storage/src/retrieval/hybrid.rs:313-317`).

## Tests Run

- `cargo test -p jurisearch-cli zone_search_rejects_a_multi_corpus_cursor`
- `cargo test -p jurisearch-cli multi_corpus_zone_search_fails_closed`
- `cargo test -p jurisearch-storage --test query_fanout_p3c multi_corpus_pagination_is_stable_and_reaches_deep_cursor_ranks`

VERDICT: GO
