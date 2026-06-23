# jurisearch CLI — Gap Analysis Toward "The Single Interface"

**Date:** 2026-06-23
**Author:** Claude (autonomous analysis)
**Review:** codex-reviewed (`2026-06-23-cli-analysis-codex-review.md`, FIXES_REQUIRED) — all 6 findings
applied: corrected warm-session parity (`model fetch`/`setup`/`help` *are* session-handled), added the
`eval france-legi` contract/schema gap, flagged `fetch --as-of`/`--part` as rejected-reserved, narrowed
§K (schema already has exit_codes/error-code enum/pagination/diagnostics; real gap is `routing` +
named-but-undefined `Related`/`Sync`), reframed §I (status already probes embedding endpoints), and
split evidence into must-own vs gated/optional.
**Scope:** What is missing for the `jurisearch` CLI to be the *single, complete, self-describing*
interface for every French-juri-search-related need — including a future client/server split.

---

## 0. Framing (the principle this analyzes against)

Per the stated intent:

1. **The CLI is NOT a thing to bypass.** Querying the database directly (psql, ad-hoc SQL, hand-
   written Python harnesses) is a smell: every such workaround is a capability the CLI should own.
2. **Client/server is a future split.** The CLI today is the server *and* the client in one process;
   tomorrow it may be a thin client over a daemon. The command/IO contract must be designed so that
   split is mechanical, not a rewrite.
3. **The CLI MUST be the single interface** for all French-juri-search requests.
4. **Inline help must expose the full spectrum** of capabilities — an agent (or human) should be able
   to discover *everything* the tool can do, and how, without reading the source.
5. **Capabilities must cover the full spectrum of needs** — if a legitimate task can't be done through
   the CLI, that's a gap.

This document inventories the current surface, uses **this session's own out-of-CLI workarounds as
evidence**, and proposes a prioritized set of additions.

---

## 1. Current CLI surface (verified inventory)

Top-level (`crates/jurisearch-cli/src/main.rs:124`), global `--index-dir` (env `JURISEARCH_INDEX_DIR`):

| Command | Purpose | Schema status¹ | In warm session?² |
|---|---|---|---|
| `search` | ranked candidates (`--kind --mode --format --top-k --cursor --as-of`) | implemented | ✅ |
| `fetch` | full source text for IDs | implemented — **but `--as-of`/`--part` are rejected as reserved** (`main.rs:1554`) | ✅ |
| `cite` | verify a citation (`--strict --online --as-of`) | implemented | ✅ |
| `related` | graph neighbours (`--rel`) | **stub** | ❌ not_implemented |
| `context` | structural neighbourhood (`--siblings --as-of`) | implemented | ✅ |
| `expand` | legal-vocabulary expansion | implemented | ✅ |
| `status` | coverage / fingerprints / index health (`--deep`) | implemented | ✅ |
| `model fetch` | explicit model-cache ops | implemented | ✅ |
| `setup` | check/prepare local setup | — | ✅ |
| `session` | warm JSONL subprocess protocol | — | (is the protocol) |
| `batch` | finite JSONL protocol for eval/bulk | — | (is the protocol) |
| `ingest {plan-archives,legi-archives,embed-chunks,backfill-legi-hierarchy}` | ingestion | — | ❌ not_implemented |
| `eval phase1` | built-in Phase-1 LEGI fixtures | in contract | ✅ |
| `eval france-legi` | France-LEGI benchmark artifact | **implemented one-shot but NOT in `COMMANDS`/`compiled_schema`** | ❌ |
| `sync` | synchronize official sources (`--source --since`) | named in contract, schemas undefined | ❌ not_implemented |
| `help {agent,schema}` | compiled agent contract + JSON schemas | implemented | ✅ (`help`, `help schema`) |

¹ from `help schema --json` (`status: implemented|stub`). ² from `dispatch_session_request`
(`main.rs:4228-4243`): **handled** = `help`, `help schema`, `status`, `search`, `fetch`, `cite`,
`context`, `expand`, `model fetch`, `eval phase1`, `setup`; **`related | ingest | eval (generic &
france-legi) | sync` → `not_implemented`**; unknown → `bad_input`.

