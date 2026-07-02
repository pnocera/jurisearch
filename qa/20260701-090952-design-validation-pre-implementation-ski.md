# Q&A — 20260701-090952

## Question

# Design validation (pre-implementation) — skip the full-corpus replay-snapshot rehash on delta-only cycles

Repo `/home/pierre/Work/jurisearch`. **Read the actual source; don't trust this prose.** Confirm the design is
correct/sufficient or push back, against the real code. Design gate before I implement.

## Problem
The producer's post-ingest **replay-snapshot refresh** (`refresh_replay_snapshot_with_client`,
`crates/jurisearch-storage/src/ingest_accounting/replay_snapshot.rs:72`) does a full-table
`md5(string_agg(md5(row_hash) ORDER BY row_key))` over documents/chunks/publisher-edges/embeddings/index_manifest
on EVERY `producer update` — minutes of CT110 I/O even for a tiny delta. We just shipped delta-only steady-state
ingest (commit 60b46c5). Optimize the snapshot refresh for delta-only cycles WITHOUT weakening any correctness it
provides.

## Proposed design (verify each claim against source)
1. **LOAD-BEARING: the snapshot is a producer-DB-local observability/integrity digest whose signature is NEVER
   machine-compared, and NO consumer requires it to be FRESH.** The investigation found the ONLY reader of the
   stored `index_manifest['replay_snapshot']` is `load_cached_replay_snapshot`
   (`replay_snapshot.rs:162`) via `load_ingest_health_with_replay_snapshot_mode` in `ReplaySnapshotMode::Cached`
   (`health.rs:124`), feeding (a) `jurisearch status` (observability; `--deep` recomputes via `Refresh`,
   `status.rs:95-100,437`) and (b) the Phase-1 release gate (`gates/phase1.rs:77-85`) which passes iff
   `ingest_available && replay_snapshot_status == "available"` — **status/existence only, NEVER the signature
   value or freshness**; `status()` is `"available"` whenever any component count>0 (`replay_snapshot.rs:41-51`).
   **Verify independently and exhaustively:** grep the WHOLE repo for `replay_snapshot`, `'replay_snapshot'`,
   `load_cached_replay_snapshot`, `ReplaySnapshotReport`, `.signature`, `query_readiness` and confirm:
   - publish/verify (`crates/jurisearch-package-build`, `crates/jurisearch-package`, `verify.rs`) does NOT read
     or compare the replay snapshot;
   - the client/syncd (`crates/jurisearch-syncd`) does NOT read it (its "signature"/readiness refs are the manifest
     crypto signature and the read-topology readiness signature, NOT this);
   - `query_readiness`/`readiness.rs` is a SEPARATE mechanism (read-topology cache), not gated on the replay
     snapshot value;
   - nothing auto-compares two snapshot signatures to detect drift (so a stale snapshot neither false-alarms nor
     silently masks drift in any automated path).
   If ANY consumer requires a fresh/accurate snapshot after an update, the skip is unsafe — flag it.
2. **A stale-but-present snapshot after a delta-only cycle breaks nothing.** Because skipping leaves the last full
   snapshot in `index_manifest` untouched, `status()` stays `"available"` and the Phase-1 gate stays `pass`; only
   non-deep `status` shows a stale count under `replay_snapshot_source:"cached"` (which already means "not
   recomputed this run — run --deep"). Confirm the store only happens inside a real refresh (so skip = untouched,
   NOT cleared/emptied — clearing would flip `status()` to `"empty"` and break the gate).
3. **Incremental refresh is infeasible** (order-dependent `string_agg` over all rows; a commutative combiner would
   be a signature-scheme change, out of scope). So SKIP (not scoped-recompute) is the right move. Confirm.
4. **COMPLETENESS: embed ALSO refreshes — both sites must be gated.** `embed`
   (`crates/jurisearch-pipeline/src/embed.rs:225`) runs the same full-corpus refresh, and the producer runs `embed`
   every cycle (`update.rs:324-325`); on any non-empty delta the embed pass would re-hash the whole corpus and
   negate an ingest-only fix. Confirm this is real and that gating BOTH the ingest refresh (`legi.rs:317`,
   `juri.rs:268`) AND the embed refresh (`embed.rs:225`) is necessary and sufficient.
