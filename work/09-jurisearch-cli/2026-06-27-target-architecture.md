# JuriSearch — target deployment architecture (producer ▸ customer-site server ▸ thin clients)

Status: **target architecture** (the system we are building toward), not a description of what
exists today. It defines the roles, components, protocols, and invariants of the intended
three-tier deployment, and the deltas from the current single-binary model. A dependency-ordered
delivery sequence is sketched in [§11](#11-delivery-sequence-capability-milestones).

Related: this builds directly on the work/08 central-ingest package-distribution system
(`work/08-jurisearch-server/`) — that work delivers the producer side and the signed-package
contract this architecture consumes.

---

## 1. Purpose

Move JuriSearch from a **single self-contained binary** (each `jurisearch` invocation starts and
owns its own local Postgres and embedding stack) to a **three-tier client/server deployment**:

1. a **producer** that ingests legal corpora and emits signed update packages;
2. a **customer-site server host** that automatically pulls those packages into a shared
   PostgreSQL database and answers queries for the whole site;
3. many **thin CLI clients** on user/agent machines that carry no database and no model — they
   ask the site server and render the answer.

The goal is operational: one place per customer site holds the corpus, applies updates, and owns
the heavy retrieval stack; every other machine is a near-zero-install client.

---

## 2. Topology

```
┌──────────────────────────── PRODUCER (central, our infrastructure) ────────────────────────────┐
│                                                                                                 │
│   legal sources ──▶ jurisearch ingest ──▶ producer PostgreSQL ──▶ package-build                 │
│   (LEGI, Judilibre, …)                     (full corpus + outbox)   │                            │
│                                                                     ▼                            │
│                                                     signed baseline / incremental / re-baseline  │
│                                                     packages + signed remote manifest            │
│                                                                     │                            │
└─────────────────────────────────────────────────────────────────── │ ──────────────────────────┘
                                                                       │  publish (filesystem / object store / CDN)
                                                                       ▼
┌──────────────────── CUSTOMER SITE ─────────────────────────────────────────────────────────────┐
│                                                                                                 │
│   ┌──────────────── server host (one machine) ──────────────────────────────┐                  │
│   │                                                                          │                  │
│   │   syncd service ──(verify + apply, online gen swap)──▶  PostgreSQL       │                  │
│   │   (polls manifest, auto-updates)                        server          │                  │
│   │        ▲                                                  ▲              │                  │
│   │        │ pulls packages                                   │ SQL (pool)   │                  │
│   │        │                                jurisearch query service ────────┼──┐               │
│   │        │                                (concurrent, embeds queries,     │  │ JSONL/session │
│   │        │                                 owns retrieval + model access)  │  │ over TCP/UDS  │
│   └────────┼──────────────────────────────────────────────────────────────-─┘  │               │
│            │ network (manifest + package fetch)                                  │               │
│            ▼                                                ┌────────────────────┴─────────────┐ │
│      producer publish endpoint                             │      thin jurisearch CLI         │ │
│                                                            │  (many machines / agents)        │ │
│                                                            │  no Postgres, no model, no corpus│ │
│                                                            └──────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Three trust/network boundaries: producer→site (signed packages over an untrusted channel),
site-host-internal (syncd↔PG↔query-service, all on one host), and site-LAN (thin clients↔query
service).

---

## 3. Roles and responsibilities

| Role | Process | Owns | Talks to |
|---|---|---|---|
| **Producer** | `jurisearch ingest` + `jurisearch-package` | the canonical corpus, the outbox, package signing | legal sources; a publish endpoint |
| **Site PostgreSQL** | `postgres` (system service) | the customer-site corpus (generations + views), trust anchors, license, app/control schema | local syncd + query-service connections only |
| **syncd service** | `jurisearch-syncd` (daemon) | applying verified updates, online generation swap, the client cursor | the producer publish endpoint; the site PG |
| **Query service** | `jurisearch serve` (daemon) | hybrid retrieval, query-embedding orchestration, response shaping | the site PG (read pool); the local bge-m3 service; thin clients |
| **Embeddings service** | local `llama.cpp` bge-m3 on the server host | computing query embeddings on-site (no egress) | the query service (localhost only) |
| **Thin client** | `jurisearch` (thin build) | nothing persistent — just the configured service URL; formats a request, renders a response | the query service only (by URL, no auth) |

A clean separation falls out: **syncd is the only writer** of corpus data; **the query service is
read-only**; **thin clients touch neither PG nor models.**

---

## 4. Starting point (what we build on)

The target is reachable by recomposing pieces that already exist, not a rewrite:

- A **JSONL session protocol** and a transport-neutral handler (`dispatch_session_request`) already
  back both the one-shot CLI and a socket daemon (`jurisearch serve`, TCP/Unix).
- The **work/08** machinery already gives signed packages, per-corpus **generations behind stable
  views**, a `corpus_state` cursor, online cursor-gated apply, and the `jurisearch-syncd`
  consumer (`trust` / `subscribe` / `update` / `status`).
- **Query embedding** is already a discrete step at search time (`PreparedQueryEmbedder`).

The deltas (§5–§8) are architectural, not just repackaging: connect to a **shared external PG**
instead of a private managed one; turn the query service into a **concurrent, strictly read-only,
snapshot-consistent** service behind a **narrow query-only API**; turn syncd into an **always-on
daemon** that owns corpus state (including query-readiness); and ship a **structurally separate thin
client**. §6 reworks the read path's contract, not only the transport.

---

## 5. Decouple from embedded Postgres → shared server

**Target.** Both the query service and syncd connect to a **standalone PostgreSQL server** on the
site host via connection parameters (host/socket, port, db, role, password/sslmode), not by each
process running `initdb`/`pg_ctl` on a private data dir.

- Introduce a storage connection mode that **attaches to an existing server** (a libpq-style
  connection config / URL) alongside today's self-managed mode. The query service uses a **read
  pool**; syncd uses a small **writer pool**.
- The site PG is a **persistent system service** (its own data dir, WAL, backups, `pg_search` +
  `vector` preloaded) — its lifecycle is independent of any JuriSearch process. JuriSearch
  processes become **clients** of it.
- Self-managed `ManagedPostgres` remains the mode for the **producer** and for **tests/dev** (a
  one-binary, zero-setup experience). The customer-site server host runs the shared-server mode.

**Why.** Multiple concurrent readers plus a continuous writer cannot share a single-owner embedded
instance; a shared server is the natural system-of-record and the thing an operator already knows
how to back up, monitor, and secure.

---

## 6. The query service (concurrent, multi-client)

**Target.** `jurisearch serve` becomes a **concurrent, strictly read-only** query service. Its
defining architectural properties:

- **Concurrent.** It serves many thin clients in parallel, pooling connections to the shared PG.
  Reads scale because they target the corpus's **active generation**; the writer (syncd) does not
  block the hot read path.

- **Hot search fans out over physical generations, not union views.** BM25/IVFFlat indexes live in
  each corpus's **physical generation schema** (the stable `jurisearch_server` union views are for
  non-indexed reads / status / readiness, not hot search). So an all-corpora search **resolves the
  active corpus set once per request and runs hot retrieval against each active physical generation
  schema under the one request snapshot, then fuses/paginates above those per-corpus arms** — keeping
  index acceleration. Union views are never the hot-search path. The embedding-compatibility preflight
  is correspondingly **operation-scoped**: every corpus a dense/hybrid request touches must match the
  server's bge-m3 fingerprint, or the request **fails closed**.

- **Strictly read-only — and therefore state is the writer's job.** The service performs no writes:
  it holds a read-only database identity and cannot mutate corpus, cursor, index, or trust state.
  A direct architectural consequence: any *query-readiness / index-state* bookkeeping the read path
  needs is **owned and produced by the writer (syncd) at apply time**, never as a side effect of
  answering a query. Concretely (codex-validated against the source): syncd **stamps a query-readiness
  record inside its activation transaction**, and the query path is a **read-only lookup** of that
  stamp — a missing or stale stamp is an **apply/writer failure**, never a query-time recompute. The
  only query-time-varying input, the service's own embedder fingerprint, is in-memory and compared
  read-only. **An active generation is always fully ready** — work/09 *extends* work/08's INV-6 (which
  proves the generation is structurally indexed and converged to the signed package, but **not**
  query-readiness *coverage*) with an **apply-time readiness validation**: syncd stamps readiness only
  after **projection and dense coverage are complete** for the active topology, and incomplete coverage
  is an **apply failure with the cursor unchanged** (complementarily, the producer should only publish
  dense-ready packages). So there is no partial-readiness case at a client site — it is enforced at
  apply, not assumed. Serving a query never requires a write.

- **Snapshot-consistent per request.** Each request observes a **single, consistent generation for
  its entire duration**. An online generation swap (§7) is therefore never observed mid-request — a
  client sees a wholly-old or wholly-new corpus, never a mixture — and reads in flight when a
  generation is retired are not pulled out from under it.

- **Embeds queries on-site, and refuses incompatible queries.** Query embedding is computed **on the
  site server host by a local `llama.cpp` bge-m3 embeddings service** co-located with Postgres — so
  **query text never leaves the site network** (the decisive property for confidential legal queries;
  there is no external embedding API at the customer site). The query service calls this local
  endpoint to embed each query; thin clients never embed. It treats embedding compatibility with the
  **active generation** as a precondition: if the local embedder does not match what the active
  generation was built for, it returns a **clear compatibility error rather than silently degrading**
  the result. The active and server embedding fingerprints are visible in health/status. (The site's
  bge-m3 must match the producer's bge-m3 that built the corpus — that fingerprint coupling is what
  the preflight checks.)

- **Exposes only a narrow query API — not the local command surface.** The client-facing API is a
  deliberately **scoped query contract** (search / fetch / cite / related / context / compare /
  status). Administrative, model-management, ingestion, and evaluation operations are **not** part of
  the client API; they live on a **separate local management surface**. The service **owns its own
  data-source binding** — a client cannot direct it at an arbitrary database or index location. Even
  on a trusted LAN this keeps the service's authority minimal and its blast radius small (a client
  can only run queries, never admin/model/ingest operations or point the server elsewhere).

**Network & exposure.** Thin clients do **not** authenticate. A client needs only the **service URL**
(host:port, or a local socket path) of its site query service, configured once. The trust boundary is
the **site network perimeter**, not per-client credentials: the service binds to the trusted site
network and answers any client that can reach it. This matches the deployment assumption that the site
network is trusted; the *untrusted* boundary — producer→site — stays signed and verified per work/08.
Exposing the service beyond the trusted site network is an operator/perimeter concern and out of scope
here. Request-size and idle limits remain (resource hygiene, not auth), and the narrow API surface +
read-only data identity above keep the blast radius small even without client auth.

---

## 7. The syncd service (automatic updates)

**Target.** `jurisearch-syncd` gains a long-running **`run` (daemon) mode** that keeps the site
corpus current without operator action.

- **Owns the multi-corpus set.** syncd manages **all corpora** the site subscribes to — each with its
  own per-corpus generation and cursor (work/08) — and the single query-service endpoint exposes **all
  of them** to every thin client. Adding/removing/updating a corpus is an operator action on syncd,
  transparent to clients.
- **Poll loop.** On a configurable interval, for each subscribed corpus fetch the producer's **signed
  remote manifest**, run the work/08 **catch-up planner** (§9.4 size-driven baseline-vs-incremental
  decision) against the local cursor, fetch the chosen artifacts, **verify** (signature + digest +
  schema/embedding/builder compat + entitlement), and **apply** in a cursor-gated transaction.
- **Online, atomic swap.** Apply builds the new generation's indexes **before** activation and
  repoints the views + advances the cursor in one short switch transaction — so the query service
  keeps answering against the old generation throughout, then atomically sees the new one. No
  downtime, no torn reads (this is the work/08 INV-6 / §7.4 contract).
- **Stamps readiness in the swap.** The same activation transaction writes the **writer-owned
  query-readiness record** for the new generation (it already writes `corpus_state` + dense manifest
  rows atomically) — but **only after validating that projection + dense coverage are complete** for
  the active topology; incomplete coverage **fails the apply (cursor unchanged)**. So the read-only
  query service only ever **reads** a "ready" stamp (§6).
- **Warn-and-reject, never partial.** Any failed precondition leaves the cursor untouched and emits
  a closed-vocabulary reject code (work/08 §6.3); the daemon logs it and retries on the next tick.
- **Writer isolation.** syncd is the **only** writer to corpus + cursor + trust tables, serialized
  by the existing apply advisory lock — so it never races itself or a second instance.
- **Bootstrap unchanged.** `trust install-anchor`, `subscribe --token-json`, and a first
  `update`/baseline remain the one-time provisioning steps before the daemon takes over.

---

## 8. The thin client

**Target.** A **thin build** of the `jurisearch` CLI that links **no storage, no embedding, no
model assets** — only the request/response types and a client transport.

- **Behaviour.** It is configured once with the **service URL** (`--server host:port` or a Unix
  socket path, defaulted from config/env), sends a `SessionRequest` (`search` / `fetch` / `cite` /
  `related` / `context` / `status` …), and renders the `SessionResponse` exactly as the one-shot CLI
  renders it today (same human and `--json` output). No credentials — just the URL.
- **Zero corpus footprint.** No `index_dir`, no Postgres, no model key on the client. Install is a
  single small binary; the heavy dependency surface stays on the server host.
- **A structural separation, not a build flag.** The thin client is a **distinct artifact** that
  depends only on the shared request/response contract plus transport and rendering — with no link to
  the storage, embedding, or ingestion stacks. Today's CLI is tightly coupled to all three, so
  reaching this is genuine restructuring (separating the client-facing contract and rendering from the
  server stack), not a toggle. The heavy binary remains the local/server/admin tool.
- **Graceful degradation.** Clear errors when the server is unreachable or a protocol-version
  mismatch occurs; an optional `--local` escape hatch (dev only) can fall back to the self-contained
  path.

A **protocol version** field on the session envelope lets server and client evolve independently and
reject incompatible peers with a precise message. Thin-client upgrades are **operator-managed —
auto-update is out of scope** — so version negotiation must fail loudly on skew, never silently
degrade.

---

## 9. Data and control flows

**Query (thin client → answer).**
1. Thin client sends `{proto, op: "search", args:{query, mode, k, …}}` to the service URL over
   TCP/UDS (no credentials).
2. Query service checks out a pooled read connection.
3. Service embeds the query via the local bge-m3 service (if dense/hybrid), resolves the active
   generation, runs hybrid retrieval (BM25 + dense + RRF) inside one snapshot, shapes the response.
4. Service returns `SessionResponse`; client renders it. No client-side corpus access at any point.

**Automatic update (producer → site).**
1. syncd daemon ticks, pulls the signed remote manifest, plans catch-up vs the local cursor.
2. Fetches + verifies the chosen baseline/incremental artifacts (all preconditions).
3. Applies into a new/active generation, builds indexes, swaps views + cursor atomically.
4. Query service — already connected — transparently begins resolving the new generation on its
   next request; in-flight queries finish against their snapshot.

---

## 10. Cross-cutting concerns

- **Consistency model.** Single writer (syncd) + many readers (query service) + generations behind
  stable views + per-request snapshot reads (§6) ⇒ readers always see a coherent generation, and
  swaps are atomic and online. This is the central invariant the whole topology rests on.
- **Online but bounded-lock swap.** A generation swap is online, not lock-free: repointing the stable
  views takes brief, bounded locks. Hot reads against the active generation stay available; reads that
  go through the stable views may briefly wait behind a swap; and the writer treats a contended or
  timed-out activation as **retryable**. The right mental model is a "bounded, retryable lock", not
  "never blocks readers".
- **Security.** Producer→site: signed packages + installed trust anchors + license entitlement
  (work/08). Site host: least-privilege database identities — a **read-only** identity for the query
  service, a **writer** identity for syncd, with the read-only identity gaining visibility of each
  newly activated generation automatically. Site LAN: **trusted perimeter** — thin clients reach the
  service by **URL, without credentials** (§6); the site network boundary is the trust boundary, and
  exposing the service beyond it is an operator concern, out of scope. **Query text and embedding
  compute stay on the site network** — embeddings come from the local bge-m3 service, nothing is sent
  to any external provider.
- **Observability.** `status --json` on the site host (cursor, compat stamps, last applied digest,
  applied-at) for the corpus; the query service exposes health + active-generation + embedding
  fingerprint; syncd logs each tick's plan/verify/apply outcome.
- **Deployment.** **systemd** units on the site host — `jurisearch-syncd.service`,
  `jurisearch-serve.service`, and the local `bge-m3` embeddings service — with the query service
  depending on Postgres + the embeddings service, and syncd depending on Postgres; a config file/env
  for PG connection, producer endpoint + poll interval, and the local embeddings-service endpoint (no
  external credentials). Thin clients ship a single binary + a one-line service-URL config.
- **Failure modes.** PG down → both services fail health, queries error cleanly, syncd retries.
  Producer unreachable → site keeps serving the current generation; updates resume when it returns.
  Bad/unauthorized package → rejected, cursor untouched, current generation still served.
- **Designed-in extension seams (single-host now; scale-out a later addition, not a rewrite).** The
  decomposition keeps a future move to multi-host HA cheap, without committing to it now:
  - the **query service is stateless + read-only**, so it can be replicated as N instances against
    the same PG with nothing to coordinate;
  - **clients address the service by URL**, so a load balancer / VIP can later front multiple
    instances with no client change;
  - **PG is the shared system-of-record**, so read-replicas can later absorb read load (the read-only
    query service can target a replica);
  - the **embeddings service is a separate endpoint**, so it can move to its own host or be pooled;
  - **syncd stays the single writer** per site regardless — the canonical HA shape is "scale the
    stateless readers, keep one writer".
  The work/08 generations-behind-views + per-request snapshot reads are already safe for many
  concurrent readers, including across hosts.

---

## 11. Delivery sequence (capability milestones)

A dependency-ordered sketch of the capabilities to stand up — not an implementation plan. Each is a
coherent milestone and (per project discipline) codex-reviewed before the next.

1. **Shared-server storage** — the query service and syncd connect to a standalone PG as clients,
   with least-privilege read-only and writer identities; self-managed mode stays for producer/tests.
   (Foundational; everything else depends on it.) **Acceptance invariant:** because generation schemas
   are created dynamically, every activation must leave the read-only identity able to read
   `corpus_state`, `index_manifest`, the stable views, and the **newly created generation schemas** at
   commit time — an active condition to verify, not passive config.
2. **Read-only-safe, snapshot-consistent read path** *(prerequisite for the query service)* —
   query-readiness becomes writer-owned (produced at apply time), and each request reads one
   consistent generation snapshot with the embedding-compatibility precondition enforced. This is the
   contract §6 rests on.
3. **Always-on syncd** — the daemon that auto-pulls, verifies, and applies updates with online,
   retryable generation swaps and writer-owned readiness.
4. **Concurrent query service** — the read-only, snapshot-consistent service over the shared PG,
   owning embedding and exposing only the narrow query API.
5. **LAN exposure** — the service binds to the trusted site network, reachable by **service URL with
   no client auth**, with client/server protocol-version negotiation.
6. **Thin client** — the structurally separate client artifact, rendering identically to the
   one-shot CLI.
7. **Operationalization** — configuration, service units, health/observability, site-PG
   backup/restore, and an end-to-end two-host acceptance run (producer → site server → thin client).

---

## 12. Open decisions (to resolve before building)

- **Transport framing:** *recommended* — **keep the raw JSONL line protocol** (newline-delimited
  JSON request/response, already implemented and byte-identical to the one-shot CLI). It is the
  low-friction fit for a thin first-party CLI on a trusted LAN with no auth. **Revisit only if** a
  concrete need appears: non-Rust / third-party clients, or off-the-shelf observability / load
  balancing / TLS termination via standard HTTP or gRPC tooling. Until then, the simplicity wins.
- **Concurrency substrate:** *recommended* — a **bounded worker-thread pool + a blocking PG
  connection pool** (substrate A), reusing the existing synchronous storage/retrieval/embedding code.
  A request is dominated by two independent waits (PG-bound retrieval, network-bound embedding), and a
  site's concurrency is modest, so blocking threads parked on those waits are fine. Two tuning knobs:
  the **DB pool size** (the retrieval ceiling; also bounds open read-snapshots during a swap) and the
  **embedder concurrency** (governed by the provider's throughput, not the DB). **Revisit only if** a
  deployment needs very high concurrent connection counts — only then does an async (tokio) rewrite of
  the whole read path pay for itself.
- **Embedding placement:** *decided* — computed **on the site server host by a local `llama.cpp`
  bge-m3 service**; query text never leaves the site network, the thin client stays thin, and cost is
  a non-issue (no external API). The §6 fingerprint preflight guards correctness (the site's bge-m3
  must match the producer's). Client-side / air-gapped variants are moot — the on-site local model
  already keeps everything local.
- **Readiness ownership:** *decided (codex-validated against source) — FEASIBLE.* Readiness is
  writer-stamped inside syncd's activation transaction and read-only at query time; a missing/stale
  stamp is an apply/writer failure, not a recompute. Active generations are always fully ready because
  the stamp is **gated on apply-time projection + dense coverage validation** — work/09 extends INV-6,
  and incomplete coverage is an apply failure (§6, §7), not merely assumed. The stamp's keying (today
  `index_manifest` is global) is resolved together with the multi-corpus decision below. Net-new
  implementation work this implies: the main chunk-search **fingerprint preflight** (today it degrades
  silently rather than erroring; zone search already has the pattern) and the **apply-time coverage
  gate** on the stamp.
- **Thin-client/server skew:** *decided* — thin-client **auto-update is not in scope**; client
  upgrades are operator-managed / out-of-band. The wire envelope carries a **protocol version**, and
  the service **negotiates and rejects incompatible peers with a clear, actionable message** (§8), so
  field skew fails loudly rather than degrading silently.
- **Multi-corpus exposure:** *decided* — **one query-service endpoint exposes all corpora**, and every
  thin client can query any/all of them. The **corpus set is managed by syncd** (per-corpus generations
  + cursors already exist in work/08); adding/removing/updating a corpus is an operator action on
  syncd, transparent to clients. This also settles the readiness keying: the writer-stamped readiness
  signature is an **aggregate over all active corpora**, so a single stamp correctly represents "all
  active corpora ready" and is re-stamped whenever any corpus activates a new generation. The
  fingerprint preflight validates the queried corpus's active generation against the site's single
  bge-m3 (uniform in practice; per-corpus dense isolation stays deferred until corpora ever use
  different embedders). Hot search **fans out over the per-corpus physical generations and fuses above
  them** (never via the union views), preserving index acceleration — see §6.
- **HA / scale-out:** *decided* — **single server host per site** (one PG + syncd + query service +
  bge-m3); multi-host HA / scale-out is **out of scope for now**. The architecture is nonetheless
  deliberately **conceived for easy future extension** — the enabling seams are listed in §10
  (*Designed-in extension seams*), so scale-out is a later addition rather than a rewrite.

---

## 13. Relationship to work/08

work/08 delivers the **producer** and the **package contract** end of this picture (ingest →
signed packages; the consumer apply primitives; generations/cursor/trust). work/09 is the
**customer-site server + thin-client** end: turning the consumer primitives into a **running site
service** (auto-updating syncd daemon + concurrent query service over a shared PG) and the
**fleet of thin clients** that consume it. Together they are the full producer→site→client chain.
