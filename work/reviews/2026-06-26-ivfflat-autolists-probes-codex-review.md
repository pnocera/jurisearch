# Code Review: IVFFlat Auto Lists + Probes

## Summary

The change makes `--index-lists` default to `0` for chunk and zone embedding rebuilds, treats `0` as auto-sizing via the new IVFFlat list heuristic, writes `vector_index.default_probes` from the effective list count, and makes dense chunk/zone query paths choose probes in this order: explicit request override, manifest default, legacy fixed fallback.

## BLOCKER

None.

## WARN

- Zone auto-list rebuild reports the raw requested list count instead of the list count actually built. In `finalize_zone_dense_rebuild`, `effective_lists` is used for `CREATE INDEX ... WITH (lists = ...)` and for `index_manifest.vector_index.lists`, but the returned `ZoneDenseRebuildReport` still uses `index_lists: spec.index_lists` at `crates/jurisearch-storage/src/zone_units.rs:537`. With the new CLI default of `--index-lists 0`, `ingest embed-zone-units` serializes that report at `crates/jurisearch-cli/src/ingest/pipeline.rs:437`, so the response says `"index_lists": 0` even though PostgreSQL built at least one list and the manifest records the effective count. This breaks the build-side/reporting consistency the chunk path already preserves in `finalize_dense_rebuild`. Recommended fix: return `index_lists: effective_lists` from `ZoneDenseRebuildReport`, and add a zone-unit rebuild test mirroring the chunk auto-list assertion so `index_lists == 0` reports the resolved list count and manifest `default_probes`.

## NIT

None.

VERDICT: FIXES_REQUIRED