5. **Signal & axis.** Ingest: gate per source on `!incremental` (delta-only ⇔ `filter.incremental==true`). Embed:
   gate on a CYCLE-LEVEL `any_full_scan` (OR across sources) — NOT on "did embed embed anything", because a
   full-scan cycle must refresh even if few chunks changed, and a delta-only cycle must skip even though it embedded
   new chunks. Design threads `full_scan=!incremental` from `ingest_one` via `IngestJournalCoordinate` and computes
   `any_full_scan = ingest_journals.any(full_scan)`. Confirm this axis is correct and that
   rebaseline/new-baseline/cold-cursor cycles all yield `incremental=false` → full refresh still happens (edge cases
   2/3 in the design). Confirm the existing `run_status==Completed` gate on the ingest refresh is preserved (a
   failed run never refreshes), and that `JURISEARCH_SKIP_REPLAY_SNAPSHOT` still composes (additional AND-guard).
6. **Mechanism:** add `refresh_replay_snapshot: bool` to `IngestArchivesRequest` and `EmbedRequest`; all non-producer
   callers (CLI `ingest legi-archives`/`embed-chunks`, tests, loopback) default it `true` to preserve behavior;
   producer sets ingest=`!mode.incremental` and embed=`any_full_scan`. Is adding an explicit policy field the right
   call vs gating directly on the in-scope `filter.incremental` (no struct change)? Any downside to the struct-field
   approach (e.g. a caller that would wrongly default to false)?
7. **Anything MISSED** that would make skipping unsafe or the full-refresh trigger wrong — e.g. a maintenance/doctor
   path, a test that asserts a fresh snapshot after a producer update, the standalone `refresh_replay_snapshot` CLI,
   or a status field an operator/alert depends on.

## Deliver
GO / GO-with-adjustments / NO-GO on the design, grounded in source. Especially adjudicate #1 (no consumer needs
freshness — the whole thing rests on this), #4 (embed site must also be gated), and #5 (the any_full_scan axis).

## Answer

# Design Validation: Skip Full-Corpus Replay Snapshot On Delta-Only Producer Cycles

Verdict: **GO-with-adjustments**.

The optimization is safe in principle. The replay snapshot is a producer/local observability cache, not part of package signing, package verification, syncd apply, or query readiness. A delta-only producer cycle can leave the previous cached snapshot in `index_manifest['replay_snapshot']` without weakening any automated correctness path I found.

The design does need two adjustments:

- Add an explicit refresh policy, but pass it through the existing `maybe_refresh_replay_snapshot` path so `JURISEARCH_SKIP_REPLAY_SNAPSHOT` remains an additional skip condition.
- In producer, derive the cycle-level embed policy from whether any source full-scanned, but recognize the current source: only `EmbedTarget::Chunks` refreshes replay snapshots today; `EmbedTarget::ZoneUnits` does not.

## Source Findings

`refresh_replay_snapshot_with_client` is exactly the expensive full-corpus path described:

- `replay_snapshot.rs:72-78`: `refresh_replay_snapshot_with_client` loads then stores the snapshot.
- `replay_snapshot.rs:80-160`: `load_replay_snapshot` opens a repeatable-read transaction and snapshots `documents`, `chunks`, publisher `graph_edges`, `chunk_embeddings`, and `index_manifest`.
- `replay_snapshot.rs:208-225`: each component uses `md5(coalesce(string_agg(row_hash, E'\n' ORDER BY row_key), ''))`.
- `replay_snapshot.rs:186-206`: storage happens only in `store_replay_snapshot`, called by the refresh path. If refresh is skipped, the existing manifest row is left untouched.

`ReplaySnapshotReport::status()` is presence/count based:

- `replay_snapshot.rs:41-51`: status is `"empty"` only if documents, chunks, publisher edges, and embeddings all have count zero; otherwise `"available"`.

Cached status reads do not recompute:

- `health.rs:123-130`: `ReplaySnapshotMode::Cached` calls `load_cached_replay_snapshot`; `ReplaySnapshotMode::Refresh` calls `refresh_replay_snapshot_with_client`.
- `health.rs:132-136`: status is `"missing"` only when there is no readable cached row; otherwise it calls `snapshot.status()`.
- `status.rs:95-100`: `--deep` maps to `ReplaySnapshotMode::Refresh`; normal status maps to `Cached`.
- `status.rs:437`: status loads ingest health with that mode.

The Phase-1 gate checks only availability:

- `gates/phase1.rs:27-34`: it reads `replay_snapshot_status` and `replay_snapshot_source` for message text.
- `gates/phase1.rs:77-85`: the pass condition is `ingest_available && ingest_health["replay_snapshot_status"] == "available"`. It does not inspect the snapshot signature or freshness.

