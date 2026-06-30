# Juridia — Update-Server Dashboard — Design

> Status: DESIGN (no implementation plan yet). Companion to
> `00-update-server-dashboard-analysis.md` (scope/decisions) and its Codex reviews.
> **Stack (operator-chosen):** Bun · Vue 3 · shadcn-vue · TypeScript · Biome · **self-executable single binary**.
> **Principles:** DRY + SOLID. **Phase 1 only** — producer-only, on-box sources, NO database, NO auth,
> Tailscale-only, hosted on CT 111, built into the `dist.sh` bundle + installed by `deploy.sh`.

---

## 1. Purpose & boundaries

A small, read-only operational dashboard answering, at a glance: *is each corpus current and publishing
correctly, what packages exist, what failed, and what do the logs say?* It is a **pure observer**: it never
writes the DB, `state_dir`, or `corpora_dir`, and never triggers a producer action.

In scope (Phase 1): Ingestion status · Packages produced · Errors · Logs. Out of scope here (design seams
kept, see §11): PostgreSQL corpus stats (Phase 2), email alerts/reports (Phase 3).

The dashboard treats the **producer's JSON outputs as its only contract** — it does NOT link Rust code. Its
inputs are: `jurisearch-producer status` (stdout JSON), the `RunRecord` files under `state_dir`, the served
`core/manifest.json`, `journalctl -o json`, and `systemctl`. This keeps a hard language/process boundary and
lets the dashboard ship as an independent self-contained binary.

---

## 2. Stack & rationale

| Concern | Choice | Why |
|---|---|---|
| Runtime + bundler + package mgr + test | **Bun** | one tool; fast; **`bun build --compile`** → a single self-contained executable (no Node/Bun on CT 111) |
| Backend HTTP | **`Bun.serve`** (TypeScript) | built-in, zero-dep server; serves the API + the embedded SPA |
| UI framework | **Vue 3** (`<script setup>`, Composition API) | small, typed, composable; pairs with shadcn-vue |
| Components | **shadcn-vue** (Reka UI + Tailwind) | copy-in, owned components (no heavy dep); accessible primitives |
| Language | **TypeScript** (strict) | one type system across backend + frontend (DRY contracts) |
| Lint + format | **Biome** | single fast tool (replaces ESLint + Prettier); `biome.json` |
| Packaging | **`bun build --compile --target=bun-linux-x64`** | one binary embedding server code **and** the built SPA assets |
| DB access (Phase 2 only) | **`Bun.sql`** (built-in native Postgres, Bun ≥ 1.2) | no `pg`/`postgres` dep; compiles into the binary; stays self-contained |

**Self-executable model (a build REQUIREMENT, not a settled fact):** the Vue SPA is built to static assets at
build time; those assets must be **embedded into the compiled Bun binary**, which at runtime serves them + the
JSON API from a single process. Build host needs Bun; **CT 111 needs only the produced binary** — same
deployment shape as the five Rust binaries. ⚠ Bun's `--compile` asset-inclusion mechanics are version-specific,
so this is gated by a **proof task** (§8): pin a Bun version, compile a minimal Vue build with hashed assets,
run the binary on a CT-111-like host (x86_64/glibc) with no filesystem `dist/`, and assert deep-linked SPA
routes + static-asset responses work. If embedding proves unreliable on the pinned Bun, the fallback is a
`dist/` dir shipped beside the binary — but the design's intent is a single self-contained executable.

---

## 3. High-level architecture

```
                          ┌────────────────────────── jurisearch-dashboard (one Bun binary) ──────────────────────────┐
  CT 111 on-box sources   │                                                                                          │
  ─────────────────────   │   Adapters (I/O)        Providers (domain)      Services         HTTP (Bun.serve)        │
  jurisearch-producer ──▶ │  ProcessAdapter ─┐                                                                       │
    status (JSON)         │                  ├─▶ StatusProvider ──┐                                                  │
  state_dir/runs/*.json ─▶│  FileAdapter ────┤   RunsProvider ────┤                                                 │
  corpora_dir/manifest ──▶│                  ├─▶ PackagesProvider ─┼─▶ OverviewService ─┬─▶ /api/* (typed JSON) ◀──┐ │
  journalctl -o json ────▶│  ProcessAdapter ─┤   LogsProvider ─────┤   (+ per-resource)  │                         │ │
  systemctl ─────────────▶│                  └─▶ TimersProvider ───┘                     └─▶ embedded SPA (static) │ │
                          │                                                                                       │ │
                          └───────────────────────────────────────────────────────────────────────────────────┼─┘
                                                                                                                │
  Browser (tailnet only) ◀────────────────────── Vue 3 SPA (shadcn-vue) ── polls /api/* ───────────────────────┘
```

