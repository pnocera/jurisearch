# JuriSearch site-server + thin-client — implementation plan

Date: 2026-06-27
Scope: the **executable, risk-gated build sequence** that turns the design
([`03-deployment-design.md`](03-deployment-design.md)) into shipped software. It assumes the analysis
(01) and architecture (02) as settled, and the producer (work/08) as a given. Shaped by a Codex design
consultation on the *approach* before writing (recorded in `qa/…design-consultation…`).

This is a plan, not a design: it sequences work, names the invariants each step must prove, and the
verification — it does not re-specify interfaces (see 03).

---

## Guiding principles

1. **Keep the existing local CLI green at every step.** Extract the authorities the monolith also
   consumes; local `session --jsonl` / `batch --jsonl` / `serve` byte-behaviour is preserved (golden
   tests). The new *site* protocol is added alongside, not in place of, the local one.
2. **Risk-gated, not bottom-up-pure.** The read-path refactor is the single riskiest area; it is split
   into small gates (3A→3B→3C), each with hard invariant + **negative** tests.
3. **Additive, not a rewrite.** Shared-server mode sits alongside the existing self-managed
   `ManagedPostgres` (producer/dev unchanged); every gate can fall back.
4. **Invariant-driven.** Every phase/sub-gate names non-negotiable invariants proven by negative tests,
   on the managed-PG harness (+ the work/08 loopback pattern).
5. **Codex-reviewed + committed on GO per gate.** A phase with sub-gates is reviewed at each sub-gate —
   a single review over all of "the read path" would be too broad.

## Critical path (read this first)

```
contract/codec/render → shared-server roles + write path → writer readiness → snapshot store+builders → multi-corpus fan-out → query service → thin client
   (P1: enabling seams)   (P2: foundational)                 (P3A)             (P3B, riskiest gate)        (P3C, largest change)        (P4)            (P6)

• contract/codec/render are ENABLING SEAMS, not product milestones on their own.
• response-builder extraction is NOT in the foundation phase — it waits for the snapshot store (3B).
• P2 includes a writer/update PATH gate (2B): the existing syncd/apply/activation are hard-bound to
  `&ManagedPostgres` today, so they must accept a StorageBackend/writer handle BEFORE either the query
  service (P4, populated via one-shot `syncd update`) or the daemon (P5) can run on a standalone PG.
• writer-owned readiness (3A) must fire on EVERY topology-changing writer commit — activation AND
  incremental apply (which advances the sequence) — not only activation.
• syncd daemon (P5) is NOT on the query service's critical path (P4 is tested against one-shot update).
• multi-corpus fan-out (3C) is the largest search-semantics change and gets its own gate.
```

## Session-surface compatibility matrix (decided here, enforced in P1/P4)

The current `dispatch_session_request` is broader than the target site API. Each command's disposition;
the P1/P4 allowlist test is **table-driven from this matrix** — every non-exposed command must be denied,
not just the examples.

| Command | Local `session`/`serve` | Site service API | Disposition |
|---|---|---|---|
| `search` `fetch` `cite` `related` `context` `compare` `status` | kept | **exposed** | the site query surface (the `Operation` set) |
| `expand` | kept | — | local only (query helper, not a site op) |
| `model` (fetch) | kept | — | local/management (model-asset ops) |
| `eval` | kept | — | local/management (evaluation harness) |
| `setup` `doctor` `stats` `inspect` `versions` `diff` `help` `schema` `exit` | kept | — | local/management/diagnostics/control |
| every session-**excluded** one-shot command (ingest, producer-side, …) | one-shot only | — | never on either session surface |

The site dispatcher registers **only** the exposed set; everything else is absent by construction and
rejected with a session `ErrorObject` (never routed to the local dispatcher or an `index_dir`-aware
payload, never a package `Reject`).

---

## Phase 1 — Contract / codec / render foundation + API inventory

- **Goal.** Stand up the dependency-light base seams and fix the API surface, with **zero** behaviour
  change to the local CLI.
