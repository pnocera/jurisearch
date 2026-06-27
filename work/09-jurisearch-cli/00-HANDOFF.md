# work/09 — implementation handoff

**Status:** planning is COMPLETE, Codex-GO'd, and pushed. Implementation has **not started**.
**Start at Phase 1 of [`04-implementation-plan.md`](04-implementation-plan.md).**

This file is written for a fresh session with no prior context. The auto-loaded memories (MEMORY.md)
carry the working conventions and credentials; this file carries the work/09-specific state and the
starting point. Read this, then read `04-implementation-plan.md`.

---

## 1. What work/09 is

Move JuriSearch from a single self-contained binary to a **three-tier deployment**: a **producer**
(work/08, done) emits signed packages; a **customer-site server host** runs a shared PostgreSQL + an
auto-updating `syncd` daemon + a concurrent read-only query service with an **on-site local bge-m3**
embedder; and many **thin `jurisearch` CLI clients** reach the site service **by URL** and render the
answer. One place per site holds the corpus and the heavy stack; every other machine is near-zero-install.

## 2. The documents (read in this order)

| # | File | Role | Codex |
|---|---|---|---|
| 01 | `01-target-deployment-analysis.md` | problem, current-state, gap, forces, options (analysis only) | GO r2 |
| 02 | `02-target-architecture.md` | the settled decisions / target topology | GO r2 |
| 03 | `03-deployment-design.md` | DRY/SOLID software design — components, **interfaces**, crate graph | GO r5 |
| 04 | `04-implementation-plan.md` | **the build sequence — START HERE** | GO r2 |

`reviews/` holds every Codex review; `qa/` (repo root) holds the design consultations
(`…design-consultation…` shaped the plan; `…readiness…` validated writer-owned readiness).
**04 is authoritative for *what to build next*; 03 for *interfaces*; 02 for *why* (don't re-litigate).**

## 3. Settled decisions — do NOT re-open these

- **Embedding:** on-site **local `llama.cpp` bge-m3** on the server host; query text never leaves the
  site network; no external embedding API.
- **Client↔service:** **no auth** — clients use the **service URL** on the trusted site LAN. (The
  untrusted boundary, producer→site, stays signed/verified per work/08.)
- **Transport:** raw **JSONL** line protocol (revisit only if non-Rust clients / off-the-shelf ops tooling).
- **Concurrency:** **substrate A** — bounded worker-thread pool + blocking PG connection pool (reuse the
  synchronous code; async only if very high connection counts are ever needed).
- **Readiness:** **writer-owned**, stamped at write time, read-only lookup at query time.
- **Multi-corpus:** **one endpoint exposes all corpora**, syncd-managed; hot search fans out over the
  per-corpus physical generation schemas (never the union views).
- **HA:** **single host per site** (scale-out out of scope; the design preserves the seams).

## 4. How to build (the per-gate loop)

Work **phase by phase, sub-gate by sub-gate** per 04 (P1 · P2[2A,2B] · P3[3A,3B,3C] · P4 · P5 · P6).
For **each gate**:

1. Implement the gate's deliverables; keep the **local CLI byte-green** (golden tests) the whole time.
2. Self-validate: `cargo fmt --check`, `cargo clippy`, the gate's tests (incl. the **negative** tests
   the plan names — they prove the invariants).
3. **Codex review** via the skill — see §6. Apply **every** finding (BLOCKER/WARN/NIT), re-review to a
   clean `VERDICT: GO`.
4. **Commit straight to `main`** (no feature branch) and **push**. One commit per gate-GO.

The plan gives each gate its Goal / Builds-on / Deliverables / Invariants-under-test / Negative-tests /
Verification / Risk & rollback / Deferrals / Done-when. Treat "Invariants under test" + "Negative tests"
as the acceptance bar.

## 5. Codex usage — KEEP INSTRUCTIONS MINIMAL

This is load-bearing (the user corrected it twice this project): **do not over-direct Codex.** Give it
the artifact/scope + one line of what it is + the output format, and let it discover the issues
itself — that is what surfaces the bugs you can't see (you'll have *tête dans le guidon*). A checklist
biases the review toward what you already thought of. See `[[codex-review-no-explicit-instructions]]`.

