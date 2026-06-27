# Review: work/09 Phase 3C — multi-corpus physical-generation fan-out + fusion

## Findings

### WARN: `search --zone` silently ignores an `mc:` cursor in a single-corpus topology

`parse_search_cursor` now accepts `mc:<group>:<score>:<corpus>:<id>` whenever the cursor's encoded group matches the requested CLI group (`crates/jurisearch-cli/src/retrieval/search.rs:560-597`). The zone adapter calls it with `CliGroupBy::Document` (`crates/jurisearch-cli/src/retrieval/zone.rs:109-114`), so a `mc:document:...` cursor is accepted for `search --zone`. The multi-corpus zone guard only rejects when the active snapshot has more than one corpus (`crates/jurisearch-cli/src/retrieval/zone.rs:139-141`), so under the normal single-corpus zone topology the `RetrievalCursor::MultiCorpus` reaches `zone_candidates_in_snapshot`. That path feeds it to `document_cursor_predicate` (`crates/jurisearch-storage/src/zone_retrieval.rs:238`), and the predicate deliberately returns an empty predicate for `RetrievalCursor::MultiCorpus` (`crates/jurisearch-storage/src/retrieval/sql.rs:76-87`).

The result is a silent first-page replay instead of the requested fail-closed cursor/topology behavior. This can happen if a caller accidentally reuses a multi-corpus main-search cursor with `--zone`, or if topology changed since a previous multi-corpus search. The main hybrid path handles this correctly by rejecting `MultiCorpus` on a single-corpus search (`crates/jurisearch-storage/src/retrieval/hybrid.rs:57-69`); the zone path needs the same kind of rejection.

Actionable fix: after parsing `after_cursor` in `zone_search_payload`, reject `ParsedSearchCursor::MultiCorpus` with a `bad_input` error such as "multi-corpus cursors cannot be used with --zone; restart the zone search without a cursor". Add a focused CLI unit test that feeds `mc:document:...` to the zone path in a single-corpus snapshot and proves it fails before `zone_candidates_in_snapshot`.

### NIT: The pagination test does not prove cursor-aware arm depth

`multi_corpus_pagination_is_stable_and_non_overlapping` uses two documents per arm and a page size of two (`crates/jurisearch-storage/tests/query_fanout_p3c.rs:207-251`). That catches the basic Rust-side cursor filter and non-overlap, but it would still pass if a regression fetched a fixed first-page arm depth rather than deriving depth from the cursor rank. With two candidates per arm, even `top_k + 1` per arm is enough to serve page 2.

Actionable fix: extend the test with at least one cursor page whose boundary rank is deeper than the first-page arm fetch depth would be. For example, seed five or six documents per corpus, page until the cursor is at local rank three or four, then assert the next page includes the expected deeper-rank candidates and remains non-overlapping. That would fail if `hybrid_candidates_fanout` stopped using `implied_rank(cursor.score) + page + 1`.

## Positive Checks

- The hot multi-corpus candidate reads use `read_text_for_corpus` per active corpus (`crates/jurisearch-storage/src/retrieval/hybrid.rs:278-285`), while the request-default `read_text` path switches to `jurisearch_server, public` for multi-corpus by-id/non-indexed reads (`crates/jurisearch-storage/src/query.rs:73-79`, `crates/jurisearch-storage/src/query.rs:149-163`).
- Dense/hybrid fan-out fails closed before retrieval if any active corpus fingerprint differs from the query fingerprint (`crates/jurisearch-storage/src/retrieval/hybrid.rs:243-258`).
- Multi-corpus fusion ranks each arm result by local list position, writes the cross-corpus RRF into `scores.rrf`, preserves the per-arm score as `scores.local_rrf`, tags `corpus`, and emits an `mc:` cursor (`crates/jurisearch-storage/src/retrieval/hybrid.rs:289-324`, `crates/jurisearch-storage/src/retrieval/hybrid.rs:337-367`).
- The single-corpus path rejects an `mc:` cursor rather than letting the single-corpus SQL ignore it (`crates/jurisearch-storage/src/retrieval/hybrid.rs:57-69`).

## Tests Run

- `cargo test -p jurisearch-storage --test query_fanout_p3c`
- `cargo test -p jurisearch-storage --test query_snapshot_p3b a_multi_corpus_snapshot_opens_and_resolves_every_active_corpus`
- `cargo test -p jurisearch-cli multi_corpus_zone_search_fails_closed`

VERDICT: FIXES_REQUIRED
