# jurisearch CLI Enhancement — Implementation Plan

**Date:** 2026-06-23
**Source:** `CLI-ENHANCEMENT-ANALYSIS.md` (codex-reviewed, all findings applied).
**Review:** codex-reviewed (`2026-06-23-cli-plan-codex-review.md`, FIXES_REQUIRED) — all 6 findings
applied: T0.2 expanded to schema-completeness for all 28 `COMMANDS` names (only 16 are defined today);
T1.4 split into `doctor` (T1.4a) and a DB-lifecycle task gated on a runtime-ownership design (T1.4b),
because `ManagedPostgres` stops PG on `Drop` so `start_durable` alone can't back `db start`; T0.1
help pass enumerated over every command path; T1.2 requires a document-level cursor (no post-page
dedupe); T2.1 made request-scoped (no process-env mutation; per-statement probes); T2.2 defers raw
`sql` to DB-enforced read-only and prioritizes typed introspection.
**Goal:** make the `jurisearch` CLI the *single, complete, self-describing* interface for every
French-juri-search need, with a contract that makes a future client/server split mechanical.

**Standing process gate (every code task):** modify code → **codex-review the diff → apply findings →
build/test → only then run**. No capability ships half-exposed (a command must be reachable, schema'd,
help-documented, and — unless explicitly justified — session-callable).

---

## 1. Definition of done (per command, non-negotiable invariants)

A command is "done" only when **all six touchpoints** agree:

| # | Touchpoint | File | What must exist |
|---|---|---|---|
| 1 | Clap arg struct + `Command` variant | `crates/jurisearch-cli/src/main.rs` (`enum Command` ~124) | every arg has `help=`/`long_help=`; the struct has `long_about` + ≥1 example |
| 2 | One-shot handler | `main.rs` (`emit_<cmd>` + `<cmd>_payload`) | returns the documented JSON; errors via `ErrorObject` (codes 2/3/4/5) |
| 3 | Top-level dispatch | `main.rs:528` (`match command`) | wired with input validation |
| 4 | Warm-session handler | `main.rs` (`Session<Cmd>Args` + `session_<cmd>_payload` + arm in `dispatch_session_request` ~4228) | same result as one-shot, **or** an explicit `not_implemented` with a documented reason |
| 5 | Agent contract entry | `crates/jurisearch-core/src/contract.rs` (`COMMANDS`, `CommandSpec{name,summary,request_schema,response_schema,status}`) | `status` truthful (`implemented`/`stub`) |
| 6 | JSON schemas | `crates/jurisearch-core/src/schema.rs` (`compiled_schema()`) | request + response schema bodies, incl. flags; sub-objects (`routing`/`diagnostics`/`pagination`) where emitted |

Plus: `agent_help()` text mentions it; a unit/integration test exercises it; `help <cmd> --json`
returns its schema.

> **Invariant test (add once, guards forever):** a test that asserts every `COMMANDS` entry has a
> matching schema body, a `dispatch_session_request` arm (handled or explicit `not_implemented`), and
> a clap subcommand — so the six touchpoints can never silently drift again.

---

## 2. Conventions

- **Help standard.** `<arg>`: one line on *what* + accepted format (e.g. `--as-of <YYYY-MM-DD>`).
  Command `long_about`: purpose, when-to-use (esp. `--mode` guidance: `dense` best for conceptual,
  `hybrid` auto-routes citation-shaped queries through the structured resolver), output schema name,
  and a runnable example. clap `--help` must cross-reference `help <cmd> --json`.
- **Output contract.** JSON on stdout, diagnostics on stderr (already the rule). New response fields go
  into `schema.rs` in the *same* PR that emits them — never emit an unschema'd field (this is exactly
  how `routing` slipped through).
- **Error codes.** Reuse the existing taxonomy (0 ok; 2 input/no-results/strict/validation; 3 index/
  config; 4 local dependency; 5 upstream). No new ad-hoc exit codes without adding them to the schema.
- **Session parity rule.** Default: every non-interactive command is session-callable. Any exclusion
  must be encoded as `CommandStatus` + a reason string in the contract, not an implicit `not_implemented`.

---

