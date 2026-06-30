# Juridia — Update-Server Dashboard — Implementation Plan

> Translates the DESIGN (`01-update-server-dashboard-design.md`) into ordered, reviewable work. **Phase 1 only**
> (producer-only, on-box sources, no DB, no auth, Tailscale-only, on CT 111, shipped via `dist.sh`/`deploy.sh`).
> Stack: Bun · Vue 3 · shadcn-vue · TypeScript · Biome · self-executable single binary.

---

## 0. Ground rules

- **Orchestrator discipline** (repo CLAUDE.md): each milestone's implementation is delegated to a subagent with
  a precise brief (the design doc is the spec); the agent must leave **build + lint + typecheck + tests green**
  before returning; then a **Codex review gate → `VERDICT: GO`**; only then the orchestrator commits (precise
  pathspec, paired with the review doc). `FIXES_REQUIRED` → delegate fixes → re-review until GO.
- **The producer is the contract.** The dashboard consumes JSON only (`jurisearch-producer status`, `RunRecord`
  files, the served `Signed<RemoteManifest>`, `journalctl -o json`, `systemctl`). No Rust linkage.
- **Strictly read-only**; **fail-closed tailnet bind** (never `0.0.0.0`); runs as unprivileged `jurisearch`
  (+`systemd-journal`).
- **DRY/SOLID** as specified in design §9 — enforced in reviews, not just asserted.

---

## 1. Spikes FIRST (de-risk before bulk build)

Three spikes retire the top unknowns BEFORE bulk build — packaging, the data contract, AND on-host execution
under the real dashboard identity. Do them before M1+.

### Spike A — Bun `--compile` asset embedding (a DECISION GATE for packaging, design §2/§8)
Pin a Bun version. Build a trivial hashed-asset Vue app; embed it into a `bun build … --compile
--target=bun-linux-x64` binary; run that binary on a **CT-111-like host (x86_64/glibc)** — ideally CT 111
itself in a scratch dir — with **no filesystem `dist/`**, and assert: `/` serves, a **deep-linked SPA route**
serves `index.html`, hashed JS/CSS assets return 200 with correct content-types.
- **This is a gate, not just a note:**
  - **PASS (embedding works)** → record the mechanism + pinned Bun version in the design + `bunfig.toml`;
    M5–M7 proceed as written (single self-contained binary).
  - **FAIL** → **M5/M6a/M6b and the DoD MUST be revised before implementation** to the adjacent-asset model:
    define the `dist/`-beside-binary bundle layout, the service `WorkingDirectory`, and — critically — the
    `dist.sh` packaging/audit changes, because today `dist.sh`'s tarball helper includes only
    `bin`/`config`/`systemd`/`completions`/`SHA256SUMS` (`dist.sh:329-335`) and its bundle audit FORBIDS common
    runtime asset names like `manifest.json` (`dist.sh:84-101`). So the fallback needs explicit allowed-audit
    paths, checksums, staging/install, and deploy verification — it is NOT free. Don't leave it as prose.

### Spike B — capture REAL producer JSON as fixtures (drives the contracts)
On the **live CT 111**, capture real outputs to commit as test fixtures:
`jurisearch-producer status --config …`; finished `state_dir/runs/<group>/*.record.json`; the served
`core/manifest.json` (`Signed<RemoteManifest>`); `journalctl -u jurisearch-producer-legislation.service -o json
-n 50`; `systemctl list-timers 'jurisearch-producer-*' -o json`. Cross-check the exit-class set against
`exit.rs`/`error.rs`.
- **A `running` RunRecord fixture is REQUIRED, not "if catchable"** — the producer persists in-flight records
  with `outcome=Running`, `ended_at=None`, `exit_class="running"` (`runrecord.rs:70-82`), and mishandling it
  (severity from class alone → bucketed `permanent`) is exactly the trap. Use a real captured running record,
  or a **source-derived synthetic** validated against `RunRecord::started`'s shape. M1 contract tests + M4 UI
  tests MUST assert `running` → neutral/in-progress + null duration (non-optional).
- **Output:** `apps/dashboard/fixtures/*.json` — the ground truth for `shared/` DTOs, validators, and parser
  tests (contracts verified against reality, not prose).