Three backend layers + a frontend, each with one job (SRP):
- **Adapters** — the only code that touches the outside world: `ProcessAdapter` (spawn a command, capture
  stdout/exit) and `FileAdapter` (read/stat a path). Everything else is pure + unit-testable.
- **Providers** — one per data source; turn raw adapter output into validated domain DTOs.
- **Services** — compose providers into page-shaped responses (e.g. `OverviewService` joins status + last run
  + timer); add caching.
- **HTTP** — `Bun.serve` router: `/api/*` JSON + the embedded SPA (with SPA-fallback).
- **Frontend** — Vue SPA consuming `/api/*` via shared types.

---

## 4. Data contracts (producer JSON → shared TypeScript DTOs)

A single **`shared/`** module of TS types + Zod-style runtime validators is the source of truth, imported by
both backend (to validate provider output) and frontend (to type API responses) — **DRY across the wire**.
The shapes mirror the REAL producer outputs (verified this deployment + Codex review):

- **`StatusDTO`** ← `jurisearch-producer status` (on-disk, no DB/network). It is a **narrowed mirror** — it
  carries the fields the dashboard renders via an **explicit allow-list** (not a silent subset). Include:
  `generatedAt`, `overall` (`current`|`stale`|`broken`), `corpus`, `publishedHeadSequence`, `activeBaselineId`,
  `publishedManifestGeneratedAt`, `updateLockHeld`; per `groups[]`: `group`, `sources[]`, `baselines[]`
  (`source`, `state` = `current`|`no_baseline_fetched`|`rebaseline_pending`, `adoptedBaseline`,
  `fetchedBaseline`), `fetchCursors[]`, `rebaselinePending`, `staleByAge`, and the last-run summary
  (`lastRunId`, `lastOutcome`, `lastExitClass`, `lastError`, `lastEndedAt`). **`lastError` + `fetchCursors`
  directly feed the Errors / Freshness panels** — don't drop them. (Verify exact field names against
  `status.rs:73-105`,`:180-212`.)
- **`RunRecordDTO`** ← `state_dir/runs/<group>/*.record.json` (+ `last.json`): `runId`, `group`, `sources[]`,
  `kind` (`incremental`|`rebaseline`|`dry_run`), `outcome` (`running`|`success`|`failure`), `exitClass` (the
  EXACT string — note an in-flight record persists `outcome=running` AND `exitClass="running"`), `error?`,
  `startedAt`, `endedAt?`, `publishedPackage?`, `packageHighWaterMark?`, `adoptedBaselines[]`. **No stored
  duration** → derived (`endedAt − startedAt`; null while `running`).
- **`PackageDTO`** ← served `core/manifest.json`, which is a **`Signed<RemoteManifest>`** (signed wrapper; the
  manifest is under `payload` — `status.rs:245-264` parses it this way). The `PackagesProvider` parses the
  wrapper, validates `payload`, optionally verifies the top-level signature, then maps `payload.active_baseline`
  + `payload.packages` (`crates/jurisearch-package/src/manifest/remote.rs`). DTO fields use the **exact manifest
  names**: `manifestGeneratedAt`, `headSequence`; `activeBaseline` = `baselineId`, `generation`, `packageKind`,
  `sequence`, `schemaVersion`, `sha256`, `compressedSizeBytes`, `uncompressedSizeBytes`; `packages[]` =
  `packageId`, `fromSequence`, `toSequence`, `compressedSizeBytes`, `uncompressedSizeBytes`, `rowCounts`,
  `schemaVersion`, `embeddingFingerprint`, `sha256`, `signature`. **Manifest-available fields ONLY** — NOT
  catalog `status`/`publishedAt`/change-window (Phase-2 DB); `digest` is a catalog/embedded-manifest concept
  **not** on `RemotePackageEntry`, so don't use it here.