## 3. Phase 0 — Honesty & self-description (low effort, high leverage)

Goal: make the *current* surface truthful and fully discoverable. No new capabilities; closes the
"inline help is not the full spectrum" and "implemented-but-invisible" gaps.

### T0.1 — Help-text pass on **every** command path  · size S–M · no deps
- **Scope (enumerated — no subset):** all top-level commands `search, fetch, cite, related, context,
  expand, status, model, setup, session, batch, ingest, eval, sync, help` **and** every nested path:
  `model fetch`, `ingest {plan-archives,legi-archives,embed-chunks,backfill-legi-hierarchy}`,
  `eval {phase1,france-legi}`, `help {agent,schema}`. (Top-level list per `main.rs:124-155`.)
- **Touchpoints:** every clap arg struct + every `Command`/subcommand variant in `main.rs`.
- **Do:** add `help=`/`long_help=` to every field; add `long_about` + an example per command; document
  `--mode` selection and the silent citation-routing behavior; state `--as-of` format and `--cursor` use.
  If a command is intentionally left without a rich example, **record that exception explicitly** in this
  task (don't silently skip it).
- **Accept:** a snapshot test over the **full** command matrix (every path above) asserts non-empty arg
  docs and a `long_about`; any deliberate exception is listed in the test as an allow-list entry.

### T0.2 — Schema completeness for **all** current contract names  · size M · no deps
- **Reality:** `contract.rs` `COMMANDS` references **28** schema names; `compiled_schema()` defines only
  **16**. Undefined today: `FetchRequest`, `RelatedRequest`, `RelatedResponse`, `IngestRequest`,
  `SyncRequest`, `SyncResponse`, `SessionRequest`, `SessionResponse`, `HelpAgentRequest`,
  `HelpAgentResponse`, `HelpSchemaRequest`, `HelpSchemaResponse` (and `session_envelope` sits *outside*
  `schemas`). Adding only `routing` would leave T0.5's invariant red on day one.
- **Touchpoints:** `schema.rs` `compiled_schema()` (+ `contract.rs` if any name is wrong/duplicated).
- **Do:** (a) add `SearchResponse.routing` (`query_type`, `chosen_backend`, `candidate_count`,
  `fallback_path`) — emitted by `search_with_postgres()` but unschema'd; (b) add a schema **body or an
  explicit alias** for *every* `request_schema`/`response_schema` named in `COMMANDS`, including the
  session/help envelope schemas; bring `session_envelope` inside `schemas` (or reference it properly).
- **Accept:** a **generated check iterates every schema name in `COMMANDS`** and asserts a resolvable
  body exists (this is the data the T0.5 invariant consumes); `help schema --json` resolves all 28 names;
  the emitted-key round-trip for `search` includes `routing`.

### T0.3 — Register `eval france-legi` in the contract  · size S · no deps
- **Touchpoints:** `contract.rs` (`COMMANDS`), `schema.rs` (`EvalFranceLegiRequest/Response`),
  `agent_help()`; decide session-callability (`dispatch_session_request`).
- **Do:** it's implemented one-shot (`EvalSubcommand::FranceLegi`, `eval_france_legi_payload`) but
  invisible to the agent contract — add its spec + schema; either route it in the session or mark it
  explicitly one-shot-only with a reason (it writes an artifact file → likely one-shot-only).
- **Accept:** `help schema --json` lists `eval france-legi` with a schema and a truthful status.

### T0.4 — Resolve `fetch --as-of` / `--part`  · size S–M · no deps
- **Touchpoints:** `main.rs` `fetch_payload` (rejects at `:1554`), `retrieval.rs` `fetch_documents_json`.
- **Decision (recommend):** **wire `--as-of`** through `fetch_documents_json` for version-pinned
  retrieval (the engine already honors `as_of` in search/context), and **defer `--part`** by removing
  the flag until sub-article slicing is specced (don't ship a flag that errors).
- **Accept:** `fetch <id> --as-of <date>` returns the version-pinned record (or the flag is gone); no
  reserved-flag `bad_input` path remains in the advertised surface.

### T0.5 — Session-parity truth + the invariant test  · size S · deps: **T0.2, T0.3**
- **Touchpoints:** `contract.rs` (add a `session: bool`/reason to `CommandSpec` or a sibling table),
  `dispatch_session_request`, plus the **§1 invariant test**.
- **Do:** mark each command session-handled vs one-shot-only(+reason); add the test that fails if the
  six touchpoints drift. The schema arm of the invariant relies on T0.2 being complete (every `COMMANDS`
  name resolvable) — sequence T0.2 first so the invariant lands green, not retro-weakened.
- **Accept:** the invariant test passes over the **full** `COMMANDS` set and would fail if a new command
  skipped any of the six touchpoints (clap, handler, dispatch, session arm, contract entry, schema body).

**Phase 0 milestone:** `--help`, `help agent`, and `help schema --json` are mutually consistent and
describe the *entire real* surface; no advertised-but-rejected flags; no implemented-but-invisible
commands.

---

## 4. Phase 1 — Close the capability gaps that forced out-of-CLI work

### T1.1 — `related` / graph traversal (de-stub, typed, session-enabled)  · size M · no deps
- **Touchpoints:** `main.rs` `related_payload` (currently stub) + `RelatedArgs`; engine: reuse the
  citation/temporal edge access already proven in `france_legi.rs` (`france_legi_gold_json` mines
  `graph_edges` CITATION/temporal edges); `contract.rs`/`schema.rs` (`RelatedRequest/Response`);
  `dispatch_session_request` (remove `related` from the `not_implemented` set).
- **Do:** `related <id> --rel {cites,cited_by,temporal,sibling} --depth N` → typed, authority-scored
  edges with provenance; expose the edge taxonomy in the schema.
- **Accept:** known fixture (e.g. an article with known CITATION edges) returns the expected neighbours;
  status flips `stub`→`implemented`; session call returns identical JSON.

### T1.2 — Result shaping: doc-level + multi-mode compare + pooling  · size M–L · no deps
- **Touchpoints:** `main.rs` `SearchArgs`/`search_payload` + cursor/`next_cursor` logic
  (`main.rs:1440-1468`); engine `retrieval.rs` `hybrid_candidates_json` and the cursor predicate
  (`fused_score` + `chunk_id`, `retrieval.rs:324-335`).
- **Do NOT post-page dedupe.** The current cursor is **chunk-based**, so deduping a fetched page would
  collapse it below `top_k`, skip documents hidden behind duplicate chunks, and emit a chunk-resume
  cursor. Instead: aggregate to documents **inside the ranking query** (one row per article = its
  best-ranked chunk) **or** use an explicit overfetch-and-fill loop, and define a **document-level
  cursor** distinct from the chunk cursor.
- **Do:** `search --group-by {chunk,document}` (default `chunk` = current behavior); a `compare` verb (or
  `search --modes a,b,c`) returning aligned per-mode top-k **and** the pooled union set. The response
  cursor fields are specified **separately** for `chunk` vs `document` grouping.
- **Accept:** a pagination fixture where several top-ranked chunks belong to the **same** article still
  returns `top_k` **unique** documents on page 1 and resumes at the next document (no skip, no short
  page); `--group-by document` returns no duplicate article UIDs (BLOCKER-2 impossible at source);
  `compare` returns the structure the benchmark built by hand.

### T1.3 — General evaluation harness  · size L · deps: T1.2 (pooling/grouping)
- **Touchpoints:** new `EvalSubcommand::Run`; engine: a metrics module (P@k, recall@k, nDCG@k, MRR) +
  bootstrap CIs (resample by group); reuse `compare`/pooling from T1.2.
- **Do:** `eval run --questions FILE [--qrels FILE] --modes bm25,dense,hybrid --metrics ndcg@10,recall@10
  --group-by document --bootstrap 5000 [--judge-cmd CMD] --out artifact.json`. The CLI owns retrieval,
  pooling, blinding, scoring, and CIs; `--judge-cmd` is a **documented external-judge hook** (stdin =
  blind (question,candidates); stdout = labels) so the LLM judge stays external. This folds the entire
  `external-benchmarks/conceptual-embedding-eval/` Python pipeline into the CLI.
- **Accept:** re-running the conceptual eval through `eval run` reproduces the dense>hybrid>bm25 result
  and CIs; artifact is schema'd and deterministic given fixed inputs + judge.

### T1.4a — `doctor` preflight  · size M · no deps
- **Touchpoints:** new `Command::Doctor`; extend the existing `embedding_endpoint_status_json` probe
  (`main.rs:~4050`) rather than duplicating it; read-only health from `ingest_accounting.rs`.
- **Do:** aggregate preflight checks (PG data-dir state **without owning it**, migrations current,
  extension assets, embedding endpoint reachability + model-fingerprint compatibility, model cache,
  index/replay readiness) into one pass/fail JSON.
- **Accept:** `doctor` reports each dependency with status; session-callable; never starts/stops PG.

### T1.4b — `db {start,stop,status}` — **needs a runtime-ownership design first**  · size L · no deps
- **Blocking reality:** `ManagedPostgres::start_durable()` returns an **owning** handle that holds an
  advisory lock, and `impl Drop for ManagedPostgres` calls `self.stop()` (`runtime.rs:326-330`). A
  one-shot `db start` that just calls `start_durable()` would **start PG, return, drop the handle, and
  stop PG again** — the opposite of the feature. `start_durable`/`stop` are therefore **not sufficient**.
- **Design task (do before coding):** decide the ownership model — one of:
  (a) `db start` launches a **detached/supervised** PG (writes connection metadata; `db status`/`db stop`
  discover and manage it without re-taking the owning Drop path), or (b) `db start` is an **alias for a
  long-running `serve` daemon** (folds into T2.4), or (c) an **attach** model where the CLI connects to
  an already-running instance. Specify lock handling, metadata location, and stale-lock recovery.
- **Accept:** `db start` **returns while PG stays reachable**, `db stop` stops *that same* instance, and
  `db status` reports it — proven by a test that asserts reachability after `start` returns. Until the
  ownership model is chosen, this task is design-only.

**Phase 1 milestone:** graph traversal, doc-level/compare retrieval, a reproducible eval harness, and
dependency lifecycle all live *inside* the CLI — the psql/Python/`pg_ctl` workarounds are retired.

---

## 5. Phase 2 — Tuning, introspection, temporal depth, server

### T2.1 — Retrieval tuning surface (request-scoped, not process-global)  · size M · deps: T1.3
- **Reality:** RRF weights are read from **process env** (`rrf_weights()` `retrieval.rs:21-35`) and
  IVFFlat probes are **hard-coded** `SET ivfflat.probes = 4` inside `hybrid_candidates_json()`
  (`retrieval.rs:114-118`). Mutating env/process state per request is wrong for warm sessions and fatal
  for `serve` (concurrent requests need different weights/probes; `eval tune` needs deterministic compare).
- **Touchpoints:** add tuning fields to the retrieval options struct (`HybridCandidateQuery` or a new
  `RetrievalOptions`) so weights/probes flow as **immutable per-request options** `search_payload` →
  storage; make `SET ivfflat.probes` **SQL-local** (per-statement) from the request value, not a constant.
- **Do:** `search --rrf-lexical-weight --rrf-dense-weight --probes` populate those per-request options
  (env remains the default when unset); `eval tune --sweep rrf-dense=0.3:1.5:0.1 --against <fixture>`
  sweeps via the options struct **without changing process env**, reporting the optimum with CIs.
- **Accept:** two concurrent session requests with different weights return correctly-weighted results
  (no cross-talk); sweeping reproduces/acts on the benchmark's "raise dense weight" finding.

### T2.2 — Introspection (kill psql)  · size M
- **Priority: typed views first.** `inspect <id>` (raw canonical record), `stats [--graph --index
  --embeddings]`, `search --explain` (rank/score breakdown) — these retire psql for real workflows.
- **Raw `sql` is deferred and, if built, must be DB-enforced — not string-checked.** The engine
  primitive is `ManagedPostgres::execute_sql(&str)`, which **shells arbitrary SQL through `psql`**
  (`runtime.rs:237-239`); a hand-rolled "single SELECT" string check is insufficient (volatile
  functions, CTEs, comment/dollar-quote/multi-statement bypasses). If implemented at all, require:
  a **read-only transaction**, a **statement timeout**, a **restricted DB role** where possible, and
  **server-side statement parsing/splitting** — not ad-hoc string matching.
- **Accept:** corpus/graph/coverage fully inspectable via the typed commands *without* psql. `sql`, if
  shipped, is marked unstable in the schema and has safety tests for CTEs, comments, semicolons, COPY,
  function calls, and DDL/DML attempts (all rejected); otherwise it is explicitly out of this milestone.

### T2.3 — Temporal depth  · size M
- `versions <id>` (validity-interval timeline), `diff <id> --from D1 --to D2`, optional `timeline <code>`;
  reuse the temporal edge logic from `france_legi.rs`.
- **Accept:** version timeline + diff for a known multi-version article.

### T2.4 — `serve` (client/server)  · size L · deps: clean Phase-0/1 contract
- `serve [--http :PORT | --socket PATH] [--version v1]` exposing the **same** handlers the session
  protocol routes (transport-neutral handler refactor first); capability/version discovery; request IDs;
  stubbed authn/z + rate limiting. The warm `session` JSONL is the in-process precursor.
- **Accept:** a thin client over the socket gets byte-identical results to the one-shot CLI; capability
  discovery returns the schema.

---

## 6. Sequencing & milestones

```
M0 (Phase 0):  truthful, fully self-describing surface.   T0.1, T0.2 → T0.3,T0.4 → T0.5  (T0.2 is size M)
M1 (Phase 1):  eval/compare/graph/health in-CLI.          T1.1, T1.2 → T1.3; T1.4a parallel
M2 (Phase 2):  tuning + introspection + temporal.         T2.1 (after T1.3), T2.2, T2.3
M3 (Phase 2):  client/server + DB lifecycle.              T1.4b design → T2.4 (after the contract is stable)
```
Critical path: **T0.2 → T0.5**, **T0.* → T1.2 → T1.3 → T2.1**, and **stable contract (M0/M1) → T2.4**.
Independent/parallelizable: T1.1, T1.4a, T2.2, T2.3. **T1.4b (DB-ownership design) is a prerequisite
for any `db start` and is coupled to T2.4** — do its design before committing to `serve`.

## 7. Testing strategy

- **Six-touchpoint invariant test** (T0.5) — structural guard against drift.
- **Help snapshot tests** — `--help` for each command has non-empty arg docs.
- **Schema round-trip** — every emitted top-level response key is present in `compiled_schema()`.
- **Session ↔ one-shot equivalence** — same args → same JSON for each session-handled command.
- **Golden retrieval/eval fixtures** — `eval run` over a checked-in tiny fixture is deterministic given
  a stub judge; `--group-by document` returns dup-free UIDs.
- **`sql --read-only` safety** — rejects anything but a single SELECT.

## 8. Risks & mitigations

- **Behavior drift in `search`** (adding `--group-by`, RRF flags): default to current behavior
  (`--group-by chunk`, env-derived weights) so existing callers are unaffected; gate new behavior behind
  explicit flags.
- **`sql` escape hatch foot-gun:** read-only, single-statement, unstable-marked, never the documented way
  to do a typed task; covered by a safety test.
- **`serve` security surface:** design contract now, ship behind a flag, not open by default.
- **Scope creep:** each task ends at the six-touchpoint bar or it isn't merged — half-exposed capability
  (today's `related` stub) is worse than none.
- **Per the process gate:** each task's diff is codex-reviewed before it runs; eval-affecting changes are
  validated against the conceptual-eval fixture before claiming a result.

## 9. First concrete step

Two small, behavior-neutral PRs:
1. **T0.2 (schema completeness)** — define a body/alias for all 28 `COMMANDS` names + add `routing`.
   This is the foundation the T0.5 invariant consumes, so it goes first (it's size M, not S).
2. **T0.1 + T0.3 + T0.4** — help-text pass over the full command matrix, register `eval france-legi`,
   resolve `fetch --as-of/--part`.

Then **T0.5** (parity flags + the six-touchpoint invariant test) lands green on top. All of M0 is
no-new-capability / no-behavior-change — it just makes the CLI honestly and completely describe itself.
