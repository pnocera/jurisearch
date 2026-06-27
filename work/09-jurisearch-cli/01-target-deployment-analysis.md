# JuriSearch client/server deployment (producer ▸ site server ▸ thin clients) — analysis

Date: 2026-06-27
Scope: **analysis only — no design, no implementation plan.** This document analyses the problem, the
current system as it actually stands in the code, the gap to the envisioned deployment, the forces and
constraints, the decision space with its trade-offs, and the feasibility/risks. The chosen target
shape and its delivery sequencing live separately in
[`02-target-architecture.md`](02-target-architecture.md); this is the reasoning beneath
it, not a restatement of its decisions.

Where the code is cited it is ground truth, read this session. Throughput figures are memory-derived
(directional, not a code claim).

---

## 1. The idea (restated)

Move JuriSearch from a **single self-contained binary** — where every `jurisearch` invocation starts
and owns its own local PostgreSQL and embedding stack — to a **three-tier deployment**:

1. a **producer** (our infrastructure) that ingests legal corpora and emits **signed packages**;
2. a **customer-site server host** that automatically pulls those packages into a **shared PostgreSQL
   database** and answers queries for the whole site;
3. many **thin CLI clients** on user/agent machines that hold no database and no model — they ask the
   site server and render the answer.

The split of concern is: one place per site **holds the corpus, applies updates, and owns the heavy
retrieval+embedding stack**; every other machine is a near-zero-install client.

## 2. Why this shape fits the problem

The motivation is operational, and it is strong:

- **The heavy stack should exist once per site, not once per machine.** Retrieval needs PostgreSQL +
  `pgvector` + `pg_search`, and query embedding needs a bge-m3 model. Replicating that on every
  analyst/agent machine is the central waste; concentrating it on one site host and serving thin
  clients removes it.
- **Confidential legal queries argue for on-site compute.** Query text in this domain can be
  privileged (party names, matter strategy). A deployment where embedding and retrieval happen **inside
  the customer's network** — nothing egressing to an external API — is a strong fit, and it is achievable
  because bge-m3 can run locally.
- **Updates should be applied once per site.** The work/08 producer already ships the corpus as signed
  packages; a single site service that pulls, verifies, and applies them — rather than each machine
  doing so — is the natural consumer of that work.
- **The trust environment is a controlled site network.** Clients are first-party tools on a LAN the
  operator controls; this materially changes what the client↔service boundary has to defend against.

## 3. The current system, as it actually is

The starting point is a **monolithic, self-managing** binary. Two facts dominate the analysis:

- **Every process spins up its own PostgreSQL.** `PgConfig::discover` finds a real `pg_config`
  (`crates/jurisearch-storage/src/runtime.rs:33`) and `ManagedPostgres::start_durable` runs
  `initdb`/`pg_ctl` against a private data dir (`runtime.rs:163`). Both the CLI's `open_index`
  (`crates/jurisearch-cli/src/index_runtime.rs:39`) and the **syncd** consumer
  (`crates/jurisearch-syncd/src/main.rs:91`) go through it. There is **no "attach to an existing
  server"** path — "pgembed" here means a *self-launched local PG*, not an in-process DB.
- **The CLI is the database.** All retrieval logic, embedding (`PreparedQueryEmbedder`,
  `crates/jurisearch-cli/src/embedding_runtime/mod.rs`), and command handling live in the
  `jurisearch-cli` process, which unconditionally depends on `jurisearch-embed`, `jurisearch-ingest`,
  `jurisearch-official-api`, `jurisearch-storage`, and `postgres`
  (`crates/jurisearch-cli/Cargo.toml:16-20`).

There **is** already a service seam: `jurisearch serve` binds a TCP/Unix socket and runs a **JSONL
line protocol** (`SessionRequest`/`SessionResponse` from `jurisearch-core`) through the **session
dispatcher** `dispatch_session_request` (`crates/jurisearch-cli/src/session.rs:121`). That dispatcher
is *distinct* from the one-shot CLI path (`dispatch::run`, `crates/jurisearch-cli/src/dispatch.rs:15-71`),
but its per-command wrappers call the **same payload builders** as the one-shot commands
(`session.rs:33-86`) — so socket results match local runs via the **shared builders**, not a shared
top-level handler. And work/08 already provides the
consumer substrate: per-corpus **physical generations behind stable views**, a `corpus_state` cursor,
atomic activation (`crates/jurisearch-storage/src/generations.rs`), signed packages + trust anchors +
entitlement, and the `jurisearch-syncd` `trust`/`subscribe`/`update`/`status` surface.