- **`ExitClass`** is the exact persisted string; the shared module carries the **COMPLETE vocabulary as a typed
  table** (class → source → success/failure → severity bucket), not an ellipsis:
  - **Success** (`SUCCESS_CLASSES`, `exit.rs`): `published`, `no-op`, `rebaselined`, `published-enrich-degraded`,
    `dry-run`.
  - **In-flight**: `running` (started, not-yet-finished record — `runrecord.rs`).
  - **Failure** (`ProducerError::class`, `error.rs:75-100`): `fetch-failed`, `ingest-failed`, `enrich-degraded`,
    `embed-failed`, `provision-failed`, `storage-failed`, `integrity-failed`, `needs-rebaseline`,
    `producer-db-unprovisioned`, `config-invalid`, `publish-failed`, `skipped-lock-held`, `alert-hook-failed`,
    `io-failed` (+ `upstream-unreachable`, which `exit_code_for` maps transient even if not emitted today).
  - **Severity is derived from `outcome` FIRST** (not the class string alone): `running` → neutral/in-progress;
    success classes → ok; failure classes → the `exit_code_for` bucket (`ok`/`data`/`unprovisioned`/`permanent`/
    `transient`/`config`). `isSuccess()` + `severityOf()` are backend-owned (one map, derived from
    `SUCCESS_CLASSES`/`exit_code_for`/`ProducerError::class`), so the UI stays dumb. The table is verified
    against `exit.rs`/`error.rs` at build time so it can't silently drift.
- **`LogLineDTO`** ← `journalctl -o json`: `timestamp` (from `__REALTIME_TIMESTAMP`), `priority`, `unit`,
  `message`. No redaction.
- **`TimerDTO`** ← `systemctl list-timers`: `group`, `timerUnit` (`jurisearch-producer-<group>.timer`),
  `serviceUnit` (`jurisearch-producer-<group>.service`), `nextRun`, `lastRun`, `active` — carry the group/unit
  mapping so the UI never re-infers names (`render.rs`).

Unknown/extra producer fields are ignored (forward-compatible); a validator failure degrades that one panel
(see §7), never crashes the server.

---

## 5. Backend design (Bun) — SOLID

### 5.1 Adapters (Dependency Inversion boundary)
```ts
interface ProcessRunner { run(cmd: string[], opts?: {...}): Promise<{ stdout: string; code: number }> }
interface FileSource   { read(path: string): Promise<string>; stat(path: string): Promise<StatInfo>;
                         list(dir: string): Promise<string[]> }
```
Only `ProcessAdapter` (wraps `Bun.spawn`) and `FileAdapter` (wraps `Bun.file`/`node:fs`) implement these.
Providers depend on the **interfaces**, not on Bun — so providers are unit-tested with in-memory fakes (no
subprocess, no filesystem). This is the DIP seam and the testability win.

### 5.2 Providers (Single Responsibility, one per source)
```ts
interface DataProvider<T> { get(): Promise<T> }           // open/closed: add a source w/o touching others
class StatusProvider   implements DataProvider<StatusDTO>      { constructor(proc, cfg) … }   // runs `… status`
class RunsProvider     implements DataProvider<RunRecordDTO[]> { constructor(files, cfg) … }  // reads state_dir
class PackagesProvider implements DataProvider<PackageDTO>     { constructor(files, cfg) … }  // reads Signed<RemoteManifest>, maps payload
class LogsProvider     implements DataProvider<LogLineDTO[]>   { constructor(proc, cfg) … }   // journalctl
class TimersProvider   implements DataProvider<TimerDTO[]>     { constructor(proc, cfg) … }   // systemctl
```
Each provider: invoke adapter → parse → **validate against the `shared/` schema** → return DTO. Parsing is
pure and separately unit-tested. Adding the future PG provider (Phase 2) is a new `DataProvider` — **no edits
to existing providers/parsers** (Open/Closed); it still needs composition-root wiring + a route + an API-client
endpoint + a panel (additive, not modifying existing code).

### 5.3 Services (composition + caching)
`OverviewService` joins `StatusProvider` + `RunsProvider` (last run per group) + `TimersProvider` into the
Overview payload; thin pass-through services wrap the others. A small **`Cached<T>`** decorator (one generic,
DRY) wraps any provider with a TTL (status/overview ~2–5 s; logs ~2 s; packages ~15 s) so a fast UI poll never
storms the box. Caching is a cross-cutting wrapper, not duplicated per provider.

### 5.4 HTTP (`Bun.serve`)
One router; endpoints return the `shared/` DTOs verbatim:
`GET /api/overview` · `/api/runs?group=&limit=` · `/api/packages` · `/api/logs?group=&limit=&since=` ·
`/api/health`. Everything else serves the **embedded SPA** (with `index.html` fallback for client routes).
- **Fail-closed bind:** read `bind`/`port` from config; refuse to start if `bind` resolves to `0.0.0.0`/`::` —
  tailnet address (or `tailscale0`) only. No auth by design (tailnet ACL is the boundary).
- **Read-only:** no route mutates anything; adapters expose no write methods.
- **Resilience:** a provider error → that endpoint returns a typed `{ ok:false, error }` (degraded panel), the
  process stays up; one bad source never blanks the whole dashboard.