- **Builds on.** `jurisearch-core::session` (existing).
- **Deliverables.** `jurisearch-contract` (`SessionRequest`/`Response` reused, `ProtocolEnvelope` +
  `ProtocolVersion`, `Operation` + `parse_command`/`as_command` + typed `RequestDto` + `parse_args`,
  `ErrorObject`); **break the wire-enum cycle** — `RetrievalMode`/`GroupBy`/`RetrievalOptions` live in
  `jurisearch-storage` today, so **make the ownership direction explicit**: either move them down to
  `jurisearch-contract`, or keep them in storage with conversions in the storage/query crates so
  `jurisearch-contract` never depends on storage (no duplication, no back-edge). `jurisearch-transport`
  (JSONL codec: encode/decode, framing, max-line, version rejection, transport errors — no heavy deps).
  `jurisearch-render` (**first target: byte-parity + response unwrapping**, not a rich renderer).
  Re-wire local `session`/`batch`/`serve` onto contract+codec+render with **identical output bytes**;
  version-gate only the *new site* protocol (local bare-`SessionRequest` parsing preserved). The
  compatibility matrix above is an output of this phase.
- **Invariants under test.** The three base crates have a **clean dependency cone** (no
  `jurisearch-storage`/`-embed`/`-ingest`/`-cli`, no `postgres`, no model/runtime deps); local
  `session`/`batch`/`serve` output is byte-identical to today; a versioned site frame round-trips; an
  unversioned/legacy frame is rejected on the *site* path but still accepted on the *local* path.
- **Compatibility surface.** All current local session/serve byte behaviour; no command removed yet.
- **Negative tests.** Malformed/oversize frame rejected; unknown `command` → `ErrorObject` (not a
  `Reject`).
- **Verification.** A **`cargo tree` dependency-cone assertion** on the three base crates (caught here,
  not deferred to the thin client); golden byte-parity tests for the local paths; codec round-trip +
  version-rejection unit tests.
- **Risk & rollback.** The wire-enum move is the main risk (cycles/duplication) — if large, ship
  conversion types instead of moving enums. Crates are purely additive; easy revert.
- **Deferrals.** Response-builder extraction (→ 3B); the site service (→ P4).
- **Done-when.** Base crates exist with a clean cone, local CLI byte-green, compatibility matrix
  committed.

## Phase 2 — Shared-server storage, roles, and the writer/update path

### 2A — Backend, pools, roles, activation read-visibility

- **Goal.** Attach to an existing PostgreSQL as a client with separate read-only/writer identities;
  keep self-managed green.
- **Builds on.** `jurisearch-storage` runtime/generations.
- **Deliverables.** `StorageBackend`: `SharedServerBackend` (attach via connection config; read pool +
  writer pool) + `ManagedPostgresBackend` (existing). Least-privilege DB **roles** (SELECT-only read
  role; writer role) + grant DDL. An **activation-time grant path**: activation grants the read role
  visibility of each new generation schema + `corpus_state`/`index_manifest`/views, inside the switch
  transaction. (Today "read role" is a search-path convention, not an identity — this is new.)
- **Invariants under test.** The read identity **cannot** INSERT/UPDATE/DELETE; after activation
  commits, the read identity can read `corpus_state`, `index_manifest`, stable views, and the new
  physical generation schema(s) — **else apply fails, cursor unchanged**.
- **Negative tests.** Read-role write → permission denied; activation that fails to grant visibility →
  apply aborts, cursor unchanged; a generation the read role can't see is never left active.
- **Verification.** Harness test creating a real SELECT-only + writer role; activation→visibility
  postcondition test.

### 2B — Shared-server one-shot writer/update path

- **Goal.** Make the existing writer machinery run against a **standalone PG with the writer identity** —
  the prerequisite for populating a site PG (P4) and for the daemon (P5). *(Codex flagged that today's
  writer path is hard-bound to `&ManagedPostgres`; "compose existing seams" hides this refactor.)*