So the deployment is **not a rewrite** — but the existing seams were built for a *local, single-user,
trusted* process, and several of their properties are exactly wrong for a *shared, multi-client*
service. That tension is the analysis.

## 4. The gap — what today's code cannot do for this deployment

Each item below is a concrete property of the current code that a site-server/thin-client deployment
would violate or be unable to satisfy.

1. **No shared-server connection mode.** Both consumers self-manage PG (§3). A site host needs one
   persistent PostgreSQL serving *concurrent* readers (the query service) and a *continuous* writer
   (syncd) — which a single-owner embedded instance cannot provide.

2. **`serve` is single-client and sequential.** It handles one request at a time because "the index's
   advisory lock means one request holds the index at a time"
   (`crates/jurisearch-cli/src/serve.rs:72`, and the banners at `:147`/`:185`). A fleet of thin clients
   needs concurrent service.

3. **`serve` is loopback-only and unauthenticated by design.** It refuses a non-loopback bind without
   an explicit `--allow-remote` precisely because "the protocol is unauthenticated"
   (`serve.rs:133-141`). Any multi-machine use has to take a deliberate position on the network/trust
   boundary rather than inherit this one.

4. **The read path performs writes.** The query-readiness gate persists into `index_manifest` via
   `INSERT … ON CONFLICT DO UPDATE` (`store_query_readiness`,
   `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:147-161`; reached on a cache
   miss/stale signature from `load_or_compute_query_readiness`, `:197`), and ingest/embed paths
   *invalidate* it (`:174`). Every shared read entry (`open_query_index`,
   `ensure_query_readiness` in `crates/jurisearch-cli/src/index_runtime.rs`) routes through it. A
   strictly read-only service identity cannot do this — the first query after activation/cold-cache
   would fail, or the role would have to be granted write access.

5. **The session dispatcher is a local surface, not a site API.** `dispatch_session_request` also routes
   `setup`, `doctor`, `stats`, `inspect`, `versions`, `diff` (and model/eval)
   (`crates/jurisearch-cli/src/session.rs:140-145`) alongside the query commands (`:130-136`), and the
   socket honours a **client-supplied `index_dir`** (it only injects a default when the client *omits*
   it — `crates/jurisearch-cli/src/serve.rs:22-32`). Exposed off-host, an authenticated-or-not client
   could steer the server at arbitrary local databases or invoke admin/model operations.

6. **Embedding is per-process and config-driven.** `PreparedQueryEmbedder::from_env` builds the
   embedder per run (`embedding_runtime/mod.rs`); a thin client cannot carry it. Where embedding runs,
   and how the query↔corpus embedding fingerprints are kept compatible, becomes a service concern.

7. **Multi-corpus hot search is a known, unbuilt follow-up.** `execute_read_sql` routes the single-corpus
   case to that corpus's active physical generation, but the **>1-corpus case to the `jurisearch_server`
   UNION views** — explicitly "correct for non-indexed reads … a documented follow-up, **not yet
   reachable since only `core` is installed**" (`crates/jurisearch-storage/src/runtime.rs:286-290`).
   Indexed (BM25/IVFFlat) search must target the *physical* generation schemas (the indexes live there,
   not on the union views — `generations.rs:1-4`). So an "all corpora" endpoint that searched via the
   views would silently lose index acceleration.

8. **The thin client is an extraction, not a flag.** Because `jurisearch-cli` is hard-wired to
   storage/embed/ingest/official-api/postgres (`Cargo.toml:16-20`) and imports those at the crate root,
   a build with "no DB, no model" is genuine restructuring, not a feature toggle.

9. **syncd is one-shot.** `update` is a single plan→verify→apply invocation
   (`crates/jurisearch-syncd/src/main.rs:88`), not a daemon. Keeping a site current automatically needs
   a long-running loop.

## 5. The forces and constraints shaping any solution

These are the analytical "givens" — they don't dictate a single design, but they bound the space.

