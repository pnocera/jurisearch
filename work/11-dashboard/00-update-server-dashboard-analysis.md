# Juridia — Update-Server Dashboard — Analysis & Proposal

> Status: PROPOSAL (analysis only — no implementation). Authored 2026-06-30; revised after a Codex review
> (`2026-06-30-dashboard-analysis-codex-review.md`) and a scope decision by the operator.
> **Scope decision:** a SIMPLE, self-hosted, read-only operational dashboard for the **producer**
> (update-server), hosted on **CT 111**, deployed by **`deploy.sh`**, **Tailscale-only / no auth**, with
> **eventual email alerts/reports**. Explicitly **NOT** Grafana/Prometheus and **NOT** the consumer/site side.

---

## 1. What it must show (the operator's ask)

1. **Ingestion process status** — per fetch group (`legislation`, `jurisprudence`): the current/last run —
   running vs done, success/failure, kind (incremental/rebaseline), start/end, freshness vs DILA, next timer.
2. **Packages produced** — the published `core` chain from the served `manifest.json` (active baseline +
   retained increments): package id, package sequence (from/to), size, row counts, schema/fingerprint, digest.
   *(Catalog-only fields — `status`, `published_at`, change-seq window, `package_kind` — are NOT in the served
   manifest; they are Phase-2 DB territory, see §6.)*
3. **Errors reporting** — failed runs with their exact class + message; enrichment-degraded; a stuck lock.
4. **Logs** — recent producer service logs (journald) per group, and the durable run records.
5. **(Eventual) email alerts / reports** — a notification on failure, and a periodic status digest.

Constraints: runs **on CT 111**, installed by **`deploy.sh`**, reachable **only over Tailscale**, **no auth**
(the tailnet ACL is the security boundary), strictly **read-only**, **producer-only**.

---

## 2. Key simplification — most of this needs NO database

The producer already persists everything the four core features need **on the box**, so Phase 1 can avoid
PostgreSQL (and thus the read-grant problem in §6) entirely:

| Feature | On-box source (no DB) |
|---|---|
| Ingestion status / run history / errors / freshness | `jurisearch-producer status` (on-disk JSON) + the durable `RunRecord` files under `state_dir/runs/<group>/` |
| Packages produced | the **served root** `corpora_dir` — `core/packages/<id>/` dirs + the signed `core/manifest.json` (lists active baseline + sequence chain) |
| Logs | `journalctl -u jurisearch-producer-{legislation,jurisprudence}.service` |
| Next run / timer state | `systemctl list-timers` / `is-active` |

PostgreSQL (CT 110) is needed **only** for optional *deep corpus stats* (doc/chunk/zone counts, embedding
coverage %, enrichment outcomes) — deferred to an optional Phase 2 that must first solve the read grant (§6).

---

## 3. Architecture — one small Rust binary, deployed like the rest

A single self-contained service, consistent with the existing Rust, library-first codebase:

- **`jurisearch-dashboard`** (a new small crate, or a `jurisearch-producer dashboard` subcommand) that
  **reuses the producer's own types** — `ProducerStatus`/`build_status`, `RunRecord`, the exit-class helpers —
  so there is **zero schema drift** with what the producer writes.
- Embeds a **tiny HTTP server** rendering **server-side HTML** (a couple of pages, `<meta refresh>` or a
  small poll for live-ish updates). No JS framework, no build step, no external assets required.
- **Binds to the Tailscale interface only** (e.g. `100.71.35.39:<port>` or the `tailscale0` addr), **no auth**.
- Reads: `state_dir` (status + run records), `corpora_dir` (package list + manifest), **journald** for logs
  (via `journalctl` subprocess, or the `sd-journal` API). Runs as `jurisearch` (no root).
- A systemd unit `jurisearch-dashboard.service` (simple, long-running, restart=always) started at boot. It
  **MUST set `SupplementaryGroups=systemd-journal`** (or `deploy.sh` must add `jurisearch` to that group) so it
  can `journalctl -u jurisearch-producer-*.service` — the producer units run `User/Group=jurisearch` with no
  journal access by default (`crates/jurisearch-producer/src/render.rs`), so without this the Logs page would
  fail at runtime.

Why a binary (not a static HTML cron + caddy): logs and "is it running right now" want a live read; a 150-line
Rust service reusing the producer libs is simpler and drift-free than a separate generator + file server, and
it slots into the existing `dist.sh`/`deploy.sh` pipeline with no new toolchain.

### Pages (minimal)
- **Overview** — per group: state (`current`/`stale`/`broken`), last run (outcome, class, kind, when, duration),
  freshness (adopted vs DILA-current baseline, pending-delta lag), `published_head_sequence`, lock held?,
  next timer. Red/amber/green per group.
