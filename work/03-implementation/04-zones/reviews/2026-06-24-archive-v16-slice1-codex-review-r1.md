# Code Review - Zone rollout slice 1 archive v16

Scope reviewed: commit `d7040d136d29cca021bc417bf0bbede1e2e59a49` ("Zone rollout slice 1: durable official-API response archive (v16)").

## Findings

No BLOCKER/WARN/NIT findings.

## Review Notes

- `crates/jurisearch-official-api/src/lib.rs`: the new `judilibre_search_params_exchange` and `judilibre_decision_exchange` paths preserve the raw body for the final upstream result on success and HTTP error, keep parsed JSON when available, classify transport/HTTP failures as `upstream_error`, and emit a durable missing-credential exchange instead of dropping the attempt.
- `crates/jurisearch-cli/src/main.rs`: Judilibre enrichment archives the `/search` exchange before interpreting it, archives `/decision` before zone normalization/cache writes, records the no-pourvoi path through a `local` unsupported row, and keeps the existing `decision_zones` status/TTL behavior otherwise unchanged.
- `crates/jurisearch-storage/src/official_api_archive.rs`: the insert helper is parameterized, casts JSON through `$n::text::jsonb`, returns the append-only `response_id`, and does not upsert or impose an FK back to `documents`.
- `crates/jurisearch-storage/src/migrations.rs`: migration v16 is contiguous with `CURRENT_SCHEMA_VERSION = 16`, creates the archive table without document FKs, stores raw body plus parsed JSON plus sha256, and adds targeted lookup indexes without creating a standalone BM25/vector-style index.
- Archive insert failures are hard errors at the decision level. In eager backfill, `enrich_zone_page_concurrently` maps those per-decision errors to `ZoneEnrichOutcome::Error`, so one archive failure does not abort the whole page/run.

I did not rerun the cargo suites during this review; this pass was source/diff focused against the requested commit and the validation already listed in the brief.

VERDICT: GO
