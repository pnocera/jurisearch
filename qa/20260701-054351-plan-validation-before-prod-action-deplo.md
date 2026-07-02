# Q&A — 20260701-054351

## Question

# Plan validation (BEFORE prod action) — deploy the memory-fixed producer + finish the core-1-2 legislation increment

Repo `/home/pierre/Work/jurisearch`. **Read the real source to validate; don't trust my prose.** This is a
pre-action gate: I am about to touch production (CT 111, the producer host). Confirm the plan is safe and correct,
or push back with the specific risk + fix. I will NOT execute until you validate.

## Context (verified read-only)
- Baseline `core-1-1` is published, signed, served, healthy (`active_baseline_id=core-bootstrap-v1`,
  `published_head_sequence=1`); its ~150 GB `.copybin` payload is on a 10 TB CIFS storebox (ample free).
- The producer OOM was in the INCREMENTAL delta build (`build_incremental_inner`), now FIXED and committed on `main`
  as `0858360` ("Stream incremental delta payload…"): each JSONL payload row is streamed through a
  `HashingWriter<BufWriter<File>>`, byte/digest-identical to the old `join` (Codex GO). A fresh bundle is built:
  producer `0.1.0 (085836080051)`, sha256 `974ad6fb23703df8ee81a2139210b8abb10c75e8fe570b9da9f59d55660f979b`.
- On-box producer is the OLD `ed259c4` (has the OOM bug). All producer timers are DISABLED; both producer services
  inactive; no producer process running; 95 GiB free. Dashboard service is the only thing running.