### Spike C — on-host execution under the dashboard identity (retire the runtime-perm/bind risks)
On **CT 111**, run the exact runtime operations the dashboard will do, **as the dashboard identity**
(`User=jurisearch` + `SupplementaryGroups=systemd-journal`), so M2–M5 can't go green over fixtures while the
box fails: `journalctl -u jurisearch-producer-legislation.service -o json -n 1` and `systemctl list-timers
'jurisearch-producer-*' -o json` under that identity (producer units run `User/Group=jurisearch` with no
journal access by default — `render.rs:55-56`; `deploy.sh` doesn't add the group today — `deploy.sh:294-300`);
and a **minimal `Bun.serve` bound to the configured tailnet address** on CT 111 (proves the bind + the
fail-closed `0.0.0.0` guard on the real interface).
- **Output:** recorded command/exit proof artifacts; if journal access needs `SupplementaryGroups` or a
  `deploy.sh` group-add, that requirement is locked into M6b now, not discovered at deploy time. (The M6b
  deploy-time checks stay, but are no longer the FIRST proof.)

---

## 2. Milestones (each = delegated build → green → Codex GO → commit)

### M0 — Scaffold + tooling
`apps/dashboard/` Bun workspace: `package.json` (workspaces `shared`/`server`/`web`), strict `tsconfig.json`,
`biome.json`, `bunfig.toml` (pinned Bun). Scripts: `lint` (Biome), `typecheck` (tsc), `test` (bun test),
`build` (web), `compile` (`--compile`). `--version` build-id plumbing that **exactly matches the existing
release format**.
**DoD:** Biome + tsc + a smoke test green; a trivial compiled binary runs and `jurisearch-dashboard --version`
prints **exactly** `jurisearch-dashboard <workspace-version> (<12-char-commit>, <target>)` — the same shape
`dist.sh` exact-matches (`dist.sh:274-293`) and `deploy.sh` compares (`deploy.sh:165-170`,`:502-507`), stamped
from the same workspace version / `JURISEARCH_BUILD_COMMIT` / target, so it can't pass M0/M5 locally yet fail
the M6a release audit.

### M1 — `shared/` contracts (the DRY core) — depends on Spike B
TS DTOs + runtime validators (Zod-style) for `StatusDTO`, `RunRecordDTO`, `PackageDTO` (the `Signed<…>` wrapper
→ `payload`), the `ExitClass` table + `severityOf`/`isSuccess` (severity derived from `outcome` first),
`LogLineDTO`, `TimerDTO` (`group`/`timerUnit`/`serviceUnit`). A **build-time check** asserts the exit-class
table covers the producer's actual classes (from the fixtures / `exit.rs`/`error.rs`) so it can't drift.
**DoD:** validators parse every Spike-B fixture; unit tests green; exit-class drift check green.

### M2 — Backend adapters + providers (SOLID, pure-testable)
`ProcessRunner`/`FileSource` interfaces; `ProcessAdapter` (`Bun.spawn`) + `FileAdapter` the only I/O. Providers
(`Status`/`Runs`/`Packages`/`Logs`/`Timers`) = invoke adapter → parse (pure) → validate → DTO. **Unit-tested
with in-memory fakes** (no subprocess/fs) over the fixtures.
**DoD:** provider + pure-parser tests green using fakes; zero real I/O in tests.

### M3 — Services + HTTP (`Bun.serve`)
`Cached<T>` TTL decorator (one generic); `OverviewService` (joins status+runs+timers) + thin services. Router:
`GET /api/overview|runs|packages|logs|health` + SPA fallback. **Fail-closed bind** guard (refuse `0.0.0.0`/`::`);
config resolution (flags > env > toml); per-source error isolation (typed `{ok:false,error}` degraded panel).
**DoD:** integration test drives the server with fake adapters, asserts the DTOs; bind-guard + degraded-path
tests green.

### M4 — Frontend SPA (Vue 3 + shadcn-vue) — can parallel M2/M3 once M1 lands
`api/` typed client (validates with `shared/`); the single `usePolling` primitive + per-resource composables;
presentational components (`StatusBadge`/`GroupCard`/`RunRow`/`PackagesTable`/`LogViewer`/`FreshnessMeter`/…)
from shadcn-vue; `lib/format.ts` (relative time, derived duration, bytes, seq ranges); 4 pages + Vue Router;
light/dark + **"Juridia — Update Server"** branding; R/A/G from `severityOf`+freshness (never hard-coded).
**DoD:** SPA builds; renders against the real backend (M3) over fixtures; key composable/format unit tests green.

### M5 — Packaging (self-executable) — GATED by Spike A's decision
`build/`: `bun build` web → static → embed → `bun build server/main.ts --compile --target=bun-linux-x64
--outfile jurisearch-dashboard` (with the exact-format `--version`). **If Spike A FAILED, M5 follows the revised
adjacent-asset layout instead** of embedding. Verify the compiled binary serves API + SPA standalone.
**DoD:** one self-contained binary (or the adjacent-asset deploy unit per Spike A); `--version` matches the
exact release format (M0); smoke on a CT-111-like host (API + deep-linked SPA + assets, no `dist/` in the embed
case).

### M6a — `dist.sh` + bundle integration (SCRIPT — Codex-gated)
Add `jurisearch-dashboard` to the update-server bundle via the **non-Cargo Bun build path**, integrated with the
existing audit model (`BUILD_BINS` is Cargo-only today — `dist.sh:217-265`; `UPDATE_SERVER_BINS` has one binary
— `dist.sh:71-74`; the manifest lists one — `dist.sh:446-449`): Bun build → exact-format `--version` stamp →
`SHA256SUMS` entry → `manifest.toml` line, **not a raw copy**.
**DoD:** `dist.sh` produces a **two-binary** update-server bundle; **both** binaries appear in `SHA256SUMS` and
`manifest.toml` with exact `--version` strings that pass `dist.sh`'s exact-match audit; `bash -n` + `shellcheck`
clean; **Codex `GO`**.

### M6b — `deploy.sh` integration (SCRIPT run against the live host — Codex-gated before ANY run)
Generalise the single-binary stage/verify/swap/verify (`deploy.sh:150-170`,`:261-267`,`:321-331`,`:449-507`) to
**stage + verify (SHA + `--version`) + install BOTH** binaries; write `jurisearch-dashboard.service`
(`User=jurisearch`, `SupplementaryGroups=systemd-journal` — locked in by Spike C, `Restart=always`, bound to the
configured tailnet addr) + the `[dashboard]` config (templated like the producer config); `enable --now`;
**fail-closed verify**: service `active`, listening **only** on the tailnet addr (never `0.0.0.0`), and
`journalctl -u jurisearch-producer-legislation.service -n 1` works under the dashboard identity.
**DoD:** `bash -n` + `shellcheck` clean; `--dry-run` validates; **Codex `GO`** BEFORE any real run.

### M7 — Deploy to CT 111 + verify
Run `dist.sh` then `deploy.sh`; confirm the dashboard is reachable **only** over Tailscale, no auth, and the 4
pages show **live** data — Overview (group states/freshness/last run), Packages (the published `core-1-1` +
any increments from the running legislation update), Runs/Errors, Logs. Confirm read-only (no writes anywhere).
**DoD:** live, verified dashboard on CT 111; nothing else on the box disturbed.

---

## 3. Testing & quality gates (per milestone)
- **Lint/format:** Biome clean. **Types:** `tsc --strict` clean.
- **Unit:** pure parsers + providers with in-memory fakes (no I/O).
- **Contract:** validators parse every committed real fixture (Spike B); exit-class drift check.
- **Integration:** `Bun.serve` over fake adapters → assert `/api/*` DTOs; bind-guard; degraded-panel path.
- **E2E smoke:** the compiled binary serves API + deep-linked SPA + assets standalone.
- **Deploy verification:** `deploy.sh`'s fail-closed checks (active, tailnet-only bind, journald access).

## 4. Codex review gates (where, and why)
A gate per milestone artifact. Highest-value gates: **Spike A** (the packaging decision), **M1** (the contracts
— must match the producer), **M5** (packaging — the Bun self-executable), and **M6a + M6b** (the `dist.sh` /
`deploy.sh` scripts — M6b runs against the live host, so its Codex `GO` precedes any real run). Re-review
(`…-rN.md`) until `GO`. Apply all severities whose fix fits intent.

## 5. Sequencing
`Spike A` ∥ `Spike B` ∥ `Spike C` → `M0` → `M1` → { `M2` → `M3` } ∥ `M4` → `M5` (needs Spike A + M3 + M4) →
`M6a` → `M6b` → `M7`. M4 may start once M1 (`shared/`) exists, in parallel with M2/M3. **Spike A's decision may
revise M5/M6a/M6b/DoD before they start.**

## 6. Risks → mitigations
| Risk | Mitigation |
|---|---|
| Bun `--compile` asset embedding version-specific | **Spike A** (a decision gate) proves it on a CT-111-like host; pinned Bun; if it fails, M5/M6a/M6b/DoD revised to the documented `dist/`-beside-binary layout before build |
| Producer JSON shape drift | `shared/` validators + **real fixtures (Spike B)** + build-time exit-class drift check |
| `running` in-flight record mishandled | **required** running fixture (Spike B); M1/M4 tests assert neutral/in-progress + null duration |
| Non-Cargo Bun build in `dist.sh` | integrate with the existing checksum/`--version`/manifest audit, not a copy (**M6a**) |
| No-auth exposure | fail-closed tailnet bind (never `0.0.0.0`); proven on the real interface in **Spike C**; re-verified in **M6b** |
| Logs/Timers green-but-broken on the box | **Spike C** runs `journalctl`/`systemctl` under the dashboard identity BEFORE bulk build; `SupplementaryGroups=systemd-journal` locked into **M6b** + a deploy-time check |
| `--version` fails the release audit | M0/M5 DoD require the EXACT `dist.sh` format/stamp; M6a proves both binaries pass the audit |
| Accidental writes | adapters expose **no** write methods; read-only reviewed at M2 |
| Subprocess/poll storms the box | `Cached<T>` TTLs; bounded log queries (`-n`/`--since`) |

## 7. Definition of done (Phase 1)
The `jurisearch-dashboard` binary builds + compiles self-contained; `dist.sh` bundles it (two-binary, audited);
`deploy.sh` installs + verifies it on CT 111 (tailnet-only, no auth, journald access); the 4 pages show live
producer data; every milestone Codex-`GO`'d and committed; nothing on CT 111 disturbed.

## 8. Out of scope (Phase 1) — follow-ups with seams in place
- **Phase 2 — PG corpus stats:** a `CorpusProvider` via **`Bun.sql`** over a `dashboard.*` view layer
  (`sslmode=disable`; `jurisearch_read` lacks the working tables — analysis §6). Additive (OCP).
- **Phase 3 — email alerts/reports:** an `EmailReporter` on the producer's `[alert] hook_command` + a digest
  timer; SMTP creds via `producer.env`-style delivery.