- **Trusted site network, untrusted producer link.** The boundary that must stay cryptographically
  hard is **producer→site** (already signed/verified in work/08). The **client↔service** boundary sits
  inside a controlled LAN — a materially weaker threat model, which is what makes a no-auth, URL-only
  client even arguable.
- **Data residency is a first-class force.** For legal queries, "query text never leaves the site" is a
  selling property, not a nicety. It pushes embedding **on-site** and rules out an external embedding
  API as the default.
- **Modest concurrency.** A site is a bounded population of analysts/agents, not internet scale. This
  bounds the concurrency substrate question (tens–hundreds of concurrent requests, not tens of
  thousands).
- **A request is two waits, not CPU.** Each dense query is dominated by a **PG-bound** retrieval and a
  **network/IPC-bound** embedding call — both blocking-friendly; neither CPU-bound.
- **The synchronous codebase is a real cost input.** Storage, retrieval, and embedding are all
  *synchronous* today; any "go async for scale" option must price a pervasive rewrite.
- **work/08 already drew the writer/reader line.** Single writer (syncd) applying atomically into
  per-corpus generations behind views, with cursor-gated swaps, is the substrate. work/09 is largely
  *"turn the consumer primitives into a running service + thin clients,"* which means the hard parts
  are read-path and service-boundary properties, not new storage semantics.

## 6. The decision space and its trade-offs

The deployment forces a position on each axis below. This section analyses the **options and what pulls
each way** (and the relevant feasibility finding); it deliberately does not pick — the picks live in the
architecture doc.

- **Storage attachment: self-managed vs shared server.** A shared standalone PG is the only model that
  supports concurrent readers + a continuous writer; the cost is introducing a connect-to-existing
  mode and operating PG as a system service. Self-managed stays the right fit for the producer and for
  tests/dev (zero-setup). *Force:* multi-client concurrency makes this nearly forced.

- **Concurrency substrate: blocking threads vs async.** A bounded worker-thread pool + blocking PG pool
  reuses the synchronous code and fits "two waits, modest concurrency"; async (tokio) scales to far more
  connections but requires rewriting the whole read path. *Trade-off:* simplicity/reuse vs a
  scalability ceiling the site workload is unlikely to hit. Two real knobs regardless: DB-pool size
  (the retrieval ceiling) and embedder concurrency (provider-bound).

- **Transport: raw JSONL vs HTTP/gRPC.** The JSONL line protocol already exists and produces the same
  results as the CLI (shared payload builders) — low friction for a first-party client. HTTP/gRPC buys off-the-shelf observability, load
  balancing, TLS, and non-Rust clients, at the cost of complexity. *Force:* trusted-LAN + first-party
  client favours simplicity; the pull toward HTTP/gRPC only appears with third-party clients or standard
  ops tooling.

- **Embedding placement: on-site local vs external API vs client-side.** On-site (local bge-m3) keeps
  query text in-network and the thin client thin, at the cost of hosting+running the model; an external
  API (e.g. OpenRouter, ~195 vec/s vs ~3.4 on the local APU — directional, memory) is faster but egresses
  query text; client-side embedding refattens the client. *Force:* data residency for legal queries
  dominates, and pushes decisively to on-site.

- **Client auth: none (URL-only) vs per-client credentials.** On a trusted LAN, URL-only keeps clients
  truly thin and removes credential lifecycle; per-client auth (token/mTLS) adds identity/audit/
  revocation but also operational weight and a credential on every machine. *Force:* the trusted-site
  threat model makes URL-only defensible; the narrow API surface + read-only DB identity bound the blast
  radius either way.

- **Multi-corpus exposure: one endpoint vs one service per corpus.** One endpoint exposing all corpora
  is simplest for clients and matches the aggregate readiness signature; but it surfaces the unbuilt
  multi-corpus hot-search routing (§4.7) — indexed search must fan out across the per-corpus physical
  generations and fuse above them, never via the union views. *Feasibility:* the readiness signature
  already aggregates `corpus:generation:sequence` across active corpora, so a single readiness stamp is
  coherent for an all-corpora endpoint; the open cost is the fan-out routing and per-corpus fingerprint
  scoping (`index_manifest` is global today — `generations.rs:891-896`).