- **Builds on.** 2A.
- **Deliverables.** Refactor `jurisearch-syncd` `trust`/`subscribe`/`status`/`update`, `run_catchup`,
  `apply_baseline`/`apply_rebaseline`/`apply_incremental`, and `activate_generation_with_guard` to accept
  a `StorageBackend`/writer handle (connection provider) instead of `&ManagedPostgres` — **preserving the
  self-managed adapter** so the producer/dev one-shot path is unchanged.
- **Invariants under test.** Baseline, re-baseline, **and incremental** apply succeed against a
  standalone PG using the writer role; afterwards the read role can read the resulting active topology
  (chains to 2A's visibility postcondition).
- **Compatibility surface.** Self-managed one-shot `syncd` behaviour unchanged.
- **Negative tests.** Writer handle with insufficient privilege → apply fails cleanly; self-managed
  adapter still passes the existing work/08 loopback tests.
- **Verification.** Run the work/08 baseline/rebaseline/incremental loopbacks against a standalone PG via
  the writer handle (not a managed temp instance).
- **Risk & rollback.** This is a structural signature refactor across syncd/apply/activation; rollback =
  the `ManagedPostgresBackend` adapter keeps the current path working.
- **Deferrals.** The daemon loop (→ P5); the read path (→ P3).
- **Done-when.** One-shot baseline/rebaseline/incremental apply proven against a standalone PG with the
  writer role, read-role visible.

## Phase 3 — Read-only-safe read path *(three gates, reviewed separately)*

### 3A — Writer-owned readiness (every topology-changing commit) + coverage gate + fingerprint preflight

- **Goal.** Remove the query-time readiness write; main chunk search fails **closed** on fingerprint
  mismatch. Single-corpus only.
- **Builds on.** 2A/2B writer path; work/08 apply.
- **Deliverables.** A **single writer-owned readiness-stamp helper** invoked by **every writer commit
  that changes the active read topology — baseline/rebaseline *activation* AND incremental apply**
  (incrementals advance `corpus_state.sequence`, which is part of the readiness signature, so they too
  must restamp). The helper computes projection+dense coverage over the new/mutated active generation and
  upserts the stamp **in the same transaction** before/with the cursor advance; incomplete coverage or a
  stamp failure **aborts the transaction, cursor unchanged**. The read path becomes a **read-only
  lookup**; a missing/stale stamp → a clear writer/apply error, never a recompute. Main chunk-search
  **fail-closed fingerprint preflight** (the pattern zone search already has).
- **Invariants under test.** No write on the read path — including **after an incremental**; missing/
  stale readiness stamp errors (never recomputes); mismatched fingerprint errors **before** retrieval.
- **Negative tests.** SELECT-only role + cleared cache → every read op behaves (no write); **a successful
  incremental followed by a SELECT-only read → no recompute/write**; an incremental with incomplete
  coverage → aborts, cursor unchanged; stale stamp after a sequence advance → error; wrong fingerprint
  errors in **both** hybrid (no silent lexical fallback) and explicit-dense (no false no-results).
- **Verification.** Harness tests (SELECT-only role; coverage gate on activation AND incremental;
  preflight) — single corpus.
- **Risk & rollback.** Touches both apply paths; rollback = legacy compute-on-read stays behind
  self-managed/local mode while the shared-server read role uses the stamp.
- **Deferrals.** Multi-corpus (→ 3C); the snapshot transaction boundary (→ 3B).
- **Done-when.** Read path does zero writes under a SELECT-only role across activation *and* incremental;
  preflight fail-closed; coverage gate enforced on every topology-changing commit.

### 3B — Snapshot `QueryStore` + side-effect-free response builders (single-corpus parity) *(riskiest gate)*

- **Goal.** One request = one read snapshot; CLI payloads become **adapters** over extracted
  side-effect-free builders.
- **Builds on.** P1 (contract/render), 2A (backend), 3A (readiness).
- **Deliverables.** **Storage signature refactor**: read functions (fetch/context/related/hybrid/…)
  accept a snapshot/client handle instead of `&ManagedPostgres` + internal `execute_read_sql` (which
  shells fresh `psql` sessions today); resolve `corpus_state` + `SET LOCAL search_path` + readiness +
  retrieval inside **one** transaction. `QueryStore` (object-safe `begin_snapshot` →
  `Box<dyn ReadSnapshot>`); the single `ActiveCorpusResolver` used in-snapshot. Extract per-operation
  **side-effect-free response builders** (`RequestDto` + `ReadSnapshot` + `Embedder` → body); refactor
  CLI `*_payload` into thin adapters (CLI arg validation + index open) over the same builders.
- **Invariants under test.** One request observes one active topology (a swap mid-request is invisible);
  the response builder does no `index_dir` resolution / no PG start / no write; CLI output stays
  byte-identical.
- **Compatibility surface.** CLI command outputs unchanged (byte-parity golden tests).
- **Negative tests.** Concurrent activation during a request → request sees wholly-old-or-new; a builder
  given a snapshot cannot start its own PG.
- **Verification.** Concurrent swap/read test; byte-parity tests; single-corpus retrieval parity vs
  today.
- **Risk & rollback.** Deeper than an adapter (the storage read API is not snapshot-ready today);
  rollback = adapters fall back to the legacy path under self-managed mode.
- **Deferrals.** Multi-corpus fan-out (→ 3C).
- **Done-when.** Snapshot-bound single-corpus read path with extracted builders; parity green;
  concurrent-swap invariant proven.

### 3C — Multi-corpus physical-generation fan-out + fusion *(largest search-semantics change)*

- **Goal.** All-corpus hot search over **physical generation schemas** (not union views), per-corpus
  compatibility, fuse/paginate.
- **Builds on.** 3B (snapshot store), 3A (per-corpus fingerprint).
- **Deliverables.** Resolve the active corpus set in-snapshot; run schema-qualified (or per-arm
  `SET LOCAL search_path`) indexed search per active physical generation; **fuse (RRF) + paginate**
  above the arms; **operation-scoped** preflight (every corpus touched must match, else fail closed).
  Union views reserved for non-indexed reads/status.
- **Invariants under test.** Hot search hits physical-generation indexes (never union views); the
  cross-corpus result is a correct fusion; a per-corpus fingerprint mismatch fails the request closed.
- **Negative tests.** 2-corpus setup → indexed plan per arm (assert no union-view scan); one corpus
  fingerprint-mismatched → request errors (not partial); pagination/cursor stable across arms.
- **Verification.** 2-corpus harness tests for fan-out plan, fusion, pagination, authority rerank,
  decision filters, and `--zone` behaviour (regression-sensitive).
- **Risk & rollback.** Unqualified SQL + schema routing is fiddly; rollback = the single-corpus path
  (3B) stays correct; multi-corpus gated behind >1 active corpus.
- **Deferrals.** —
- **Done-when.** Multi-corpus fan-out + fusion + per-corpus preflight proven on a 2-corpus harness.

## Phase 4 — Query service: walking skeleton → concurrent service (UDS/loopback) + health

- **Goal.** Introduce `jurisearch-query` + the site dispatcher; prove topology early via a skeleton,
  then the full concurrent service. Built/tested against a one-shot `syncd update` (P2B) — the daemon is
  not required.
- **Builds on.** P1 (contract/codec/render), 2A (read role) + 2B (to populate the site PG), 3B
  (snapshot/builders); full search needs 3C.
- **Deliverables.**
  - **4A walking skeleton** — real JSONL codec + `ProtocolEnvelope` + allowlist + server-owned context
    (strips client `index_dir`) + read-only DB identity + **one safe op (`fetch` or `status`, not dense
    search)** + render parity, over a single pooled connection, UDS/loopback only. Does **not** stub
    readiness or read-only safety. (Can land as soon as 3B is in.)
  - **4B concurrent service** — bounded worker-thread pool + blocking PG read pool (substrate A); a
    long-lived `Send`/`Sync` local **bge-m3 embedder** (or a worker/queue) with a concurrency limit;
    size/idle limits; the full `Operation` set incl. multi-corpus search.
  - **Health/status endpoint in THIS phase** — active generation, readiness stamp, server+active
    embedder fingerprint, pool status.
- **Invariants under test.** Client `index_dir` ignored/rejected; **every** command outside the
  `Operation` set rejected by the allowlist; the service performs no writes; concurrent clients served in
  parallel; one request = one snapshot.
- **Compatibility surface.** The site dispatcher is a NEW surface; the local dispatcher is untouched.
- **Negative tests.** **Table-driven from the compatibility matrix**: every non-exposed command
  (`expand`, `model`, `eval`, `setup`, `doctor`, `stats`, `inspect`, `versions`, `diff`, `help`,
  `schema`, `exit`, and session-excluded one-shots) → session `ErrorObject` from the site dispatcher,
  reaching neither the local dispatcher nor any `index_dir`-aware payload. Client supplies `index_dir` →
  ignored; unversioned site frame → rejected; embedder-fingerprint mismatch → error before retrieval.
- **Verification.** Skeleton e2e (fetch/status over UDS); concurrency/load test; the table-driven
  allowlist + `index_dir` + version-rejection negatives; health-output asserts.
- **Risk & rollback.** Embedder thread-safety + pool sizing; rollback = skeleton stays, concurrency
  conservative.
- **Deferrals.** LAN bind (→ P6); thin client (→ P6).
- **Done-when.** Concurrent read-only UDS service answering the full query surface with health; skeleton
  + service negatives green.

## Phase 5 — syncd daemon loop over the existing planner/apply substrate

- **Goal.** Daemonize; **compose** existing seams; do not rewrite apply (writer-owned readiness already
  landed in 3A; the writer/update path landed in 2B).
- **Builds on.** 2B (writer path), 3A (writer readiness), work/08 planner/apply.
- **Deliverables.** A `run` daemon: `PackageSource` + `TrustVerifier` + `Clock` seams; poll → plan
  (existing `run_catchup`) → verify → apply on an interval; retry/backoff; structured logging; graceful
  shutdown; configuration. The only writer; serialized; treats lock-timeout activation as retryable.
  systemd unit.
- **Invariants under test.** Single writer; warn-and-reject leaves the cursor unchanged; online swap
  (the live query service keeps serving across an apply).
- **Negative tests.** Bad/unauthorized package → rejected, cursor untouched; lock-timeout activation →
  retried; a second daemon → blocks on the advisory lock.
- **Verification.** Catch-up loop test (offline→head); apply-during-live-read coordination (reuse the
  work/08 concurrency-soak pattern); systemd dry-run.
- **Risk & rollback.** Mostly operational; rollback = one-shot `update` (2B) still works.
- **Deferrals.** —
- **Done-when.** The daemon converges a corpus to head on a timer, online, against the shared server,
  with a live reader unaffected.

## Phase 6 — Thin client, LAN exposure, protocol skew, ops, two-host acceptance

- **Goal.** Ship the structurally separate thin artifact and operate the site.
- **Builds on.** P1 (base crates), P4 (service).
- **Deliverables.** `jurisearch-client` (depends **only** on contract+transport+render + `JsonlClient`;
  addressed by service URL; identical rendering; clear errors on unreachable / version-skew; optional
  `--local` dev fallback). LAN exposure: bind the query service to the trusted site network (URL, **no
  auth** per decision); protocol-version negotiation rejects skewed peers loudly. Ops: systemd units
  (syncd, query service, local bge-m3) + config; site-PG backup/restore guidance. Acceptance evidence,
  by layer: (a) the serve-site SERVICE path — handlers, dispatch, the writer-owned read gate, the full
  operation set, and one-shot render parity — is proven by the AUTOMATED in-process E2E, which ALSO
  proves protocol-version skew rejection (`…rejects_an_old_servers_unversioned_reply` + the transport
  response-envelope skew tests); (b) the shipped `jurisearch-client` operator surface — the contract seam
  (each leg asserts the contract's OWN diagnostic, not just a non-zero exit) + connection/URL diagnostics
  — is proven by a checked-in SINGLE-HOST operated capture of the real binary
  (`scripts/single-host-acceptance.sh`). The
  shipped serve-site PROCESS run (bind + DB connect + embedder-from-env + answering status/fetch/search +
  a fetch-hash) requires an operator-provisioned site DB + embedder; the SAME script runs it where those
  prerequisites exist, as does the genuine TWO-physical-host operator RUNBOOK (`05-two-host-acceptance.md`).
  Neither the shipped-serve-site-process run nor the two-host run is a checked-in CI/dev gate.
- **Invariants under test.** The thin client links **no** storage/embed/ingest (dependency-cone check);
  version-skew fails loudly; the thin client renders identically to the one-shot CLI.
- **Negative tests.** Thin client vs an old/new server (skew) → clear rejection; server unreachable →
  clear error; `cargo tree` assert (no heavy crates in the client cone).
- **Verification.** Dependency-cone test; thin-client e2e vs the service (automated, in-process); the
  single-host operated capture of the shipped CLIENT binary. The shipped serve-site PROCESS run and the
  two-host operated run are operator runbook steps (field, prerequisite-gated), not automated gates.
- **Risk & rollback.** Packaging; rollback = the local heavy CLI remains usable.
- **Deferrals.** HA / scale-out (out of scope; seams preserved per 02 §10).
- **Done-when.** The serve-site service path renders identically to the one-shot CLI over the full
  operation set, AND protocol-version skew is rejected loudly — both proven by the automated in-process
  E2E. The shipped thin client speaks the versioned protocol with correct contract + connection/URL
  diagnostics (checked-in single-host capture of the real binary; skew stays automated-test evidence).
  The shipped serve-site PROCESS run and the genuine two-host physical run are operator runbook steps
  (executed + OBSERVED blocks filled in the field where the DB/embedder prerequisites exist), not CI/dev
  gates.

---

## Cross-cutting risks (mapped to phases)

- **Wire-enum dependency cycle** (P1): search DTOs need `RetrievalMode`/`GroupBy`/`RetrievalOptions`,
  which live in storage today — move down or convert; do not duplicate or add a back-edge. Caught by the
  P1 dependency-cone assertion.
- **Writer path bound to `ManagedPostgres`** (2B): syncd/`run_catchup`/`apply_*`/activation take
  `&ManagedPostgres` today — a structural signature refactor is required before a standalone PG can be
  populated; it is its own gate, not hidden inside the daemon phase.
- **Readiness must cover incrementals** (3A): incremental apply advances the sequence (part of the
  readiness signature) without calling activation, so the stamp helper must run on incremental commits
  too, or the read path is forced to recompute/write.
- **Storage read APIs are not snapshot-ready** (3B): `execute_read_sql` shells fresh sessions; read
  functions take `&ManagedPostgres`. `QueryStore` requires real storage-API changes, not a wrapper.
- **Read-role grants are absent** (2A): include role/grant DDL + an activation-time grant path + tests.
- **Embedder concurrency/thread-safety** (P4): the service needs a long-lived `Send`/`Sync` embedder or
  a worker/queue + concurrency limits, not the inline per-call construction used today.
- **Renderer scope creep** (P1): first target is byte-parity + response unwrapping, not a rich renderer.
- **Legacy local-session compatibility** (P1): today's local `session`/`serve` parse bare
  `SessionRequest`; version-gate only the *new site* protocol so local agent workflows keep working.
- **Unqualified SQL + schema routing** (3C): fan-out needs schema-qualified SQL or per-arm
  `SET LOCAL search_path` inside one transaction.

## Global deferrals / out of scope

- Client authentication (none, by decision — trusted site LAN).
- HA / multi-host scale-out (seams preserved per the architecture).
- External embedding API (on-site bge-m3 only).
- Producer internals (work/08).