**What's already excellent** (keep and build on): the `help agent` compiled contract (commands, exit
codes 0/2/3/4/5, JSONL protocol) and `help schema --json` (per-command request/response schema names
with implemented/stub status). This is a real agent-facing API contract — the right foundation for
client/server. The gaps below are about *completeness and reach*, not philosophy.

---

## 2. Evidence — what I had to do OUTSIDE the CLI this session

Each row is a task that *should* have been a CLI call but wasn't, with the workaround I used and the
capability it implies.

The **strength** column separates capabilities the typed CLI must *own* from optional *escape
hatches* that need a contract/threat-model decision before they become roadmap items (so this
session's bespoke workflow isn't blindly promoted to product requirements).

| Need (this session) | Out-of-CLI workaround I used | Missing CLI capability | Strength |
|---|---|---|---|
| Run a query in 3 modes and compare | 72× `subprocess` shelling, parsed JSON in Python | **batch/compare search**, multi-mode in one call | must own |
| Treat results as **documents** not chunks | Python dedupe of `top_uids` by article UID | `search --group-by document` / doc-level results | must own |
| Pool candidates across modes | Python union/dedup | **pooling** built into a compare/eval command | must own |
| Score P@k / recall@k / nDCG@k | hand-written `score.py` | **metrics** in a general eval harness | must own |
| Relevance judgments | external LLM (codex) + my own glue | eval harness with a **pluggable judge hook** | optional — needs contract/threat-model (judge is external) |
| Inspect `graph_edges`, counts, edge types | `psql` directly | `inspect` / `stats` typed views (graph, index, coverage) | must own (typed); raw `sql` = **gated escape hatch** |
| Start/stop embedded PG for a run | manual `pg_ctl -D … start/stop` | **lifecycle**: `db {start,stop,status}` / `serve` | must own |
| Know the embedding server (`:8097`) was up | tribal knowledge / curl | `doctor` preflight (extends `status`'s existing endpoint probe) | must own |
| Pin/compare temporal versions | `--as-of` per call + eyeballing | `versions <id>` / `diff <id> --from --to` | must own |
| Re-tune RRF dense weight | would need `JURISEARCH_RRF_DENSE_WEIGHT` env + manual loop | `--rrf-*-weight` flags + `tune/sweep` | must own |

Conclusion: the CLI covers **single-shot retrieval and verification well**, but **evaluation,
introspection, tuning, lifecycle, and multi-query orchestration live entirely outside it today**.

---

## 3. Gap analysis (by theme)

### A. Help completeness — *inline help is not the full spectrum* (HIGH)

- **Per-flag help is empty on the core commands.** `search --help` shows `<QUERY>` with **no
  description** and every flag (`--kind --mode --format --top-k --cursor --as-of`) with **no `help=`
  text** — only the enum's possible values. `SearchArgs/FetchArgs/CiteArgs/RelatedArgs/ContextArgs`
  (`main.rs:158-295`) have **no doc comments**, while newer `EvalFranceLegiArgs`/`IngestSubcommand`
  **do**. Inconsistent and below the "full spectrum" bar.
- **No semantics or guidance in help.** Nothing tells the caller *when* to use `bm25` vs `dense` vs
  `hybrid` (this session proved it matters: dense wins on conceptual queries, hybrid auto-routes
  citation-shaped queries), what `--as-of` format is (`YYYY-MM-DD`), what `--cursor` is, or that
  hybrid silently routes citation-shaped queries through a structured resolver.
- **No examples and no output-schema cross-reference in `--help`.** The rich contract lives only in
  `help agent` / `help schema`; a caller doing `search --help` is left blind. The two help surfaces
  (clap `--help` vs compiled `help agent`) are disconnected.

**Fix:** add `help=`/`long_help=` to every arg; add a `long_about` per command with 1–2 examples,
the output schema name, mode-selection guidance, and routing behavior; cross-link `--help` →
`help schema <command> --json`.

### B. Surface parity & dead stubs (HIGH)

The warm session is **more complete than I first stated** — `dispatch_session_request`
(`main.rs:4228-4243`) handles `help`, `help schema`, `status`, `search`, `fetch`, `cite`, `context`,
`expand`, **`model fetch`**, `eval phase1`, and **`setup`**. The precise three-way split is:

- **Implemented in session:** `help`, `help schema`, `status`, `search`, `fetch`, `cite`, `context`,
  `expand`, `model fetch`, `eval phase1`, `setup`.
- **Missing / stubbed in session:** `related` (also a stub one-shot — see §C), `ingest` subcommands,
  **`eval france-legi`** (and any future generic `eval run`), `sync` → all return `not_implemented`.
- **One-shot-only by design (if any):** must be stated explicitly in `help schema` with a reason —
  currently nothing marks a command as deliberately session-excluded, so the gaps above read as
  oversights rather than decisions.

An agent that adopts the warm protocol silently loses graph traversal, ingestion, the France-LEGI
benchmark, and sync. The protocol should reach **every** non-interactive capability, or the contract
should declare and justify each exclusion.

**Fix:** define one capability set; make session/batch dispatch a superset router over the same
handlers; mark genuinely one-shot-only commands explicitly in `help schema`.

### C. Cross-reference & graph traversal — first-class, not a stub (HIGH)

This session established that "what does article A cite / what cites A" is a **graph lookup** over
`graph_edges` CITATION edges — data that already exists — not an NLP problem. Yet `related` is a
stub and not in the session protocol. The engine has the data (`france_legi_gold_json` already mines
citation/temporal edges in `france_legi.rs`).

**Fix:** implement `related <id> --rel {cites,cited_by,temporal,sibling,…} --depth N` returning typed,
authority-scored edges; expose the edge taxonomy in `help schema`; add it to the session protocol.

### D. Temporal capabilities beyond `--as-of` (MEDIUM)

`--as-of` pins *one* version. There is no way to **list** an article's version timeline, **diff** two
versions, or ask "what changed in code X between D1 and D2" — even though the eval's `temporal_sql`
already computes version sets structurally.

**Fix:** `versions <id>` (timeline of validity intervals), `diff <id> --from D1 --to D2`,
optionally `timeline <code> --since`.

### E. Evaluation harness generality (HIGH — biggest "needs" gap)

`eval` only runs **built-in, structurally-derived fixtures** (`phase1`, `france-legi`). There is no
way to:
- run an eval over a **custom** question set / qrels file,
- choose **modes to compare** and get a per-mode metric table,
- **pool** candidates and attach **external relevance judgments** (LLM-judged or human),
- compute a **metric library** (P@k, recall@k, nDCG@k, MRR) with **uncertainty** (bootstrap CIs).

I rebuilt *all* of this in `external-benchmarks/conceptual-embedding-eval/` (run_retrieval.py +
build_judge_input.py + score.py + an external judge). That entire pipeline is evidence of a missing
first-class subsystem.

**Fix:** `eval run --questions FILE --qrels FILE --modes bm25,dense,hybrid --metrics ndcg@10,recall@10
--group-by document --bootstrap 5000 --out artifact.json`, plus a documented **judge hook**
(`--judge-cmd` / pluggable) so the CLI owns pooling, blinding, scoring, and CIs while the judge stays
external. This makes evals reproducible and keeps them inside the single interface.

### F. Result shaping — chunk vs document, pooling, compare (HIGH)

`search` returns **chunk-level** candidates; callers must dedupe to articles themselves (this
session's BLOCKER-2 bug came from exactly this). No `--group-by`, no multi-mode compare, no pooling.

**Fix:** `search --group-by {chunk,document}` (doc-level aggregates best chunk rank); a `compare`
verb (or `search --modes a,b,c`) returning aligned per-mode top-k + a pooled set; ensure cursor
pagination semantics are consistent across grouping.

### G. Retrieval tuning surface (MEDIUM)

`rrf_weights()` reads **env-only** (`JURISEARCH_RRF_LEXICAL_WEIGHT` / `_DENSE_WEIGHT`,
`retrieval.rs:23`); IVFFlat `lists` is an ingest flag and `probes` isn't surfaced at all. The
benchmark's actionable finding — *raise the dense RRF weight; hybrid currently loses to dense* —
cannot be acted on through the CLI without env juggling and manual loops.

**Fix:** `search --rrf-lexical-weight --rrf-dense-weight --probes` flags (override env); a
`tune`/`calibrate` command that sweeps weights against an eval fixture and reports the optimum with
CIs. Pairs naturally with §E.

### H. Introspection & diagnostics — so psql is never needed (MEDIUM)

No way to inspect the corpus/graph without psql: counts by source/kind, edge-type histogram, a
document's raw record, index parameters, embedding coverage. `ManagedPostgres::execute_sql` exists in
the engine but is not reachable.

**Fix:** `inspect <id>` (raw canonical record), `stats` (corpus/graph/index/embedding coverage), an
optional **gated** `sql --read-only` escape hatch (clearly marked unstable/foot-gun), and a richer
`search --explain` (scoring/rank breakdown beyond the current per-candidate scores).

### I. Runtime & dependency lifecycle/health (MEDIUM, blocks client/server)

**Partial coverage already exists:** `status` reports embedding configuration and
`EmbeddingEndpointStatus`, and `embedding_endpoint_status_json()` (`main.rs:~4050`) probes local
loopback embedding endpoints with a TCP connection — so endpoint reachability is *not* absent. What's
missing is a **comprehensive `doctor`/preflight** and an **explicit DB lifecycle** command (I still
started/stopped PG by hand via `pg_ctl`). The engine already has `start_durable`/`stop`.

**Fix:** `doctor` (preflight that *aggregates* checks: PG data-dir status without opening/owning it,
migrations current, extension assets present, embedding endpoint reachability **and model/fingerprint
compatibility**, model cache, replay/index readiness) and `db {start,stop,status}` for explicit
lifecycle. Extend — don't duplicate — the existing endpoint probe.

### J. Client/server readiness (STRUCTURAL — design now, build later)

To make the split mechanical:
- **`serve`** — a long-running daemon (HTTP/gRPC/unix-socket) exposing the *same* command handlers the
  session protocol already routes; the warm `session` JSONL is the in-process precursor.
- **Versioned API contract** — `help agent` has `schema_version: 1`; promote that to a real
  compatibility guarantee with a capability/version discovery call.
- **Concurrency / connection management** — today every invocation cold-starts PG (~2s warm here,
  cold otherwise). A daemon amortizes this and is required for a client/server world.
- **AuthN/Z, rate limiting, request IDs** — at least stubbed in the contract.
- **Transport-neutral handlers** — ensure command logic is independent of stdin/stdout vs socket.

### K. Output contract & schema completeness (MEDIUM)

`help schema --json` is already strong: `compiled_schema()` (`jurisearch-core/src/schema.rs`) **does**
include `exit_codes`, an `error_object.code` enum, `SearchResponse.pagination`, and
`SearchResponse.diagnostics`. The real, *narrower* gaps are:

- **`SearchResponse.routing` is emitted but unschema'd** — `search_with_postgres()` adds
  `routing.{query_type,chosen_backend,...}` (grep of schema.rs finds zero `routing`), yet the
  benchmark depended on it. Add it: `query_type`, `chosen_backend`, `candidate_count`, `fallback_path`.
- **Named-but-undefined schemas:** `RelatedRequest`/`RelatedResponse` and `SyncRequest`/`SyncResponse`
  appear in `COMMANDS` (`contract.rs:89,159`) but have no schema body — define them (placeholder or real).
- **`eval france-legi` is absent from the contract entirely** — add `EvalFranceLegiRequest`/`...Response`
  (see the dedicated P0 item below).
- **Per-flag metadata is shallow** — extend each command's schema with its flags; add
  `help <command> --json`.

(Drop the earlier overclaim that error-code enums / pagination / diagnostics are unrepresented — they
are.)

---

## 4. Prioritized roadmap

**P0 — make the existing surface honestly complete & self-describing (low effort, high leverage)**
1. Fill `help=`/`long_help=` on **all** args; add `long_about` + examples + mode/routing guidance (§A).
2. Add `SearchResponse.routing` to the schema (currently emitted but absent); define the named-but-empty
   `RelatedRequest/Response` and `SyncRequest/Response` (§K).
3. **Register `eval france-legi` in `COMMANDS` + add `EvalFranceLegiRequest/Response` schema** — it's
   implemented one-shot (`EvalSubcommand::FranceLegi`, `eval_france_legi_payload`) but invisible to the
   agent contract; decide session-callable vs explicitly one-shot-only (§B/§K).
4. **Resolve `fetch --as-of`/`--part`**: they parse but are rejected with `bad_input` (`main.rs:1554`).
   Either implement version-pinned/sliced fetch or remove the flags; document which primitive pins a
   document version (this interacts with §D `versions`/`diff`).
5. Resolve stubs/parity: implement or explicitly mark `related` and the session `not_implemented` set;
   align session ↔ one-shot (§B).

**P1 — close the biggest capability gaps that forced out-of-CLI work**
6. `related`/graph traversal as a real, typed, session-available command (§C).
7. General **eval harness**: custom questions/qrels, mode compare, pooling, metric library + bootstrap
   CIs, pluggable judge hook (§E) — folds this session's Python pipeline into the CLI.
8. Result shaping: `--group-by document`, multi-mode `compare`, pooling (§F).
9. `doctor` + `db {start,stop,status}` so no manual `pg_ctl` and a clean preflight (§I).

**P2 — tuning, introspection, temporal depth, server**
10. RRF/probes flags + `tune`/`calibrate` sweep (§G).
11. `inspect`/`stats`/gated read-only `sql`/`search --explain` (§H).
12. `versions`/`diff`/`timeline` (§D).
13. `serve` daemon + versioned capability discovery; transport-neutral handlers (§J).

---

## 5. Proposed command surface (target state, abbreviated)

```
jurisearch
  search   <q> [--mode --kind --group-by {chunk,document}] [--modes a,b,c] [--explain]
                [--rrf-lexical-weight --rrf-dense-weight --probes] [--as-of --top-k --cursor --format]
  compare  <q> --modes bm25,dense,hybrid [--group-by document]        # aligned top-k + pooled set
  fetch    <ids…> [--as-of --part]
  cite     <cite> [--strict --online --as-of]
  related  <id> --rel {cites,cited_by,temporal,sibling} [--depth N]    # implement + session-enable
  versions <id> | diff <id> --from D1 --to D2 | timeline <code>        # temporal depth
  context  <id> [--siblings --as-of]
  expand   <q>
  inspect  <id> | stats [--graph --index --embeddings] | sql --read-only "<SELECT…>"   # introspection
  eval     phase1 | france-legi
           | run --questions F [--qrels F] --modes … --metrics … [--judge-cmd …] [--bootstrap N --out F]
           | tune --sweep rrf-dense=0.3:1.5:0.1 --against <fixture>     # tuning
  status [--deep] | doctor | db {start,stop,status}                    # health & lifecycle
  ingest {plan-archives,legi-archives,embed-chunks,backfill-legi-hierarchy}
  sync [--source --since]
  serve [--http :PORT | --socket PATH] [--version v1]                  # client/server
  help {agent, schema [--json], <command> [--json]}
  model fetch | setup
```
Every command: rich `--help`, a documented JSON request/response schema, available in the warm session
unless explicitly one-shot-only.

---

## 6. Non-goals / risks

- **Raw `sql` is a foot-gun.** Gate it read-only, mark it unstable in the schema, never let it be the
  *blessed* way to do something a typed command should own. Its existence is a backstop, not an API.
- **`serve` adds a security surface** (auth, input validation, resource limits) — design the contract
  now, implement behind a flag, don't ship it open by default.
- **Scope creep vs the "single interface" goal.** Each addition must be reachable, schema'd, and help-
  documented — a half-exposed capability (like today's `related` stub) is worse than none.
- **Don't fragment help.** Keep clap `--help`, `help agent`, and `help schema` consistent and
  cross-referenced; ideally generate the agent contract from the same metadata as `--help`.

---

## 7. One-paragraph bottom line

The CLI is a well-architected **single-shot retrieval/verification** tool with a genuine agent
contract, but it is **not yet the single interface**: evaluation, multi-query orchestration, result
shaping (doc vs chunk), graph/cross-reference traversal, temporal depth, tuning, introspection, and
lifecycle/health all currently require bypassing it (psql, bespoke Python, manual `pg_ctl`), and its
**inline help under-documents even what exists**. The highest-leverage first move is **P0: make the
current surface honest and self-describing** (fill help text, document the response/error contract,
fix the `related` stub and session parity); then **P1: fold this session's out-of-CLI eval/compare
pipeline and graph traversal into first-class commands**. Doing so also lays the exact contract a
future `serve`/client-server split needs.
