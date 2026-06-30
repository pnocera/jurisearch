# Spike B — REAL producer JSON fixtures (CT 111) — RESULT

De-risking spike: captured live producer JSON from CT 111 (`100.71.35.39`,
`jurisearch-producer 0.1.0 (ed259c4)`) to drive the `shared/` DTOs. Fixtures live in
`./fixtures/*.json` and become `apps/dashboard/fixtures/` in M1.

## 1. Capture commands (exact)

Discovery:
```
ssh root@CT111 'jurisearch-producer status --help'      # → only flag is --config; NO --json/--format (status is JSON-by-default on stdout)
ssh root@CT111 'cat /etc/jurisearch/producer-paths.env' # state_dir=/var/lib/jurisearch-producer, packages(corpora)=/srv/jurisearch/storebox/packages
ssh root@CT111 'cat /etc/jurisearch/producer.toml'      # corpora_dir, state_dir, groups: legislation[legi], jurisprudence[cass,inca,capp,jade]
```

**The `status` invocation that worked** (no `--json` flag exists — stdout IS JSON):
```
jurisearch-producer status --config /etc/jurisearch/producer.toml          # exit 0, empty stderr
```
(The producer-paths.env was sourced first for safety, but `producer.toml` carries absolute
paths so `--config` alone is sufficient; no DB/network touched.)

Fixture captures:
```
# status.json (real)
jurisearch-producer status --config /etc/jurisearch/producer.toml > status.json

# run record (real) — the ONLY record on the box, see §3
scp CT111:/var/lib/jurisearch-producer/runs/legislation/legislation-1782819024-111818663-787.record.json  runrecord-legislation-running-real.json

# served manifest (real Signed<RemoteManifest>)
scp CT111:/srv/jurisearch/storebox/packages/core/manifest.json  manifest.json

# journald (real, NDJSON) — see §4 surprise
ssh root@CT111 'journalctl -u jurisearch-producer-legislation.service -o json -n 50' > journal-legislation.json

# timers (real)
ssh root@CT111 "systemctl list-timers 'jurisearch-producer-*' -o json" > timers.json   # -o json IS supported (Debian 13 / systemd)
```

## 2. Fixtures captured

| File | Real/Synthetic | One-line shape note |
|---|---|---|
| `status.json` | REAL | `jurisearch-producer status` output; snake_case; top: `active_baseline_id, corpus, generated_at, overall(="stale"), published_head_sequence, published_manifest_generated_at, update_lock_held, groups[]`. Per group: `group, sources[], baselines[]{source,state,adopted_baseline,fetched_baseline}, fetch_cursors[]{source,latest_file_name,latest_compact_timestamp}, rebaseline_pending, stale_by_age, last_run_id, last_outcome, last_exit_class, last_error, last_ended_at`. |
| `runrecord-legislation-running-real.json` | REAL | A genuine **in-flight `running`** RunRecord (the live legislation run, stuck `activating`). `outcome=running, exit_class=running, ended_at=null`. Field order matches `RunRecord::started`. |
| `runrecord-running-synthetic.json` | SYNTHETIC | Source-derived from `RunRecord::started` (jurisprudence multi-source). REQUIRED in-flight fixture; byte-shape validated against the real running record (§5). |
| `runrecord-legislation-finished-synthetic.json` | SYNTHETIC | A `published`/`success` finished record (none exist on the box — §3). Models `finish()`: `ended_at` set, `outcome=success, exit_class=published`, populated `package_high_water_mark`/`published_package`/coordinates (struct shapes from `cursors.rs`). Gives M1 a success-path fixture. |
| `manifest.json` | REAL | `Signed<RemoteManifest>` = top-level `{payload, signature}`; `payload` carries the manifest (§6 surprises). |
| `journal-legislation.json` | REAL | NDJSON (`-o json`), **one object per line**; here exactly **1 line** (§4). Fields incl. `__REALTIME_TIMESTAMP`(string µs), `PRIORITY`(string), `MESSAGE`, `UNIT`, `_SYSTEMD_UNIT`. |
| `timers.json` | REAL | JSON array of 2 objects; machine fields `{next,left,last,passed,unit,activates}`, times in **epoch microseconds** (§ timer quirk). |

## 3. Run-record situation on the box (contract-relevant)