## Claim 1: Consumer Freshness

Confirmed.

Repo-wide source search shows the only runtime readers of `index_manifest['replay_snapshot']` are the ingest-health/status path above. Package build, package verification, and syncd do not read it:

- `crates/jurisearch-package-build` has no replay-snapshot reader.
- `crates/jurisearch-package` has no replay-snapshot reader.
- `crates/jurisearch-syncd` has no replay-snapshot reader.

The `.signature` fields in `crates/jurisearch-package*` and `crates/jurisearch-syncd` are package/remote-manifest cryptographic signatures, not replay snapshot signatures. `crates/jurisearch-syncd/src/apply.rs` stamps query readiness after apply, and `crates/jurisearch-syncd/src/planner.rs` verifies remote manifest signatures; neither path consumes `ReplaySnapshotReport`.

`query_readiness` is separate:

- `readiness.rs:1-11`: query readiness is a writer/apply-owned readiness stamp scoped to active read topology.
- `readiness.rs:119-181`: the readiness cache/stamp is stored under `index_manifest['query_readiness']`.
- `readiness.rs:445-568`: installed read paths validate the readiness stamp by active-topology signature.
- `replay_snapshot.rs:131`: replay snapshots explicitly exclude `query_readiness` from their `index_manifest` component.

I found no automated path that compares two replay snapshot signatures to detect drift. Tests assert replay snapshot signatures in storage/CLI contract tests, but those are tests of status/cache behavior, not production consumers.

## Claim 2: Stale-But-Present Cache

Confirmed.

Skipping refresh leaves the existing `index_manifest['replay_snapshot']` row untouched because only `store_replay_snapshot` writes it, and that is only called from `refresh_replay_snapshot_with_client`.

That means:

- normal `status` continues to report `replay_snapshot_source: "cached"`;
- `replay_snapshot_status` remains `"available"` if the prior snapshot had nonzero corpus counts;
- the Phase-1 replay gate continues to pass;
- `status --deep` still recomputes and overwrites the cache when an operator wants fresh evidence.

Do not clear the manifest row on skip. Deleting it would make cached health report `source="missing"` and `status="missing"`, which would make the Phase-1 replay gate pending.

## Claim 3: Incremental Refresh

Confirmed for this slice.

The current snapshot signature is order-dependent over all row hashes. Without storing per-row hash state and changing the replay snapshot scheme, there is no cheap scoped recompute. A commutative or rolling scheme would be a new signature contract and would need separate design and migration. Skip is the right minimal optimization.

## Claim 4: Embed Must Be Gated Too

Confirmed, with one source nuance.

The ingest refresh sites are:

- `ingest/legi.rs:317-321`: on completed LEGI ingest, call `maybe_refresh_replay_snapshot`.
- `ingest/juri.rs:268-272`: on completed JURI ingest, call `maybe_refresh_replay_snapshot`.

The embed refresh site is:

- `embed.rs:225`: after chunk embedding and dense rebuild, call `maybe_refresh_replay_snapshot`.

Producer runs both embed targets every cycle:

- `update.rs:323-325`: `embed_pending(... Chunks)` then `embed_pending(... ZoneUnits)`.

So gating ingest only is insufficient. On a non-empty delta that embeds new chunks, `EmbedTarget::Chunks` would still do the full replay snapshot and erase the win.

Nuance: `EmbedTarget::ZoneUnits` currently does **not** refresh replay snapshots (`embed.rs:400-424` returns `replay_snapshot: None` and no `replay_snapshot_cache` field). That means the required embed gate is specifically the existing chunk-embedding refresh site unless you intentionally add replay refresh to zone embedding, which is outside this optimization and would increase work.

## Claim 5: Signal And Axis

The axis is correct: gate on full-scan vs delta-only, not on whether the embed pass inserted rows.

Current delta-only ingest state after commit `60b46c5`:

- `update.rs:442-486`: `choose_ingest_mode` returns `incremental=false` for a source in `new_baselines`, no cursor, or stale cursor failure; otherwise `incremental=true` with `since_compact`.
- `update.rs:522-536`: `IngestArchivesRequest.filter.incremental` is set from that mode.
- `update.rs:538-549`: producer now fails if `report.run_status != Completed`, preserving the completed-run cursor invariant.