- **Packages** — table from `core/manifest.json` + `corpora_dir`: package id, sequence (from/to), size, row
  counts, schema/fingerprint, digest, and the manifest `generated_at`; highlight the active baseline.
  (`RemoteManifest`/`BaselineRef`/`RemotePackageEntry` in `crates/jurisearch-package/src/manifest/remote.rs`
  expose exactly these — NOT catalog `status`/`published_at`/change-window/kind, which are Phase 2.)
- **Runs / Errors** — the last N `RunRecord`s per group: kind, outcome, exact `exit_class`, duration, error;
  failures pinned to the top.
- **Logs** — last N journald lines per producer service (filterable by group), auto-refresh.

---

## 4. Deploy integration (`dist.sh` + `deploy.sh`)

Today `dist.sh` bundles only `jurisearch-producer` for the update-server (its `UPDATE_SERVER_BINS`) and
`deploy.sh` hardcodes a single staged/installed binary + one SHA/`--version` check — so the bundle/deploy
become **two-binary**. The concrete script changes:
- **`dist.sh`**: add `jurisearch-dashboard` to `UPDATE_SERVER_BINS` → built into the update-server bundle with
  the same `--version` stamping and audited into `SHA256SUMS`.
- **`deploy.sh`**: stage + install BOTH binaries; verify **both** against `SHA256SUMS` and by `--version`;
  install `/usr/local/bin/jurisearch-dashboard`; write `jurisearch-dashboard.service`
  (`User=jurisearch`, `SupplementaryGroups=systemd-journal`, `Restart=always`, bound to the configured tailnet
  addr) + the `[dashboard]` config; `enable --now`; then **fail-closed verify**: the service is `active`,
  listening **only** on the configured tailnet addr (never `0.0.0.0`), AND `journalctl -u
  jurisearch-producer-legislation.service -n 1` succeeds **under the dashboard identity** (proves journal access).
- Config knobs: `bind` (default the tailnet addr — never `0.0.0.0`), `port`, `state_dir`, `corpora_dir`,
  optional `[dashboard.database]` (Phase 2) and `[dashboard.email]` (Phase 3).

---

## 5. The exit-class & state vocabulary the dashboard must speak (corrected)

Two DISTINCT fields — do not conflate (`crates/jurisearch-producer/src/exit.rs`):
- **`exit_class`** — the EXACT persisted string a run reports. Successes: `published`, `no-op` (empty outbox,
  still success), `rebaselined`, `published-enrich-degraded`, `dry-run`. Failures: `skipped-lock-held`,
  `fetch-failed`, `upstream-unreachable`, `integrity-failed`, `producer-db-unprovisioned`, `config-invalid`,
  `publish-failed`, … **Group runs/panels by this exact string.**
- **Derived severity / exit-code bucket** — from `exit_code_for(exit_class)`: `0` ok · `65` data · `69`
  unprovisioned · `70` permanent · `75` transient · `78` config. Use this for **colour/alert severity only**;
  `is_success()` separates success vs failure.