### 5.5 Config
`[dashboard]` resolved from flags > env > a small toml (mirroring the producer's config ergonomics):
`bind`, `port`, `producerBin` (path to `jurisearch-producer`), `producerConfig` (`/etc/jurisearch/producer.toml`),
`stateDir`, `corporaDir`, `groups[]`, cache TTLs, log window defaults. `--version` prints the stamped build id
(parity with the Rust binaries).

---

## 6. Frontend design (Vue 3 + shadcn-vue) — DRY

### 6.1 Composition layers
- **`api/` client** — one typed `fetchJson<T>(path)` (validates with the shared schema) + an endpoint map.
  Single place that knows URLs/shapes.
- **`composables/`** — `usePolling(fetcher, intervalMs)` is the ONE polling/refresh primitive (visibility-aware,
  pause on hidden tab, manual refresh). Per-resource composables (`useOverview`, `useRuns`, `usePackages`,
  `useLogs`) are thin wrappers over it → no duplicated fetch/loading/error logic. (TanStack Query Vue is an
  optional drop-in if we want cache/dedupe out of the box; the composable abstracts that choice away.)
- **`components/`** — presentational, prop-driven, no fetching: `StatusBadge`, `GroupCard`, `RunRow`,
  `PackagesTable`, `LogViewer`, `FreshnessMeter`, `KeyValue`. Built from shadcn-vue (`Card`, `Table`, `Badge`,
  `Tabs`, `ScrollArea`, `Alert`, `Tooltip`). Reused across pages (DRY).
- **`pages/`** (Vue Router): **Overview**, **Packages**, **Runs/Errors**, **Logs** — compose composables +
  components only.
- **`lib/format.ts`** — shared formatters (relative time, duration from `started/endedAt`, bytes, sequence
  ranges) used by both pages and components; the ONE place these live.

### 6.2 Pages
- **Overview** — a `GroupCard` per fetch group: R/A/G state, last run (`outcome`/`exitClass`/`kind`/derived
  duration/when), `FreshnessMeter` (adopted vs DILA-current; pending-delta lag), `publishedHeadSequence`, lock
  held, next timer. A `rebaseline` kind is flagged prominently.
- **Packages** — `PackagesTable` from the manifest: active baseline highlighted, then the increment chain
  (id, sequence from/to, size, row counts, schema/fingerprint, digest, manifest `generatedAt`).
- **Runs / Errors** — per-group `RunRow` list (kind, outcome, exact `exitClass`, derived duration, error);
  failures pinned; severity colour from `severityOf`.
- **Logs** — `LogViewer` (shadcn `ScrollArea`): a small ring-buffer / `since`-window per producer service,
  group filter, priority colour, auto-refresh. No redaction.

### 6.3 Design system & branding
shadcn-vue + Tailwind tokens; light/dark; title/brand **"Juridia — Update Server"**. R/A/G semantics derived
from `severityOf` + freshness, never hard-coded per component.

---

## 7. Project layout (one Bun workspace, shared types)

```
crates/… (unchanged Rust)            apps/dashboard/                 # new, Bun workspace
                                       package.json  biome.json  tsconfig.json  bunfig.toml
                                       shared/      # DTOs + validators (imported by server AND web) — DRY
                                       server/      # adapters / providers / services / http / config / main
                                       web/         # Vue 3 SPA: api · composables · components · pages · lib
                                       build/       # bun build (web → static) + bun --compile (→ binary w/ embedded assets)
```
`shared/` is the single contract both sides import — no drift between API responses and UI types.

---

## 8. Self-executable packaging & deploy shape (design)

- **Build:** `web` → static via `bun build`; those assets are **embedded** into the `server` bundle; then
  `bun build server/main.ts --compile --target=bun-linux-x64 --outfile jurisearch-dashboard` → one binary that
  serves API + SPA. Stamp a `--version` (build id) like the Rust binaries.
- **Distribution:** `dist.sh` adds `jurisearch-dashboard` to the update-server bundle (`UPDATE_SERVER_BINS`),
  audited into `SHA256SUMS`; the bundle is now **two binaries**. ⚠ Constraint: `dist.sh`'s build path is
  **Cargo-only** today (`BUILD_BINS`, the per-binary `--version`/SHA audit, the bundle manifest) — adding a
  **non-Cargo Bun build step** must INTEGRATE with that existing checksum/version-audit model (a Bun
  build→`--version` stamp→SHA entry→manifest line), not bolt on a raw copy. `deploy.sh` likewise hardcodes a
  single staged/installed/verified binary; it must generalise to stage+verify+install BOTH.
- **Install (`deploy.sh`):** install `/usr/local/bin/jurisearch-dashboard`; write
  `jurisearch-dashboard.service` (`User=jurisearch`, **`SupplementaryGroups=systemd-journal`**, `Restart=always`,
  bound to the configured tailnet addr) + the `[dashboard]` config; `enable --now`; **fail-closed verify**:
  service `active`, listening **only** on the tailnet addr, and `journalctl -u
  jurisearch-producer-legislation.service -n 1` works under the dashboard identity.
- CT 111 needs only the binary; the build host needs Bun. (Build host is `x86_64` linux/glibc, matching
  CT 111 — native target, no cross-compile.)
- **Asset-embedding proof task (gates the self-executable claim, §2):** before committing to embedding, pin a
  Bun version and prove a `--compile` binary serves a real hashed-asset Vue build + deep-linked SPA routes on a
  CT-111-like host with no filesystem `dist/`. Fallback if it's unreliable on the pinned Bun: ship a `dist/`
  dir beside the binary (still one deploy unit).

> Detailed `dist.sh`/`deploy.sh` edits and the build pipeline are the **implementation plan** — deferred per
> "design only".

---

## 9. How the design honours DRY & SOLID

- **SRP** — adapters do I/O; providers parse one source; services compose; components render; composables fetch.
  Each module has one reason to change.
- **OCP** — new data source = a new `DataProvider` + endpoint + panel; existing code untouched. The Phase-2 PG
  provider and Phase-3 email reporter slot in here.
- **LSP** — every provider satisfies `DataProvider<T>`; the cache decorator and services treat them uniformly.
- **ISP** — narrow `ProcessRunner` / `FileSource` interfaces; consumers depend only on what they use.
- **DIP** — providers/services depend on adapter + provider **interfaces**, not on Bun/fs/subprocess; the
  concrete adapters are injected at composition root (`main.ts`). Enables fakes → fast unit tests.
- **DRY** — the `shared/` DTO+validator module (one contract for wire + UI); `usePolling` (one fetch/refresh
  primitive); `Cached<T>` (one caching wrapper); `lib/format.ts` (one formatting home); `severityOf`/`isSuccess`
  (one exit-class mapping, backend-owned).

---

## 10. Cross-cutting concerns

- **Security:** tailnet-only bind (fail closed; never `0.0.0.0`), no auth (tailnet ACL), strictly read-only,
  runs as unprivileged `jurisearch` (+`systemd-journal`). Logs unredacted (no secrets in producer logs; archive
  names only).
- **Resilience:** per-source error isolation; the dashboard reflects "source unavailable" instead of failing.
- **Performance:** `status` is cheap (on-disk); `Cached<T>` TTLs keep subprocess/file reads bounded under fast
  UI polling; log queries bounded by `-n`/`--since`.
- **Observability of the dashboard itself:** `/api/health`; its own logs to journald.
- **Versioning:** `--version` build-id stamp; bundle audit parity with the Rust binaries.

---

## 11. Deferred — with design seams already in place

- **Phase 2 (PG corpus stats):** add a `CorpusProvider` (`DataProvider<CorpusStatsDTO>`) reading CT 110 via
  **`Bun.sql`** — Bun's built-in **native PostgreSQL driver** (Bun ≥ 1.2), so **no `pg`/`postgres` npm
  dependency** and it compiles straight into the self-contained binary. Connect with **`sslmode=disable`**
  (CT 110's SSL is broken) as a least-privilege role over a `dashboard.*` view layer (`jurisearch_read` cannot
  see the working tables — see analysis §6). New provider + endpoint + panel only (OCP) — no rework, no new dep.
- **Phase 3 (email alerts/reports):** an `EmailReporter` triggered by the producer's existing `[alert]
  hook_command` (on-failure) + a periodic digest; SMTP creds via `producer.env`-style delivery. Reuses the
  same providers (DIP) to compose the digest.

---

## 12. Open design questions

1. **Data fetching lib** — custom `usePolling` only, or TanStack Query Vue underneath it (cache/dedupe/retry
   for free)? The composable hides the choice; pick before the SPA is built.
2. **Asset embedding mechanism** in `bun --compile` — embed the built SPA via imported file assets vs a small
   static map; confirm the chosen Bun version's embedding API.
3. **Config format** — a `[dashboard]` block in `producer.toml` vs a standalone `dashboard.toml`; align with
   how `deploy.sh` already templates config.
4. **Status acquisition** — shell out to `jurisearch-producer status` (clean contract, one subprocess) vs read
   the `state_dir` JSON directly (no subprocess, but re-implements `build_status`'s on-disk composition). The
   subprocess keeps the producer as the single source of truth — recommended.
5. **Bun version pinning** for reproducible `--compile` builds (record in `bunfig.toml`/CI).