The proposed `any_full_scan = ingest_journals.any(full_scan)` is the right cycle-level embed policy, but the current `IngestJournalCoordinate` does not yet carry `full_scan` (`cursors.rs:37-44`). You need to add it or carry the mode in a local producer-side vector.

Edge cases:

- New baseline/rebaseline: source is in `new_baselines`, so `incremental=false`; refresh should run.
- Cold DB / hand-loaded corpus with no completed ingest cursor: `cursor=None`, so `incremental=false`; refresh should run.
- Stale cursor: fails before ingest; no refresh.
- Ordinary steady-state delta-only: `incremental=true`; skip both ingest replay refresh and chunk-embed replay refresh.

Preserve the existing completed-run gate:

```text
run_status == Completed
&& refresh_replay_snapshot
&& !JURISEARCH_SKIP_REPLAY_SNAPSHOT
```

Do not derive embed refresh from `embeddings_inserted > 0`. A full-scan cycle should still leave fresh replay evidence even if few or no chunks needed embedding; a delta-only cycle should skip even if it embedded many new chunks.

One practical detail: because `embed_pending` treats `NoResults` as success (`update.rs:634-637`), a full-scan cycle where chunk embedding has no pending rows will get its replay refresh from ingest, not embed. That matches current behavior. Zone-unit embedding does not refresh today.

## Claim 6: Mechanism

Adding explicit policy fields is the right mechanism:

```rust
pub struct IngestArchivesRequest<'a> {
    ...
    pub filter: ArchiveSyncFilter<'a>,
    pub refresh_replay_snapshot: bool,
}

pub struct EmbedRequest {
    ...
    pub pool_endpoints: Vec<EmbeddingPoolEndpoint>,
    pub refresh_replay_snapshot: bool,
}
```

Why explicit fields are better than directly checking `filter.incremental` inside ingest:

- CLI `sync` also uses `incremental=true`, but the user asked to optimize producer delta-only cycles, not silently change CLI sync behavior.
- Non-producer CLI ingest/embed commands should keep today’s behavior by passing `true`.
- Embed has no archive filter, so it needs an explicit policy anyway.
- Rust struct literals mean new callers must choose a value; there is no silent default-to-false unless you add a `Default` impl. Do not add a default that could accidentally skip refresh.

Implementation shape:

- Change pipeline `maybe_refresh_replay_snapshot` to accept a policy boolean, e.g. `maybe_refresh_replay_snapshot(client, refresh_replay_snapshot)`.
- Return `Ok(None)` when either policy is false or `JURISEARCH_SKIP_REPLAY_SNAPSHOT` is set.
- Keep `replay_snapshot_cache_value(None)` as the report surface, so skipped paths report `{ "source": "skipped" }` where the body includes a cache field.
- Update all non-producer callers to pass `true`:
  - CLI LEGI/JURI archive ingest request constructors;
  - CLI chunk/zone embed request constructors;
  - tests/helper `EmbedRequest` constructors.
- In producer, pass ingest `refresh_replay_snapshot = !mode.incremental`.
- In producer, compute `any_full_scan` from the resolved ingest modes/journals and pass chunk embed `refresh_replay_snapshot = any_full_scan`.

I would pass the policy to both embed targets for a uniform API, but only `EmbedTarget::Chunks` currently consumes it. If you want zone embedding output to continue not mentioning replay snapshots, leave zone behavior as-is.

## Missed Items / Tests

Tests will need updates:

- CLI ingest contract tests currently assert `replay_snapshot_cache.source == "refreshed"` for non-producer commands. Keep those passing by setting request policy `true`.
- Add producer/pipeline tests for the new policy:
  - ingest completed + `refresh_replay_snapshot=false` reports skipped and leaves an existing manifest row untouched;
  - ingest completed + `true` refreshes as today;
  - chunk embed + `false` skips;
  - chunk embed + `true` refreshes as today;
  - `JURISEARCH_SKIP_REPLAY_SNAPSHOT` still overrides a `true` policy.

No package-build, package-verify, syncd, or query-readiness code needs a replay snapshot change.

## Final Verdict

**GO-with-adjustments.**

The core skip is safe because replay snapshots are cached observability evidence, not a signed-package or sync/apply input. Gate both archive-ingest refresh sites and the chunk-embed refresh site. Use a cycle-level `any_full_scan` signal for producer embed, preserve default refresh behavior for CLI/non-producer callers, and never delete the cached `replay_snapshot` row on skip.