- Skill: `/home/pierre/.claude/skills/codex-review/scripts/codex_session.sh`
  - **Ask** (design consultation, BEFORE building a consequential gate):
    `… ask --prompt-file F` → answer lands in `qa/`.
  - **Review:** `… review --workdir /home/pierre/Work/jurisearch --instructions-file F --review-file work/09-jurisearch-cli/reviews/<name>.md`
  - Run in the background and watch for the review file; verify the session is `ALIVE` (`… list`); a
    review/answer can take several minutes (the script timeout is now 900s).
  - **Re-review:** point Codex at its **own prior review file** + say findings are addressed; do NOT
    re-enumerate the fixes.
- A review is **artifact-agnostic** (these are document/code reviews) — never call it a "code review".
- Consider a Codex **design consultation** before the trickiest gates (esp. P1's wire-enum boundary,
  3B's storage-API refactor) — `[[ask-codex-before-important-decisions]]`.

## 6. Technical context the plan assumes

- **Rust workspace**, edition 2024. Existing crates: `jurisearch-{core,cli,embed,ingest,official-api,
  package,package-build,storage,syncd}`. New crates the plan introduces: `jurisearch-{contract,
  transport,render,client,query}`.
- **Managed-PG test harness:** export `JURISEARCH_PG_CONFIG=/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_config`;
  tests skip cleanly when it's absent. PG18 + pgvector + pg_search.
- **Standalone site PG (the client/site role):** this workstation runs system PG18 on `:5432`
  (postgres/postgres, db `jurisearch`) — use it for shared-server-mode work. `[[client-pg-server]]`
- **Producer data (E1):** the proxmox **bear** guest (root/20Sense20, postgres/postgres, on tailscale)
  holds a real ingested `core` corpus — use it as the producer for two-host work. `[[proxmox-producer-db]]`
- **work/08 substrate (done):** per-corpus generations behind stable views, `corpus_state` cursor,
  `activate_generation_with_guard`, signed packages + trust anchors + license, the §6.3 `Reject` codes,
  `run_catchup`, `apply_baseline`/`apply_rebaseline`/`apply_incremental`.

## 7. The traps Codex already flagged (in the plan, emphasized here)

- **2B:** the writer side (`syncd` `update`/`run_catchup`/`apply_*`/`activate_generation_with_guard`) is
  hard-bound to `&ManagedPostgres` today — refactor it onto a `StorageBackend`/writer handle **before**
  P4/P5 can run on a standalone PG. This is a structural signature refactor, not "compose existing seams".
- **3A:** readiness must restamp on **incremental** apply too (incrementals advance `corpus_state.sequence`,
  which is in the readiness signature) — not only on activation, or reads are forced to recompute/write.
- **3B (riskiest):** storage read APIs aren't snapshot-ready — `execute_read_sql` shells fresh `psql`
  sessions and read functions take `&ManagedPostgres`. A real `ReadSnapshot` needs a deeper storage-API
  change, not a wrapper.
- **Main chunk search lacks a fail-closed fingerprint preflight** (zone search has the pattern); today a
  mismatch silently degrades (hybrid → lexical) or returns false no-results (explicit dense).
- **Read-role grants don't exist yet** — "read role" is a search-path convention today, not a DB identity.
- **Keep the local CLI byte-green** — `session`/`batch`/`serve` output must not change; version-gate only
  the *new site* protocol.

## 8. Working conventions (restated from memory)

- Autonomous: don't stop except for true blockers; **ask Codex, not the user**, for decisions.
  `[[autonomous-execution]]`
- **Commit on `main` directly** (no feature branches) `[[commit-on-main-directly]]`; **commit + push per
  Codex GO** after applying its fixes `[[commit-per-codex-go]]`.
- `cargo fmt` + `cargo clippy` clean before every review.

## 9. Current git state

`main` @ **`8a2e283`** (`work/09: … implementation plan — codex GO r2`), working tree clean, in sync
with `origin/main`. All four docs + their reviews + the consultation Q&A are committed and pushed.

## 10. Start here

1. Read `04-implementation-plan.md` end-to-end (esp. the critical-path block + the compatibility matrix).
2. Begin **Phase 1** (the dependency-light `jurisearch-contract`/`-transport`/`-render` base + the
   session-command compatibility matrix), keeping the local CLI byte-green. The known hazard is the
   wire-enum cycle (`RetrievalMode`/`GroupBy`/`RetrievalOptions` live in `jurisearch-storage`) — consider
   a short Codex design consultation on the crate boundary before coding.
3. Implement → self-validate → minimal Codex review → apply all findings → GO → commit on `main` → push.
4. Proceed gate by gate through P6.