- Stale state from the OOM-killed run `legislation-…-3455`: `last.json` (+ that run's `.record.json`) frozen at
  `outcome:running`; a stranded `core/.staging/pending/payload/` with a partial 8.3 GB `documents.upsert.jsonl` +
  a 309 MB `legi_metadata_roots.upsert.jsonl`. A newer legislation compact `LEGI_20260629-223234` is already
  fetched (fetch cursor), and ~229k `chunks` rows have NULL `embedding_fingerprint` (un-embedded delta).
- Legislation unit: `ExecStart=/usr/local/bin/jurisearch-producer update --config /etc/jurisearch/producer.toml
  --group legislation`, `User=jurisearch`, `EnvironmentFile=/etc/jurisearch/producer.env`. `update` help:
  "(fetch) → ingest → enrich → embed → publish core"; a `--skip-fetch` flag exists.

## Planned actions — validate each

### 1. Deploy the fixed binary WITHOUT arming timers (lightest path; NOT `deploy.sh`, which re-arms timers)
A verified in-place swap of `/usr/local/bin/jurisearch-producer` only — leave units, config, secrets, and the
(disabled) timers exactly as-is:
1. `scp` the new binary to a CT 111 staging path.
2. On box: assert staged `sha256 == 974ad6fb…979b` AND `--version` shows `085836080051`; abort otherwise.
3. `install -m 0755 -o root -g root` the staged binary over the target via a temp file in the same dir + atomic
   `mv` (services are inactive, so no stop needed; confirm nothing is running first).
4. Re-assert installed `sha256` + `--version`. Do NOT touch timers/units/config/provision.

**Verify:** is a bare binary swap (no `install`/`provision-db`/unit re-render) safe given the units already exist
and the fix changes only in-process behavior (no new config keys, no schema/migration change, no unit contract
change)? Anything in the new binary that REQUIRES a re-provision or unit re-render before it can run correctly?

### 2. Clean up stale state
- `rm -rf /srv/jurisearch/storebox/packages/core/.staging/pending/` (the stranded, UN-cataloged delta slot).
- Finalize the stale `running` RunRecord so `status`/the dashboard stop showing the dead run as in-progress.

**Verify against source:** Is manually removing the `.staging/pending/` slot safe, or does the next incremental
run already discard+rebuild it (so I should leave it)? Is there ANY catalog/lock/cursor that points at that pending
slot such that deleting it breaks resume? For the stale RunRecord — is it safe to leave it (the next run overwrites
`last.json`), or should I rewrite that record's outcome to a terminal state, and if so what's the correct schema
(so I don't write a malformed record the producer/dashboard chokes on)? Point me at the RunRecord writer/reader.

### 3. Run the increment, monitored, with an abort threshold
Trigger a one-shot via `systemctl start jurisearch-producer-legislation.service` (NOT the timer — arms nothing).
Watch `free`/RSS; if memory climbs toward ~70 GiB, `systemctl stop` the service and abort.

**Verify against source:**
- Does `update --group legislation` on the CURRENT state take the INCREMENTAL path (produce `core-1-2`), given
  `rebaseline_pending=false` and the already-published `core-1-1`? Confirm it will NOT trigger a full rebaseline.
- Given the delta is already fetched/ingested and ~229k chunks are un-embedded: will `embed` only process the
  un-embedded (fingerprint-NULL) rows (bounded, ~GBs), or re-embed everything? Will re-running with the DEFAULT
  ExecStart (no `--skip-fetch`) re-fetch/re-ingest idempotently and safely? Is `--skip-fetch` materially better
  here (and if so, is a transient `systemctl edit`/manual run worth it, or is plain `start` fine)?
- With `0858360`, is the incremental PUBLISH now bounded (peak a few GB, streamed per-row)? Any OTHER step
  (enrich/embed, the O(scopes) coalescing I deliberately deferred, the CT110 content-digest `string_agg`) that
  could still approach 96 GiB for this legislation delta?
- On success: exactly which signals confirm it (`status.published_head_sequence` → 2, `core-1-2` cataloged
  `status=published`, `last_outcome` terminal-success, served `manifest.json` head_sequence 2)? Does the producer
  self-verify the built package before promoting to the served root (so a bad build fails closed without corrupting
  `core-1-1`)?
- Is there any risk to the SERVED `core-1-1` if this run fails midway (staged-then-atomic-promote)? Confirm the
  served baseline is untouched on failure.

## Deliver
A GO / GO-with-adjustments / NO-GO on the plan, with any corrected command/step and the precise success + abort
signals to watch, grounded in the source.

## Answer

# Plan Validation: deploy fixed producer and finish `core-1-2`

## Verdict

**GO-with-adjustments.**

The plan is operationally safe for the served `core-1-1` baseline and is the right way to recover the legislation incremental, but I would tighten three points before touching CT 111:

1. Before deleting `.staging/pending`, confirm it is truly an incomplete/uncataloged build: no staged `manifest.json`, and no `package_catalog` row in `status='built'` for the staged package. If it is only the partial `payload/` directory described, deletion is safe and the next run would clean it anyway.
2. Do not rely on the ordinary incremental cycle to do a full public-verifier post-publish check before exposing `core-1-2`; it does not. It publishes atomically and then rebuilds the signed remote manifest from catalog/published embedded manifests, but a separate read-only `jurisearch-package verify` after success is the explicit full-root verification gate.
3. `systemctl start jurisearch-producer-legislation.service` is safe. `--skip-fetch` is marginally cleaner because the compact is already fetched, but not worth transiently changing the unit unless you specifically want to avoid the network listing/download phase.

## 1. Binary Swap

**GO.** A bare in-place binary swap is safe here.

Source checks:

- The rendered service contract is just `ExecStart={binary} update --config {cfg} --group {group}` (`crates/jurisearch-producer/src/render.rs`), matching the existing unit.
- The update path loads the same config and constructs `UpdateOptions` from CLI flags; no new unit field is needed for the fixed incremental writer (`crates/jurisearch-producer/src/bin/jurisearch_producer.rs:83-97`, `:265-277`).
- `run_update_inner` only checks that the DB is provisioned via `public.documents` before mutating (`crates/jurisearch-producer/src/update.rs:253-257`, `:392-410`). It does not require a unit re-render or provisioning pass.
- Commit `0858360` changed package/incremental/canonical code and tests, not migrations or producer config shape.

Use the in-place swap plan, with these prechecks:

```bash
systemctl is-active jurisearch-producer-legislation.service jurisearch-producer-jurisprudence.service
pgrep -a jurisearch-producer || true
sha256sum /path/to/staged/jurisearch-producer
/path/to/staged/jurisearch-producer --version
```

Then install via temp file in the target directory and atomic rename:

```bash
install -m 0755 -o root -g root /path/to/staged/jurisearch-producer /usr/local/bin/.jurisearch-producer.new
mv -f /usr/local/bin/.jurisearch-producer.new /usr/local/bin/jurisearch-producer
sha256sum /usr/local/bin/jurisearch-producer
/usr/local/bin/jurisearch-producer --version
```

Abort unless the staged and installed binary both match sha256 `974ad6fb23703df8ee81a2139210b8abb10c75e8fe570b9da9f59d55660f979b` and version `085836080051`.

Do not run `deploy.sh`, `provision-db`, daemon-reload, enable, or timer actions for this fix.

## 2. Stale Staging And RunRecord

### `.staging/pending`

**Safe to remove, with one precheck.**

The source says the next cycle would also clean this up:

- `producer_cycle` first calls `resume_pending`, then unconditionally `clean_dir(&pending)` and recreates it before a fresh build (`crates/jurisearch-package-build/src/cycle.rs:151-168`).
- `resume_pending` only resumes when it can read a staged manifest and finds a matching catalog row; if there is no manifest it returns `None`, and if the manifest is uncataloged it deletes the pending slot (`cycle.rs:386-405`).

So a stranded `payload/` without `manifest.json` is not resumable and can be removed manually. The manual removal is not required for correctness, but it reclaims the 8.6 GB immediately.

Before `rm -rf`, check:

```bash
P=/srv/jurisearch/storebox/packages/core/.staging/pending
test ! -e "$P/manifest.json"
```

Also check there is no unpublished catalog row for a staged package. Use the box's normal DB access from the producer environment; the exact wrapper depends on CT 111, but the query is:

```sql
SELECT package_id, package_kind, package_sequence, status
FROM package_catalog
WHERE corpus = 'core' AND status = 'built'
ORDER BY package_sequence;
```

If there is a cataloged built incremental with a staged manifest, do not delete it; let `resume_pending` publish it or investigate. For the described OOM state, the partial payload files are uncataloged and deletion is safe:

```bash
rm -rf /srv/jurisearch/storebox/packages/core/.staging/pending/
```

### Stale `RunRecord`

Producer correctness does not depend on editing the stale run record.

Source checks:

- `RunRecord::started` writes `outcome="running"` and `exit_class="running"` at run start (`crates/jurisearch-producer/src/runrecord.rs:69-89`).
- `RunRecord::finish` stamps `ended_at`, `exit_class`, `outcome`, and `error` (`runrecord.rs:91-101`).
- `RunRecord::save` atomically writes both `<run_id>.record.json` and `last.json` (`runrecord.rs:115-125`).
- `run_update` writes a new start record before doing work, then overwrites it at success/failure (`crates/jurisearch-producer/src/update.rs:136-173`).
- Dashboard runs provider reads only `*.record.json`; it ignores `last.json` for the runs list, while status uses `last.json`.

If you leave the stale record alone, the next run will overwrite `last.json` immediately. The old `.record.json` will still exist in history as a running/crashed record, which is observationally annoying but not harmful.

If you want the dashboard clean before the new run, edit both the stale `<run_id>.record.json` and `last.json` with the normal persisted snake_case schema. Use a known failure class; `publish-failed` is the closest source-level class for an OOM during package build/publish.

Shape:

```json
{
  "ended_at": "2026-07-01T...Z",
  "outcome": "failure",
  "exit_class": "publish-failed",
  "error": "previous run was OOM-killed during incremental package build; finalized manually before rerun"
}
```

Do not invent a new outcome such as `aborted`; `RunOutcome` only accepts `running`, `success`, and `failure` (`runrecord.rs:27-37`).

## 3. One-Shot Run

### Incremental vs Rebaseline

**Given `rebaseline_pending=false`, this run takes the ordinary incremental path and should produce `core-1-2`.**

Source checks:

- Automatic routing is recomputed under the update lock from fetch cursor + adopted baseline markers (`update.rs:258-286`).
- It rebaselines only if `force_rebaseline` or a pending newer baseline is found and auto mode allows it (`update.rs:292-327`).
- Otherwise it calls `ensure_incremental_may_proceed` and then `producer_cycle` (`update.rs:361-363`).
- `group_run_kind` returns `Incremental` when no source has a pending fetched-vs-adopted baseline (`crates/jurisearch-producer/src/baseline.rs:151-181`).

So the plan relies on the verified state `rebaseline_pending=false`. Recheck it immediately before the run:

```bash
/usr/local/bin/jurisearch-producer status --config /etc/jurisearch/producer.toml --json
```

Confirm the legislation group has `rebaseline_pending: false` and the top-level `published_head_sequence` is still `1`.

### Fetch/Ingest/Embed

Plain `systemctl start` is safe.

Fetch:

- Default `update` runs fetch first unless `--skip-fetch` is set (`update.rs:204-213`).
- Fetch selection is by persisted fetch cursor; already-fetched archives are reported as `already_present`, and only new entries are downloaded (`crates/jurisearch-fetch/src/engine.rs:112-138`, `:145-188`).

Ingest:

- Ingest uses the mirrored archive directory and the ingest journal for de-duplication; it is not keyed by package sequence (`update.rs:413-443`).

Embed:

- The production no-limit path pages pending chunks/zone units in pages of `EMBED_STREAM_PAGE_SIZE = 20_000` (`crates/jurisearch-pipeline/src/embed.rs:171-205`, `:345-382`; `crates/jurisearch-pipeline/src/lib.rs:116`).
- The pending query selects missing or mismatched embeddings, not every already-current row (`crates/jurisearch-storage/src/dense.rs:85-114`; `crates/jurisearch-storage/src/zone_units.rs:380-403`).
- The producer uses batch size 32 and pool concurrency 4 (`crates/jurisearch-producer/src/update.rs:48-49`, `:514-520`).

`--skip-fetch` would avoid a network listing/download phase and use the existing cursor (`update.rs:207-210`), so it is cleaner if you are certain the compact is already fetched. But the default service start is idempotent and keeps you on the exact installed unit path. I would use `systemctl start` rather than edit the unit.

### Remaining Memory Risks

The fixed incremental writer is present in `0858360`:

- Base upserts/deletes stream row-by-row through `JsonlOpWriter` (`crates/jurisearch-package-build/src/incremental.rs:260-289`).
- Graph edges stream per document (`incremental.rs:291-312`).
- Replace sets stream one materialized set at a time in fixed group order (`incremental.rs:314-346`).
- `JsonlOpWriter` lazy-opens files, writes compact JSON + newline, flushes, finalizes the `HashingWriter`, and records digest/metadata (`incremental.rs:640-739`).
- `HashingWriter` hashes only bytes accepted by the inner writer and shares the canonical `sha256:` formatter (`crates/jurisearch-package/src/canonical.rs:99-150`).

The deferred O(scope) coalescing remains:

- `scopes_changed_for_corpus_with_client` still fetches the whole scope window into memory (`crates/jurisearch-storage/src/outbox.rs:243-276`).
- `build_incremental_inner` still coalesces keys in `BTreeMap`/`BTreeSet` before streaming.

For this described delta, that is not plausibly a 96 GiB Rust RSS term. It is strings/sets over changed scopes, not payload bodies. The 8.3 GB `documents.upsert.jsonl` OOM class is addressed.

Other steps:

- Embed is paged in Rust, but finalizes by rebuilding ivfflat indexes over full embedding tables. That is DB-side time/I/O/shared-memory work, not expected producer RSS.
- `corpus_table_digests_with_client` still uses `string_agg`, but over per-row `md5(sig)` fixed-size strings, with source comments estimating about 95 MB at 2.88M rows (`outbox.rs:380-396`).
- Replay snapshot refresh also aggregates fixed 32-byte row hashes (`crates/jurisearch-storage/src/ingest_accounting/replay_snapshot.rs:208-225`). It can take time and DB work, but it is not the old multi-GB raw JSON aggregation shape.

The 70 GiB abort threshold is reasonable. If RSS approaches it, stop the service and treat the run as failed:

```bash
systemctl stop jurisearch-producer-legislation.service
```

After a manual stop, expect that the producer may not write a terminal `RunRecord` because SIGTERM is not caught by `run_update`'s error path. Verify no producer process remains, check/clean `.staging/pending` as above, and finalize the run record manually if needed.

## Success Signals

Expected terminal success class for legislation is `published` if `core-1-2` is built. `published-enrich-degraded` is only for an attempted Judilibre enrichment skip; legislation sources do not enter the Judilibre enrichment branch, so `published` is the normal class (`update.rs:114-122`, `:446-490`).

Check all of these:

1. Service exited successfully:

```bash
systemctl is-active jurisearch-producer-legislation.service
systemctl show jurisearch-producer-legislation.service -p Result -p ExecMainStatus
```

For a completed oneshot, inactive with `Result=success` and `ExecMainStatus=0` is expected.

2. Producer status:

```bash
/usr/local/bin/jurisearch-producer status --config /etc/jurisearch/producer.toml --json
```

Expect:

- top-level `published_head_sequence: 2`
- `active_baseline_id: "core-bootstrap-v1"`
- legislation group `last_outcome: "success"`
- legislation group `last_exit_class: "published"`
- `rebaseline_pending: false`
- `update_lock_held: false`

3. Run record:

The newest `state_dir/runs/legislation/last.json` should have:

- `outcome: "success"`
- `exit_class: "published"`
- `published_package: "core-1-2"`
- `package_high_water_mark.head_sequence: 2`

4. Catalog:

```sql
SELECT package_id, package_kind, package_sequence, status
FROM package_catalog
WHERE corpus = 'core'
ORDER BY package_sequence;
```

Expect `core-1-2`, `incremental`, sequence `2`, `status='published'`.

5. Served manifest:

Read `/srv/jurisearch/storebox/packages/core/manifest.json` and confirm:

- `payload.head_sequence` is `2`
- `payload.packages[]` contains `package_id: "core-1-2"` with `from_sequence: 1`, `to_sequence: 2`
- `payload.active_baseline.baseline_id` remains `core-bootstrap-v1`

6. Full root verification:

Because ordinary `producer_cycle` does not run the public verifier before remote-manifest publish, run the read-only verifier after success:

```bash
jurisearch-package verify \
  --root /srv/jurisearch/storebox/packages \
  --corpus core \
  --public-key-hex '<producer-public-key-hex>' \
  --key-id producer-k1 \
  --key-epoch 1 \
  --uri-base '<configured package.uri_base or media:// default>'
```

The CLI says this checks the published manifest signature/corpus and every referenced artifact's existence, sha256, and signature (`crates/jurisearch-package-build/src/bin/jurisearch_package.rs:57-75`, `:285-306`).

## Failure Safety For `core-1-1`

The served `core-1-1` baseline is not modified by this run.

Source checks:

- Fresh incrementals build into `root/<corpus>/.staging/pending`, not into the served `packages/<id>` directory (`cycle.rs:151-168`).
- `publish_package` copies the staged artifact into a sibling `.tmp` and renames it to the new immutable `packages/<package_id>` path; it never removes an existing live package and refuses divergent republish of the same id (`crates/jurisearch-package-build/src/publish.rs:40-80`).
- The remote manifest is written via `manifest.json.tmp` then renamed (`publish.rs:91-109`).
- `build_remote_manifest` reads already-published artifacts and verifies catalog identity before creating the new manifest (`crates/jurisearch-package-build/src/remote_manifest.rs:93-153`, `:227-260`).

If the run fails before publishing `core-1-2`, `core-1-1` remains served and the old remote manifest remains the visible head. If it fails after publishing the package but before publishing the remote manifest, the new artifact may exist under `packages/core-1-2`, but clients will not see it until `manifest.json` advances. If it fails after manifest publish, the success checks and read-only verifier above should catch any inconsistency.

## Corrected Action Plan

1. Swap only `/usr/local/bin/jurisearch-producer`, with sha/version checks before and after. Leave timers disabled.
2. Recheck status JSON: no active producer run, `published_head_sequence=1`, legislation `rebaseline_pending=false`.
3. Clean `.staging/pending` only after confirming no staged manifest and no built catalog row.
4. Either leave the stale run record alone, or mark both stale files terminal `failure`/`publish-failed` with the existing schema.
5. Run:

```bash
systemctl start jurisearch-producer-legislation.service
```

6. Monitor RSS/free space/journal. Abort at sustained approach to ~70 GiB RSS.
7. On success, verify status, run record, catalog, served manifest, and run `jurisearch-package verify`.

With those adjustments, this is a **GO** for production action.
