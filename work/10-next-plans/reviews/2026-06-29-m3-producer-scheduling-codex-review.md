# Codex Review: M3 Producer Scheduling

## Findings

### BLOCKER: `rebaseline_cycle` is not resumable and can advance the catalog over a missing or unpublished artifact

`crates/jurisearch-package-build/src/cycle.rs:225` enters the shared pending slot, immediately deletes it at `crates/jurisearch-package-build/src/cycle.rs:229`, then builds a new rebaseline with `build_rebaseline` at `crates/jurisearch-package-build/src/cycle.rs:231`. That builder writes the embedded manifest before inserting a `status = 'built'` catalog row at `crates/jurisearch-package-build/src/baseline.rs:471` and `crates/jurisearch-package-build/src/baseline.rs:475`. Unlike `producer_cycle`, the rebaseline path never calls `resume_pending` before cleaning the pending slot.

That leaves a real crash window:

- If the process dies after the `built` catalog row is inserted but before `publish_package` at `crates/jurisearch-package-build/src/cycle.rs:236`, the next rebaseline deletes the only staged artifact. `build_rebaseline_locked` reads the newest catalog row via `latest_package_for_corpus` at `crates/jurisearch-package-build/src/baseline.rs:223`, and that query does not filter out `status = 'built'` rows (`crates/jurisearch-storage/src/package_catalog.rs:155`). The next run can therefore chain from an unpublished/missing `core-1-2` and build `core-2-3`, advancing the producer catalog over a package that was never served.
- If the process dies after `publish_package` but before `mark_package_published` at `crates/jurisearch-package-build/src/cycle.rs:237`, the next run still treats the `built` row as the latest chain head, builds another rebaseline, and leaves the previous rebaseline in an inconsistent status.

This violates the non-negotiable "exactly-once / no-partial-publish" gate for `rebaseline_cycle`. The existing gated test at `crates/jurisearch-package-build/tests/rebaseline_cycle_loopback.rs:122` only covers the happy path, so it would not catch this.

Actionable fix: make rebaseline publishing use the same resume discipline as `producer_cycle`: inspect the pending slot before deleting it, publish/mark the same `package_id` when a staged artifact exists, and only then build a new package if no pending package exists. Also make the catalog chain/head readers ignore non-`published` rows where appropriate, or introduce explicit `latest_published_package_for_corpus` semantics for building and manifest publication. Add a rebaseline fault seam/test equivalent to `producer_cycle_faulted` that fails after stage-before-publish and proves the next run publishes the same package rather than building a successor.

### WARN: Automatic rebaseline routing is decided before the core lock and is not rechecked under the lock

`run_update_inner` computes `run_kind`, `new_baselines`, and `do_rebaseline` before acquiring `update-core` at `crates/jurisearch-producer/src/update.rs:216` through `crates/jurisearch-producer/src/update.rs:236`. If two group timers fetch the same new baseline close together, both can decide that adoption is pending before either holds the lock. The first run can publish the rebaseline and write the adoption marker at `crates/jurisearch-producer/src/update.rs:279`; the second run then acquires the lock with stale `new_baselines` and runs another rebaseline for the same upstream baseline.

The lock still serializes DB mutation, but the adoption decision is stale at the point where mutation begins. This makes automatic adoption not exactly once under overlap and can create redundant rebaseline packages.

Actionable fix: after acquiring `update-core`, recompute `group_run_kind` from the current adoption markers and decide the rebaseline/manual-incremental path from that locked state. Keep the ordinary incremental backstop inside the lock as well.

### WARN: Rendered systemd units are documented as absolute-path only, but validation does not enforce it

The renderer interpolates configured paths directly into `EnvironmentFile`, `ExecStart`, and `ReadWritePaths` at `crates/jurisearch-producer/src/render.rs:33` through `crates/jurisearch-producer/src/render.rs:68`. The semantic validation in `ProducerConfig::validate` checks corpus, retention, groups, embedding dimension, and secret permissions at `crates/jurisearch-producer/src/config.rs:321` through `crates/jurisearch-producer/src/config.rs:375`, but it does not reject relative `install.unit_dir`, `install.binary_path`, `install.config_path`, `install.environment_file`, or producer data paths.

The tests assert absolute paths only for the rewritten example config (`crates/jurisearch-producer/tests/scheduling_render.rs:40` and `crates/jurisearch-producer/tests/scheduling_render.rs:75`), so a config with relative paths would still validate and render units that violate the acceptance gate.

Actionable fix: add validation that every path rendered into a unit is absolute, with a test that a relative value is rejected before rendering.

### WARN: `status --json` can report `current` for a stalled but last-successful producer

`build_status` derives `behind` only from pending rebaseline markers, running records, and never-run groups at `crates/jurisearch-producer/src/status.rs:145` through `crates/jurisearch-producer/src/status.rs:175`. It does not use the configured cadence (`crates/jurisearch-producer/src/config.rs:116`) or any freshness threshold for `last_ended_at`, so a producer whose last successful run was days old, with no pending baseline marker and an existing manifest, classifies as `Current`.

That does not satisfy the status gate for making stale/stalled cursor state clear without reading logs. The current status test covers no-runs, failed-run, and pending-baseline cases (`crates/jurisearch-producer/tests/run_record_and_status.rs:109`), but it would not catch an old successful cursor/run.

Actionable fix: persist/check a freshness signal per group, such as last successful fetch/update age compared with cadence or a configured stale-after threshold, and classify stale when that threshold is exceeded. Add a status test where the last run is successful but stale by age.

### WARN: Run IDs collide for same-group runs started in the same second

`make_run_id` uses only `<group>-<unix_secs>` at `crates/jurisearch-producer/src/update.rs:159` through `crates/jurisearch-producer/src/update.rs:167`. A manual invocation and timer invocation for the same group in the same second will write the same run record path and `last.json`, overwriting observability for one run. This is especially plausible now that starts can overlap before the lock and one run may later report `skipped-lock-held`.

Actionable fix: include nanoseconds, pid, a monotonic suffix, or a UUID in `run_id`, and add a small test that two immediate IDs for the same group differ.

## Notes

I did not rerun the reported cargo gates; this review is based on source inspection of `git diff main`.

VERDICT: FIXES_REQUIRED
