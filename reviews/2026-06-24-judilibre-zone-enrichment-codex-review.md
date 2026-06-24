# Code Review: Judilibre Lazy Zone Enrichment

Scope reviewed: the uncommitted working-tree diff for v12 `decision_zones`, Judilibre client helpers, and `fetch --part --online` enrichment. This was a static review only; I did not run `fetch --online`, apply migrations, mutate an index, or run tests/builds.

## Findings

### WARN: Cache TTLs are written but not enforced by the enrichment path

`decision_zones_json` returns an `expired` field from `expires_at <= now()` at `crates/jurisearch-storage/src/decision_zones.rs:13`, but `official_decision_part` never reads it. Any cached `status = 'ok'` row is accepted forever at `crates/jurisearch-cli/src/main.rs:3560`, even after the intended positive TTL. Conversely, cached negative rows (`not_found`, `unsupported`, `invalid_offsets`) never suppress a repeat online lookup because non-`ok` rows fall through to `enrich_decision_from_judilibre` whenever `online && source == "cass"` at `crates/jurisearch-cli/src/main.rs:3565`. `cache_zone_status` also sets `ttl_seconds = None` for `upstream_error` at `crates/jurisearch-cli/src/main.rs:3763`, which stores `expires_at = NULL`; that contradicts the comment that upstream errors are "not cached long" and would become a permanent cache if the caller starts honoring `expired`.

This breaks the stated cache contract: positive rows are not really 30-day rows, negative rows are not really 7-day rows, and transient upstream failures do not have a coherent retry policy.

Concrete fix: make `official_decision_part` treat a row as cacheable only when it is not expired. For `status = 'ok' && !expired`, return the official block. For non-`ok && !expired`, return `Ok(None)` without network. For expired rows, refresh only when `online && source == "cass"`; otherwise fall back. Give `upstream_error` an explicit short TTL, or do not persist it as a suppressing cache entry. Add unit coverage for fresh positive, expired positive, fresh negative, and upstream-error rows.

### WARN: The pourvoi/date guard can still accept a number-only match when a local date exists

The design says resolution is pourvoi-first but validated by normalized number and decision date. `find_matching_judilibre_id` correctly rejects mismatched remote dates at `crates/jurisearch-cli/src/main.rs:3710`, but if the local date exists and the remote result lacks `decision_date`, the `_` arm stores a date-agnostic candidate at `crates/jurisearch-cli/src/main.rs:3713`. That means a malformed or changed Judilibre search response can still resolve a local decision by pourvoi alone, despite the local date being available.

This is probably rare if live `/search` always returns `decision_date`, but it weakens the collision-safety property the new path is relying on.

Concrete fix: when `decision_date.is_some()`, accept only results with a matching remote `decision_date`; do not populate `date_agnostic` for missing remote dates. Keep the date-agnostic fallback only for cases where the local metadata has no date.

## Verified Areas

- The v12 migration keeps enrichment in a separate `decision_zones` table with a primary-key FK back to `documents`, so it does not mutate canonical decision records or corpus-level provenance.
- The write path uses parameterized Postgres queries for the cache upsert; read helpers quote `document_id` with `sql_string_literal`, so I did not find an SQL-injection issue in the changed SQL.
- `normalize_judilibre_zones` slices via a `Vec<char>` and validates `start <= end <= text_chars.len()`, so the reviewed offset slicing is character-safe for the stated Judilibre offset unit.
- `fetch --part --online` only attempts live Judilibre enrichment for Cassation decisions and only for the zone-backed parts (`motivations`, `moyens`, `dispositif`). Summary and visa keep the existing fallback behavior.
- The official part block is per-decision/per-part (`zone_accurate: true`, `official_zones: true`) and the existing bulk corpus honesty path still reports DILA jurisprudence sources as `zone_accurate=false`.
- Judilibre text is returned as upstream text; matching uses legal identifiers (`case_numbers`/pourvoi and date), so I did not find a new pseudonymisation transformation issue in the changed path.
- Judilibre client query parameters are passed through `ureq` query APIs rather than manual URL concatenation.

VERDICT: FIXES_REQUIRED