- **No finished RunRecord exists anywhere on CT 111.** `runs/jurisprudence/` is absent (that
  group has never run — `status` shows all `last_*=null`). `runs/legislation/` holds exactly one
  record and it is **`running`** (`outcome=running, ended_at=null, exit_class=running`).
- The legislation service is in systemd state **`activating`** (started `2026-06-30T11:30:24Z`,
  never reached `active`) — i.e. a real in-flight/stuck run, exactly the "crashed/in-flight run is
  visible" case `runrecord.rs` documents. So we captured a **REAL `running` record** — stronger
  than the required synthetic. The synthetic is provided anyway (non-optional) and a synthetic
  *finished* record fills the success-path gap M1 needs.
- A sibling `…-787.json` (no `.record`) and `last.json` also exist: `last.json` mirrors the record;
  the bare `…-787.json` is a **`RunCheckpoint`** (`phase:"fetched"`, populated cursors) — a
  different artifact, NOT part of the dashboard's RunRecord contract; not captured.

## 4. journald surprise (Logs panel)

`journalctl -u jurisearch-producer-legislation.service -o json -n 50` returns **only 1 line** —
the systemd lifecycle message `Starting jurisearch-producer-legislation.service…`. **The producer's
own detailed logs do NOT go to journald**; they go to a log file (`JURISEARCH_PRODUCER_LOG_DIR=
/var/log/jurisearch-producer`). journald for the unit carries unit lifecycle only.

Two consequences the `shared/` `LogLineDTO` + LogsProvider must handle:
- **`PRIORITY` is a string** (`"6"`) and **`__REALTIME_TIMESTAMP` is a string of microseconds** —
  not numbers. Parse/convert accordingly.
- **Unit field is split:** the systemd-emitted lifecycle line carries the unit in **`UNIT`**
  (`=jurisearch-producer-legislation.service`) while `_SYSTEMD_UNIT=init.scope`. Lines emitted *by*
  the service would instead put the unit in `_SYSTEMD_UNIT`. DTO must coalesce
  `_SYSTEMD_UNIT ?? UNIT`. (Design §4 named only "unit" from `render.rs` — make it a coalesce.)
- The Logs page will be **sparse** off journald alone. (Phase-1 note, not a blocker; reading the
  on-disk log file is a possible future source.)

## 5. Running synthetic ⇄ `RunRecord::started` validation

`RunRecord::started` (`runrecord.rs:70-89`) serializes (serde field-declaration order) as:
`run_id, group, sources, kind, started_at, ended_at(=null), outcome(=Running→"running"),
exit_class(="running"), error(=null), fetch_cursors([]), ingest_journals([]),
package_high_water_mark(=null), published_package(=null), adopted_baselines([])`.

Validated programmatically: `runrecord-running-synthetic.json` has the **same keys in the same
order** as the REAL captured running record, and **both** satisfy every in-flight invariant above
(ended_at null, outcome/exit_class "running", error null, all three coordinate arrays empty,
HWM/published null). Confirmed: the synthetic's shape exactly matches `RunRecord::started`.

## 6. Manifest / `Signed<>` surprises (Packages panel)

- Wrapper is `{ "payload": {...}, "signature": "..." }` (top-level signature over the payload).
- **`payload.packages` is currently EMPTY (`[]`)** — only `payload.active_baseline` exists
  (`core-bootstrap-v1`, no increments published yet). `PackageDTO`/PackagesProvider MUST handle a
  zero-increment manifest.
- `payload` carries MORE fields than design §4 listed (forward-compat, ignore): `manifest_version,
  min_available_sequence, catchup_ranges, catchup_policy, entitlement, signing` (plus the expected
  `generated_at, publisher, corpus, environment, head_sequence, active_baseline, packages`).
