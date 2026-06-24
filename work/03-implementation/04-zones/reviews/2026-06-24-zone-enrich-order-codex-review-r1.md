# Code Review: `c3a78cf` zone enrich order

## Findings

No BLOCKER, WARN, or NIT findings.

## Verification Notes

- Keyset paging is directionally consistent in `enrich_zone_candidates_json`: `Oldest` uses `document_id > cursor`, `ORDER BY document_id ASC`, and `max(document_id)` as `next_cursor`; `Recent` uses `document_id < cursor`, `ORDER BY document_id DESC`, and `min(document_id)` as `next_cursor`.
- The inner `jsonb_agg(... ORDER BY document_id {sort_dir})` preserves the selected page order and does not affect the boundary cursor calculation.
- The candidate predicate is unchanged from the parent commit apart from the ordering/cursor direction plumbing: `kind`, `source`, parser-valid pourvoi reachability, missing/expired zone rows, fresh `ok`/`invalid_offsets` rows with `text_hash IS NULL`, and optional `since` refresh semantics are preserved.
- No resolver, `decision_zones` cache, normalization, derivation, or retrieval semantics are modified by this commit.
- CLI threading is complete: the `EnrichZones` subcommand accepts `--order`, defaults it to `oldest`, validates it through the fixed Clap value enum, passes it through `emit_ingest` into `enrich_zones_payload`, forwards it to `enrich_zone_candidates_json` on each page, and records `"order"` in the JSON report.
- The paging loop still terminates on `--limit`, empty candidate pages, or `next_cursor == null`; `CliEnrichZoneOrder` is `Copy`, so `order.into()` inside the loop does not consume state.
- SQL-injection exposure is not introduced by the dynamic fragments: comparison operator, sort direction, and boundary aggregate are selected only from fixed enum matches; `source`, `cursor`, and `since` continue to use `sql_string_literal`, and `limit` is a `u32`.
- The new storage test proves the intended direction switch with page size 1: `Recent` returns `cass:CCC`, emits that cursor, and the next page returns `cass:BBB`; `Oldest` returns `cass:AAA` with its cursor.

## Validation

- Reviewed `git show c3a78cf`.
- Used CodeGraph to confirm the changed storage function has only four callers: the CLI payload plus three storage tests.
- Ran `cargo test -p jurisearch-storage --test zone_units enrich_candidate_order_recent_walks_newest_first` successfully.

VERDICT: GO