Other state to render: overall `current`/`stale`/`broken`; run `kind` `incremental` vs `rebaseline` (surface a
rebaseline loudly — it's a costly re-anchor); per-source baseline decision `Current`/`NoBaselineFetched`/
`RebaselinePending`.

`RunRecord` (`crates/jurisearch-producer/src/runrecord.rs`) fields actually present: `run_id`, `group`,
`sources`, `kind`, `outcome` (`running`/`success`/`failure`), `exit_class`, `error`, `started_at`,
optional `ended_at`, `fetch_cursors`, `ingest_journals`, `package_high_water_mark`, `published_package`,
`adopted_baselines`. **No stored duration** — derive it from `started_at`/`ended_at` (handle `running` with
`ended_at = null`).

`status` (`crates/jurisearch-producer/src/status.rs`) is **on-disk only (no DB, no network)** and exposes
`overall`, `published_head_sequence`, `published_manifest_generated_at`, `update_lock_held`, and per-group
sources/baselines/fetch-cursors/`rebaseline_pending`/`stale_by_age` + last-run summary — the dashboard's
primary feed.

---

## 6. PostgreSQL (optional Phase 2 only) — the read-grant reality

If/when we add deep corpus panels (doc/chunk/zone counts, embedding coverage %, enrichment outcomes), note:
**`jurisearch_read` CANNOT read the producer working tables.** Provisioning revokes all then grants the read
role only `public.index_manifest`, `public.schema_migrations`, `jurisearch_control.corpus_state`/
`generation_registry`, and `jurisearch_server.*` (`crates/jurisearch-storage/src/backend.rs:555-577`). It has
**no SELECT** on `package_catalog`, `package_change_log`, `ingest_run`/`ingest_member`,
`official_api_responses`, `documents`/`chunks`/`zone_units`, or the embedding tables.

So Phase 2 must FIRST deliver a **dashboard read surface**: a `dashboard.*` schema of owner-/SECURITY-DEFINER
views over those tables with `GRANT SELECT TO <dashboard_role>` (a dedicated least-privilege role, provisioned
by the dashboard install). Corrected facts for those panels:
- `package_catalog.status` ∈ `built` / `published` / `failed`; useful columns: `corpus`, `package_sequence`,
  `package_id`, `package_kind`, `baseline_id`, `generation`, `included_change_seq_low/high`, `package_digest`,
  `manifest_digest`, `embedding_fingerprint`, `schema_version`, `published_at` (chain-integrity audit).
- `ingest_run.status` ∈ `running`/`completed`/`failed`/`aborted`; `ingest_member.status` ∈
  `discovered`/`parsed`/`inserted`/`skipped`/`failed` (`crates/jurisearch-storage/src/ingest_accounting/`).
- **Embedding coverage** must require BOTH fingerprints (chunk-side AND embedding-side), per
  `ingest_accounting/readiness.rs:270-307`:
  ```sql
  SELECT count(*) AS chunks,
         count(*) FILTER (WHERE ce.chunk_id IS NOT NULL
                            AND c.embedding_fingerprint = 'bge-m3:1024:normalize:true') AS covered
  FROM public.chunks c
  LEFT JOIN public.chunk_embeddings ce
         ON ce.chunk_id = c.chunk_id AND ce.embedding_fingerprint = 'bge-m3:1024:normalize:true';
  -- analogous for public.zone_units LEFT JOIN public.zone_unit_embeddings
  ```
- `official_api_responses` (provider × outcome) for enrichment health.

Since Phase 1 reads packages from `corpora_dir`/`manifest.json` instead, **none of this blocks the core
dashboard** — it's a deliberately deferred enhancement.

---

## 7. Email alerts & reports (eventual — Phase 3)

The producer already has the hook: `producer.toml [alert]` has `hook_command` + `on_classes`, fired on the
configured exit classes after a run. Wire `hook_command` to a small **notify script** (e.g.
`/usr/local/bin/jurisearch-notify`) that emails on failure — no new producer code, just config + a script +
an SMTP relay. Separately, a **systemd timer** can run `jurisearch-dashboard report --email` for a periodic
digest (last runs, freshness, new packages, any errors). Needs SMTP creds via `producer.env`-style env
(`[dashboard.email]`), delivered by `deploy.sh` like the OPENROUTER/PISTE creds. Defer until the core
dashboard is in use.

---

## 8. Phased plan

- **Phase 1 — the core dashboard (the ask).** `jurisearch-dashboard` binary + systemd service; Overview /
  Packages / Runs+Errors / Logs pages; data from `status` + `RunRecord`s + `corpora_dir` + journald; bound to
  the tailnet, no auth; built by `dist.sh`, installed by `deploy.sh`. **No PG, no external stack.**
- **Phase 2 — optional deep corpus panels.** Add the `dashboard.*` read views + role; counts, embedding
  coverage, ingest-member breakdown, enrichment health.
- **Phase 3 — email.** On-failure alert via `[alert] hook_command` + a periodic digest timer.

---

## 9. Constraints & gotchas (carry over from the deployment)

- **Tailnet bind, no auth** — bind ONLY to the tailscale addr/interface (never `0.0.0.0`); the tailnet ACL is
  the boundary. Confirm the bind in `deploy.sh`'s verification.
- **Read-only** — the service must never write PG, `state_dir`, or `corpora_dir`.
- **Runs as `jurisearch` + `systemd-journal` group** (to read run records, the served root, and journald) —
  no root needed.
- **`corpora_dir` is the CIFS storagebox** (`uid=999` mount) — "packages" sizing reads that; CT 111 local
  disk is only ~29 GB, so don't conflate the two in any disk panel.
- **PG (Phase 2 only)**: CT 110 SSL is broken → `sslmode=disable`; CT 110↔CT 111 path MTU ~1400; and the read
  grant (§6) must be solved before any PG panel.
- **`status` is cheap (on-disk); PG `count(*)` is not** — if Phase 2 lands, interval-cache the heavy queries.

---

## 10. Resolved Phase-1 decisions (operator, 2026-06-30)

1. **Bind + port are CONFIGURABLE** (config knobs `bind` + `port`; default to the tailnet addr). Never
   `0.0.0.0`.
2. **Logs = a small ring buffer / time window** (last-N / recent window), **no redaction needed**.
3. **Packages from `corpora_dir`/`manifest.json` — NO database** in Phase 1.
4. **No PG corpus stats** — Phase 2 (PG panels) is **out of scope** for now.
5. **Title: "Juridia — Update Server".**

---

## 11. Suggested next step

Confirm §10.1–10.3, then I scope **Phase 1** as a small reviewed deliverable: the `jurisearch-dashboard`
crate (Overview/Packages/Runs/Logs from on-box sources) + its `dist.sh`/`deploy.sh` integration + the
tailnet-bound systemd unit — Codex-gated like the rest, then deployed to CT 111.
