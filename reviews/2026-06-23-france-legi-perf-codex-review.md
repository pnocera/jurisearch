# Code Review: France-LEGI Gold Extraction Speedup + Durable PG Profile

Reviewed commit: `5480388` (`Speed up France-LEGI gold extraction and tune durable PG profile`)

## Findings

No P0/P1/P2 findings.

## Review Notes

### JSONB `@>` equivalence

The containment rewrites in [crates/jurisearch-storage/src/france_legi.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/france_legi.rs:100) preserve the old predicate semantics for the modeled `attributes` array shape.

For temporal gold, `e.payload->'attributes' @> '[{"key":"debut"},{"key":"fin"},{"key":"num"},{"key":"etat"}]'::jsonb` is equivalent to the previous `jsonb_object_agg(... ) ?& ARRAY[...]` requirement that all four keys are present somewhere in the attributes array. Both reject null or empty attributes in the `WHERE` predicate; the old `coalesce(..., '[]')` produced no keys, while the new `NULL @> ...` evaluates to null and is filtered out.

For cross-reference gold, `@> '[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb` keeps the old two-`EXISTS` behavior: key and value must be on the same attribute element for each predicate, while the `typelien` and `sens` matches may be different elements. Superset elements remain accepted, matching the prior `a->>'key' = ... AND a->>'value' = ...` logic.

### Temporal semantics

The new `resolved` CTE in [crates/jurisearch-storage/src/france_legi.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/france_legi.rs:171) reproduces the old output columns and filters: `from_document_id`, seed `from_source_uid`, resolved gold document/version fields, and the same well-formed validity-window filter. The `graph_edges` filters re-applied during resolution match the `candidate_seeds` filters exactly, so the downstream `families`, `family_keys`, `chosen_families`, and `cases` logic remains semantically unchanged.

The loose `count(*) >= 2` pre-filter can admit a seed whose matching edges collapse to one distinct resolved version, but [the existing `families` guard](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/france_legi.rs:203) still requires `count(DISTINCT gold_document_id) >= 2` and self-inclusion. That makes the loose pre-filter a possible wasted seed slot, not an invalid gold producer.

### Sampling and under-fill risk

[P3] The bounded first-N seed pools can under-fill in principle if the earliest document IDs contain many seeds that do not resolve to valid families, cited corpus articles, or chunk-backed query texts. This is not a blocking correctness finding for this commit because the production validation in the review brief returned the full configured caps (`12` temporal, `120` cross-reference), and the France-LEGI artifact gate independently fails below the minimum query floors. Still, the heuristic is not self-healing: a future corpus/order change could require increasing the pool or adding fallback expansion if counts drop.

### Runtime configuration

[crates/jurisearch-storage/src/runtime.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/runtime.rs:432) rewrites `jurisearch.conf` from scratch on each durable open, before `start_pg_ctl`, so restart-only settings such as `shared_buffers` take effect and stale bulk WAL settings do not leak into the durable profile. Bulk-only settings remain confined to the `BulkIngest` match arm; durable gets `shared_buffers = '2GB'` without `synchronous_commit = off`, `wal_compression`, `max_wal_size`, or checkpoint relaxation.

The fixed durable `shared_buffers = '2GB'` plus parallel-worker defaults are operationally heavier on boxes already running multiple Postgres instances, but I do not see a config regression in the target flow reviewed here. If this profile is expected to run on small-memory CI/dev systems, making these knobs configurable would reduce portability risk.

## Validation

I reviewed the diff and source paths requested in `/tmp/codex-france-legi-perf-review.md`. I did not rerun the already-reported cargo or live production-index validation.

VERDICT: GO