- **HA / scale-out: single host vs multi-host.** Single host per site is simplest and matches the
  workload; multi-host scale-out adds availability/throughput at real complexity. *Feasibility:* the
  decomposition (stateless read-only service, URL addressing, PG as system-of-record, single writer)
  keeps scale-out an additive later step rather than a rewrite — so deferring it costs little.

- **Readiness ownership: writer-stamped vs read-time computed.** A strictly read-only service cannot
  compute-and-cache readiness on the read path (§4.4). *Feasibility (codex-validated against source,
  recorded in [`qa/…readiness…`](../../qa/20260627-071703-question-can-query-readiness-move-entire.md)):*
  every readiness input is a writer-owned generation fact, the **only** query-time-varying input is the
  service's own embedder fingerprint (in-memory, compared read-only), and syncd's activation transaction
  already writes `corpus_state` + dense manifest rows atomically — so readiness can move to apply-time.
  The nuance the analysis surfaces: work/08 INV-6 proves a generation is *structurally indexed and
  converged*, **not** that query-readiness *coverage* (projection + dense) is complete — so "always
  fully ready at a client site" depends on an apply-time coverage check, it is not free from INV-6 alone.

## 7. Feasibility — what the codebase makes cheap vs expensive

**Reusable as-is (cheap):**
- the JSONL `SessionRequest`/`SessionResponse` envelope and the **session dispatcher**, whose
  per-command wrappers reuse the **same payload builders** as the one-shot CLI (wire shape + rendering
  already exist);
- per-corpus generations behind stable views + `corpus_state` cursor + **atomic** activation
  (`generations.rs:947`), which already give online, snapshot-coherent swaps;
- the entire work/08 producer→consumer apply path (verify→build→validate→activate in one transaction,
  `crates/jurisearch-syncd/src/apply.rs`).

**Genuinely new work (expensive / structural):**
- a connect-to-existing-PG storage mode + pooling, and least-privilege roles where the read role must
  see *dynamically created* generation schemas at activation time;
- making the read path **write-free** (readiness → writer-owned) and **snapshot-coherent per request**
  (today readiness/probe/retrieval span multiple statements);
- a **site-service dispatcher** that allowlists the query surface and strips client `index_dir`;
- **multi-corpus hot-search fan-out** over physical generations (the explicit follow-up at
  `runtime.rs:290`);
- the **thin-client extraction** away from storage/embed/ingest;
- daemonising syncd.

The shape of the effort: **little of it is storage semantics (work/08 did that); most is read-path
correctness and service-boundary properties.**

## 8. Risks and hard requirements surfaced by the analysis

- **Read-only is load-bearing and currently violated.** Until readiness is writer-owned and the dense
  fingerprint check is a real **fail-closed preflight** (main chunk search lacks the check that zone
  search has — today a mismatch fails *open*: in **hybrid** mode it silently becomes lexical-only, and
  in **explicit dense** mode it silently returns no dense matches / false no-results), a "read-only
  query service" is not actually achievable. This is a hard requirement, not a nicety.
- **Snapshot coherence is asserted, not yet provided.** The current helpers route and retrieve across
  separate statements/connections; concurrent activation can yield stale/failed reads. Any concurrency
  story has to make a request observe one generation for its whole duration.
- **"Online swap" is bounded-lock, not lock-free.** Activation rebuilds the stable views (DDL takes
  locks); hot physical-generation reads stay online but view-backed reads can briefly wait, and a long
  reader can time out an activation — so the writer must treat that as retryable.
- **Coverage ≠ convergence.** A signed package built from incomplete dense coverage would still apply
  and index; "no partial readiness at a client site" requires an apply-time coverage gate (or a
  producer guarantee), not INV-6 alone.
- **Multi-corpus is the least-exercised path.** Only `core` is installed today (`runtime.rs:290`), so
  the all-corpora routing, per-corpus fingerprint scoping, and the global `index_manifest` keying are
  the parts most likely to hold surprises.

## 9. What this analysis deliberately does not do

It does not choose the target shape, specify schemas/APIs, or sequence the work — those are design and
planning concerns captured in [`02-target-architecture.md`](02-target-architecture.md).
It also does not re-open the **work/08** producer/package decisions, which it treats as the established
substrate this deployment consumes.
