# Review: IVFFlat Auto Lists + Coupled Probes (r2)

## Summary

The diff changes chunk and zone embedding rebuilds so `--index-lists 0` means auto-scale IVFFlat `lists` from the number of indexed vectors, persists `vector_index.default_probes` as `sqrt(lists)` capped to the request probe range, and makes dense chunk/zone retrieval prefer that stored probe default unless a request explicitly passes `--probes`.

The r1 WARN is fixed correctly: `finalize_zone_dense_rebuild` now uses `effective_lists` for the `CREATE INDEX ... WITH (lists = ...)` statement, the zone manifest `vector_index.lists`, and the returned `ZoneDenseRebuildReport.index_lists`. The chunk path has the same consistency.

## BLOCKER

None.

## WARN

None.

## NIT

1. The `--probes` user/eval surfaces still hardcode the old default `4`.

   Evidence: `crates/jurisearch-cli/src/args.rs:176` advertises `default 4`, and `crates/jurisearch-cli/src/eval/generic.rs:576` reports `options.ivfflat_probes.unwrap_or(4)`. With a new manifest default such as `47`, retrieval runs with the manifest value while help/eval metadata still says `4`.

   Recommended fix: update the help text to describe the new default as "index manifest default, fallback 4"; for eval output, report requested and effective probes separately, or expose/read the same manifest default once when dense modes are evaluated.

2. `manifest_default_probes` accepts any positive stored value, even though the rest of the probe contract is `[1, 4096]`.

   Evidence: `crates/jurisearch-storage/src/retrieval/sql.rs:29` filters only `>= 1`, while `recommended_probes` and CLI validation both cap probes at `4096`.

   Recommended fix: change the stored-value filter to `(1..=4096).contains(&probes)` or clamp the stored value before passing it to `effective_probes`.

Verification: reviewed the uncommitted diff and relevant source paths, including the chunk/zone rebuild paths, manifest write/read path, dense query probe selection, and touched tests. Ran `git diff --check`; it reported no issues. I did not run Cargo tests to avoid producing unrelated build artifacts in this file-only review workflow.

VERDICT: GO