- `active_baseline` carries extra fields beyond the design allow-list: `minimum_client_version,
  artifact_uri, estimated_load_seconds, signature` (alongside `baseline_id, generation,
  package_kind, sequence, schema_version, sha256, compressed_size_bytes, uncompressed_size_bytes`).
  All snake_case. `digest` is correctly absent (design noted it's not on `RemotePackageEntry`).

## 7. Timer JSON quirk (Timers/Overview)

`systemctl list-timers -o json` returns the **machine** schema, not the human columns:
`[{next,left,last,passed,unit,activates}, …]`. `next/last` are **epoch MICROSECONDS** (integer; `0`
when none). There is **no `active` boolean and no `group`**. `TimerDTO` mapping: `timerUnit=unit`,
`serviceUnit=activates`, `nextRun=next`, `lastRun=last`; **derive `group`** from the unit name
(`jurisearch-producer-<group>.timer`); convert µs→ms for JS Dates.

## 8. Complete exit-class set (from `exit.rs` + `error.rs` + `runrecord.rs`)

Cross-checked against source. The `ExitClass` table MUST cover all of these:

| Class string | Bucket | Source |
|---|---|---|
| `ok` | **success** | `SUCCESS_CLASSES` (generic admin/read success) |
| `published` | **success** | `SUCCESS_CLASSES` |
| `published-enrich-degraded` | **success** (warning) | `SUCCESS_CLASSES` |
| `no-op` | **success** | `SUCCESS_CLASSES` |
| `rebaselined` | **success** | `SUCCESS_CLASSES` |
| `dry-run` | **success** | `SUCCESS_CLASSES` |
| `running` | **running** (in-flight, neutral) | `RunRecord::started` (`runrecord.rs`) — NOT in exit.rs |
| `fetch-failed` | failure / transient(75) | `ProducerError::Fetch` |
| `upstream-unreachable` | failure / transient(75) | `exit_code_for` only (not emitted by `ProducerError::class` today) |
| `skipped-lock-held` | failure / transient(75) | `ProducerError::LockHeld` |
| `integrity-failed` | failure / data(65) | (mapped by `exit_code_for`; emitted via Build/Fetch paths) |
| `producer-db-unprovisioned` | failure / unprovisioned(69) | `ProducerError::Unprovisioned` |
| `config-invalid` | failure / config(78) | `ConfigRead/ConfigParse/ConfigInvalid/Secret/UnknownGroup/UnknownSource/NothingToRebaseline` |
| `ingest-failed` | failure / permanent(70) | `ProducerError::Ingest` |
| `enrich-degraded` | failure / permanent(70) | `ProducerError::Enrich` |
| `embed-failed` | failure / permanent(70) | `ProducerError::Embed` |
| `publish-failed` | failure / permanent(70) | `ProducerError::Build` |
| `provision-failed` | failure / permanent(70) | `ProducerError::Provision` |
| `storage-failed` | failure / permanent(70) | `ProducerError::Storage` |
| `needs-rebaseline` | failure / permanent(70) | `ProducerError::NeedsRebaseline` |
| `alert-hook-failed` | failure / permanent(70) | `ProducerError::AlertHook` |
| `io-failed` | failure / permanent(70) | `ProducerError::Io` |

- **Success (6):** `ok, published, published-enrich-degraded, no-op, rebaselined, dry-run`.
- **Running (1):** `running`.
- **Failure (15):** the rest above.
- **Severity is derived from `outcome` FIRST** (`running`→neutral; success classes→ok; else the
  `exit_code_for` bucket) — exactly as design §4 mandates. The class string alone is NOT enough
  (the `running` trap). Both the real and synthetic running fixtures exercise this.
- Note: `error.rs ProducerError::class` does NOT emit `integrity-failed` or `upstream-unreachable`
  directly (the design lists them under failures) — they exist only in `exit_code_for`'s mapping
  and lower layers. The table must still carry them so it can't drift.

## 9. Other contract notes for `shared/` DTOs

- **All producer JSON is snake_case** (status, run records, manifest payload). DTOs/validators must
  map snake_case→camelCase (or validate snake_case input). RunRecord keys: `run_id, exit_class,
  started_at, ended_at, …`.
- **`status` has no `last_started_at`** — only `last_ended_at`. Run duration must be derived from the
  RunRecord (`ended_at − started_at`), and is **null while running** (both running fixtures have
  `ended_at=null`). The OverviewService join (status × runs) is required to get `started_at`.
- **`update_lock_held=true` while a run is in-flight** — observed (the stuck legislation run holds
  the lock). The lock indicator should not be read as an error on its own.
- Observed enum values this deployment: `overall="stale"`; baseline `state="current"`;
  `kind="incremental"`. (Other documented values not currently present.)
</content>
