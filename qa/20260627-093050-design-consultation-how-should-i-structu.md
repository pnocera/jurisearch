# Design consultation: structuring the work/09 implementation plan

## Short verdict

Your style proposal is right: compact phase templates, explicit deferrals, risk/rollback, and codex-reviewed/committed gates fit this work better than another work/08 invariant matrix. But I would change the sequencing and split two phases.

The two biggest issues:

1. **Phase 1 is not a pure refactor.** The proposed `contract`/`transport`/`render` extraction crosses current crate boundaries that are not clean today. The CLI arg/session types live in `jurisearch-cli`; retrieval wire concepts like `RetrievalMode`, `GroupBy`, and `RetrievalOptions` live in `jurisearch-storage`; and the current socket/session dispatcher calls side-effecting CLI payloads directly (`crates/jurisearch-cli/src/session.rs:33-86`, `crates/jurisearch-cli/src/session.rs:121-159`). Treating this as a simple base-crate move underestimates it.
2. **Phase 3 is too large.** Readiness ownership, snapshot routing, fingerprint preflight, response-builder extraction, and multi-corpus physical-generation fan-out are separate failure modes. They should not be one GO gate.

My recommended shape: keep the plan concise, but add a small "surface inventory + walking skeleton" concept and split the read path into smaller risk gates.

## What I would change in the phase sequence

I would structure the implementation plan around this critical path:

1. **Surface inventory and dependency-light base extraction**
   Goal: define the exact query API, contract types, protocol envelope, codec, and minimal render authority while preserving today's local CLI/session behavior.

   This phase should explicitly decide what happens to commands currently exposed over session but not in the site API: `expand`, `model fetch`, `eval phase1`, `setup`, `doctor`, `stats`, `inspect`, `versions`, and `diff` are all currently in `dispatch_session_request` (`crates/jurisearch-cli/src/session.rs:127-145`), while the design's query `Operation` set is narrower (`work/09-jurisearch-cli/03-deployment-design.md:91-100`, `work/09-jurisearch-cli/03-deployment-design.md:261-264`). The implementation plan must include a compatibility table: kept in local session, exposed in site service, moved to management, or intentionally dropped.

   Also call out the hidden enum move: `GroupBy`, `RetrievalOptions`, and `RetrievalMode` are in `jurisearch-storage` today (`crates/jurisearch-storage/src/retrieval/types.rs:42-70`, `crates/jurisearch-storage/src/retrieval/types.rs:210-233`). A dependency-light contract crate cannot depend on storage, so either those wire-level enums move down to contract or you introduce explicit contract-to-storage conversion types. Do not let this become accidental duplication.

2. **Shared-server storage + real role proof**
   Goal: attach to an existing PostgreSQL server with separate read and writer identities, while keeping self-managed `ManagedPostgres` green for local/dev.

   This is foundational as the architecture says (`work/09-jurisearch-cli/02-target-architecture.md:106-123`, `work/09-jurisearch-cli/02-target-architecture.md:323-328`). The current runtime only starts temp/durable managed PG through `initdb`/`pg_ctl` and returns connection strings for that instance (`crates/jurisearch-storage/src/runtime.rs:99-160`, `crates/jurisearch-storage/src/runtime.rs:163-230`). There is no attach mode, no read pool, and no writer pool.

   The important acceptance test is not "can connect." It is: after activation creates a new generation schema and repoints views, the read-only identity can read `corpus_state`, `index_manifest`, stable views, and the new physical generation schemas. The design correctly calls this out (`work/09-jurisearch-cli/03-deployment-design.md:170-179`), but the current code has no `GRANT`/role machinery; "read role" is currently a logical search-path convention, not a database identity (`crates/jurisearch-storage/src/runtime.rs:283-308`).

3. **Writer-owned readiness and fingerprint preflight, single-corpus first**
   Goal: remove the query-time readiness write and make main chunk search fail closed on fingerprint mismatch.

   This should be its own phase. Current readiness writes `index_manifest['query_readiness']` on a fully-ready cache miss (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:192-242`), and the CLI gate calls that through `ensure_query_readiness` (`crates/jurisearch-cli/src/index_runtime.rs:97-137`). The architecture now requires syncd to stamp readiness in activation and make missing/stale stamps a writer failure (`work/09-jurisearch-cli/02-target-architecture.md:145-159`, `work/09-jurisearch-cli/02-target-architecture.md:213-217`, `work/09-jurisearch-cli/02-target-architecture.md:366-374`).

   Keep this phase focused: apply-time coverage validation + activation-time stamp + read-only lookup + main chunk-search fingerprint preflight. Do not include multi-corpus fan-out yet. Zone search already shows the fingerprint-preflight pattern (`crates/jurisearch-cli/src/retrieval/zone.rs:5-49`, `crates/jurisearch-cli/src/retrieval/zone.rs:91-97`); main search currently only filters dense rows by the query fingerprint, which can silently degrade hybrid or return false no-results in dense mode (`crates/jurisearch-cli/src/retrieval/search.rs:303-319`, `crates/jurisearch-storage/src/retrieval/sql.rs:94-124`, `crates/jurisearch-storage/src/retrieval/sql.rs:205-224`).

4. **Snapshot-bound `QueryStore` and side-effect-free response builders, single-corpus**
   Goal: one request owns one read transaction/snapshot, and current CLI payloads become adapters over side-effect-free builders.

   This is where I would put the builder extraction, not phase 1. The design is explicit that today's payloads are not reusable service handlers because they resolve `index_dir`, start managed PG, and run readiness (`work/09-jurisearch-cli/03-deployment-design.md:276-284`). The source confirms it: `search_payload` validates args, resolves `index_dir`, starts local PG, and calls `search_with_postgres` (`crates/jurisearch-cli/src/retrieval/search.rs:38-102`); `open_query_index` starts managed PG and gates readiness (`crates/jurisearch-cli/src/index_runtime.rs:39-55`).

   Also plan for a storage signature refactor. Current retrieval functions are mostly `fn(..., postgres: &ManagedPostgres, ...)` and call `execute_read_sql` internally; examples include `fetch_documents_json` (`crates/jurisearch-storage/src/retrieval/fetch.rs:5-22`), `context_documents_json` (`crates/jurisearch-storage/src/retrieval/context.rs:5-25`), `related_neighbours_json` (`crates/jurisearch-storage/src/retrieval/related.rs:11-20`), and `hybrid_candidates_json` (`crates/jurisearch-storage/src/retrieval/hybrid.rs:5-23`). A true `ReadSnapshot` cannot be a wrapper around `ManagedPostgres::execute_read_sql`, because that helper opens fresh `psql` sessions and resolves search path per call (`crates/jurisearch-storage/src/runtime.rs:237-247`, `crates/jurisearch-storage/src/runtime.rs:283-308`).

5. **Multi-corpus physical-generation search fan-out**
   Goal: implement the hard part the architecture added: all-corpus hot search over physical generation schemas, with per-corpus compatibility and a fuse/pagination layer.

   This deserves its own phase. The architecture requires physical-generation fan-out, not union-view search (`work/09-jurisearch-cli/02-target-architecture.md:136-143`, `work/09-jurisearch-cli/03-deployment-design.md:306-322`). The current runtime explicitly sends the multi-corpus case to `jurisearch_server` union views and labels physical fan-out as a follow-up (`crates/jurisearch-storage/src/runtime.rs:283-308`). The generation module also says hot indexed retrieval must hit qualified physical schemas (`crates/jurisearch-storage/src/generations.rs:10-15`).

   This phase needs its own pagination and ranking acceptance tests. Cross-corpus RRF/fusion, cursor shape, authority rerank, decision filters, and `--zone` behavior can regress in subtle ways.

6. **Query service walking skeleton, then concurrent service**
   Goal: introduce `jurisearch-query` and the site dispatcher without waiting for LAN exposure or the thin client.

   I recommend inserting an early tracer/walking skeleton, but not as a substitute for the hard read-path work. Put it after the minimal contract/transport base and after enough storage/readiness work to run one operation honestly. A good first operation is `fetch` or `status`; avoid dense search as the first tracer. The skeleton should use the real JSONL codec, protocol envelope, allowlist, server-owned context, read-only DB identity, and one worker/pool path. It can be UDS/loopback only.

   What it buys: it validates crate boundaries, transport framing, server-owned binding, read-only role wiring, and request/response parity before the full search fan-out is complete.

   What it costs: some scaffolding may be thrown away. Keep it deliberately narrow and test-driven so it does not become a parallel dispatcher.

   Then extend to the full service: bounded worker threads + blocking PG pool, long-lived local bge-m3 embedder, size/idle limits, health/status. Do not postpone health entirely to the final ops phase; the service needs health early to prove active generation, readiness stamp, local embedder fingerprint, and pool status.

7. **syncd daemon policy loop**
   Goal: daemonize around the existing apply/catch-up machinery, not rewrite apply.

   Split "writer-owned activation/readiness" from "long-running daemon." The former belongs in phase 3; the daemon phase should mostly compose `PackageSource`, verifier, cursor planner, retry/backoff, logging, shutdown, and configuration. The existing one-shot path is already a strong substrate: `run_catchup` plans/fetches/verifies/applies (`crates/jurisearch-syncd/src/planner.rs:1-10`, `crates/jurisearch-syncd/src/planner.rs:452-480`), and apply already does verify, load, index-build, contract validation, postcondition validation, and activation (`crates/jurisearch-syncd/src/apply.rs:108-165`, `crates/jurisearch-syncd/src/apply.rs:219-264`). The current binary, however, is still a one-shot CLI over a managed index dir (`crates/jurisearch-syncd/src/main.rs:21-69`, `crates/jurisearch-syncd/src/main.rs:88-180`).

8. **Thin client, LAN exposure, and ops**
   Goal: ship the structurally separate thin artifact, URL addressing, protocol skew rejection, LAN bind, systemd units, bge-m3 unit/config, and the two-host acceptance run.

   The thin-client binary can be late. The base contract/transport/render crates must exist earlier, but the separate user-facing artifact does not need to lead the implementation. This matches the architecture's delivery sequence, which places thin client after the service/LAN work (`work/09-jurisearch-cli/02-target-architecture.md:333-342`).

If you want to keep exactly seven phases, combine phases 6 and 8 only at the document level, but keep their deliverables separately reviewable.

## Answers to your open questions

### 1. Bottom-up layering vs early tracer

Use both, but do not put the tracer first.

Start with a small amount of bottom-up extraction because the current session wire shape exists in `jurisearch-core` (`crates/jurisearch-core/src/session.rs:6-48`), but the planned protocol envelope, operation allowlist, and DTO ownership do not. Then insert a walking skeleton before full search/fan-out.

The tracer should prove:

- one framed request/response over the new codec;
- protocol-version rejection;
- server-owned context, no client `index_dir`;
- query allowlist rejects admin/model/eval;
- real read-only DB identity can perform one safe operation;
- result rendering/parity is controlled by the new render authority.

It should not stub readiness if the point is to prove read-only safety. If you need an even earlier tracer, make it explicitly fake-store-only and label it a crate-boundary smoke test, not a capability milestone.

### 2. Single riskiest item

The riskiest item is **the read path refactor: one snapshot + writer-owned readiness + physical-generation routing while preserving existing query semantics**.

Shared-server attachment is foundational, but mostly mechanical. The daemon loop is operational, but the apply substrate exists. The read path touches correctness, security, performance, and compatibility all at once:

- current retrieval functions are tied to `ManagedPostgres` and unqualified SQL;
- current readiness writes on the read path;
- current main dense/hybrid search lacks a fail-closed fingerprint preflight;
- current multi-corpus hot search is explicitly not implemented;
- current session/server path exposes more than the future site API.

Your sequencing puts this early enough, but phase 3 must be split.

### 3. Is "keep the monolith green by extracting shared authorities" realistic?

Yes, but only if you make it smaller and more staged.

The monolith can stay green because the one-shot dispatcher already centralizes command routing (`crates/jurisearch-cli/src/dispatch.rs:15-71`) and the session path already shares payload builders (`crates/jurisearch-cli/src/session.rs:33-86`). But the current `jurisearch-cli` crate is heavily coupled: it imports embed, ingest, official API, storage, Postgres, and many storage modules directly at the crate root (`crates/jurisearch-cli/src/main.rs:18-107`), and its Cargo dependencies include all heavy stacks (`crates/jurisearch-cli/Cargo.toml:11-21`).

So phase 1 should not promise to extract every DTO, transport, render, and builder in one GO. Break it into:

- contract surface inventory and golden tests;
- move or wrap the existing session envelope/errors;
- move wire enums/conversions out of storage/CLI;
- extract codec;
- extract render bytes;
- only then extract response builders.

### 4. Is any phase too large?

Yes: phase 3, and probably phase 1.

Split phase 3 into:

- **3A readiness/preflight:** writer-owned readiness stamp, read-only lookup, apply-time coverage gate, main chunk-search fingerprint preflight.
- **3B snapshot/query store:** single request transaction, active resolver in snapshot, storage read functions accept snapshot/client instead of `ManagedPostgres`.
- **3C multi-corpus fan-out:** physical schema search arms, fuse, pagination, per-corpus compatibility.

Split phase 1 into "base contract/codec/render" and "response-builder extraction" unless you are comfortable with a very large refactor phase.

### 5. Minimal valuable first deliverable

The minimal valuable first deliverable is:

**A dependency-light, versioned JSONL contract/codec plus a one-operation query-service walking skeleton over a real read-only DB identity.**

If that is too much for the first phase, the minimal first code deliverable is narrower:

**A contract/transport extraction that preserves existing `session --jsonl`, `batch --jsonl`, and `serve` byte behavior, while adding the new protocol envelope behind tests.**

Do not make the thin client the first deliverable. It proves less than a server skeleton because the hardest boundary is server-owned context + read-only store, not client argument parsing.

### 6. What will this trip over?

The main things I think you are not giving enough weight:

- **Contract DTO dependency cycles.** Search wire DTOs need retrieval mode/grouping/options, but those live in storage today (`crates/jurisearch-storage/src/retrieval/types.rs:42-70`, `crates/jurisearch-storage/src/retrieval/types.rs:210-233`). Plan the move/conversion explicitly.
- **Legacy local session compatibility.** The design says unversioned site frames are rejected (`work/09-jurisearch-cli/03-deployment-design.md:116-121`), but today's local `session` and `serve` parse bare `SessionRequest` (`crates/jurisearch-cli/src/session.rs:88-118`, `crates/jurisearch-cli/src/serve.rs:99-103`). Preserve local agent workflows or deliberately version-gate only the new site protocol.
- **Site API allowlist vs current session surface.** Current `dispatch_session_request` is much broader than the target service (`crates/jurisearch-cli/src/session.rs:127-145`). The plan needs an operation-by-operation compatibility matrix.
- **Renderer scope creep.** Today output is mostly JSON emission: pretty `Value` for one-shot commands and compact session envelopes (`crates/jurisearch-cli/src/output.rs:55-77`). Do not invent a rich renderer while trying to split crates. First renderer target should be byte-parity and response unwrapping.
- **Storage read APIs are not snapshot-ready.** `execute_read_sql` shells through fresh sessions/search paths, and read functions take `ManagedPostgres` (`crates/jurisearch-storage/src/runtime.rs:237-247`, `crates/jurisearch-storage/src/runtime.rs:283-308`, `crates/jurisearch-storage/src/retrieval/fetch.rs:5-22`). QueryStore requires deeper storage API changes than an adapter.
- **Unqualified SQL and schema routing.** Multi-corpus fan-out needs either schema-qualified SQL generation or carefully scoped `SET LOCAL search_path` per arm inside one transaction. Today most SQL assumes unqualified `documents`, `chunks`, etc.
- **Read-role grants are not present.** The plan should include role/grant DDL and tests. Current generation schemas/views exist (`crates/jurisearch-storage/src/migrations.rs:903-958`), but there is no activation-time grant path.
- **Embedder concurrency/thread-safety.** Current search builds/probes a `PreparedQueryEmbedder` inline when no batch embedder is passed (`crates/jurisearch-cli/src/retrieval/search.rs:303-308`), and `PreparedQueryEmbedder::from_env` probes runtime and constructs a client (`crates/jurisearch-cli/src/embedding_runtime/mod.rs:25-50`). The service needs a long-lived embedder with explicit `Send`/`Sync` or a worker/queue model, plus concurrency limits.
- **Health is not just ops.** Query-service health must be available before phase 7 because it proves active generation, readiness stamp, read role, and embedder fingerprint. Keep systemd/two-host acceptance late, but put service health in the query-service phase.
- **syncd "daemon" is not the same as "writer."** The writer-owned activation/readiness changes belong before the daemon loop. The daemon should compose existing planner/apply seams rather than carrying storage correctness work.

## Style/structure recommendations

Your compact template is good. I would add four required fields to each phase:

- **Invariants under test:** short list of non-negotiable properties, e.g. "read identity cannot `INSERT`", "missing readiness stamp errors, never recomputes", "one request observes one active topology".
- **Compatibility surface:** exactly which existing CLI/session bytes or command behavior must remain unchanged.
- **Negative tests:** not just success tests. Examples: client-supplied `index_dir` ignored/rejected; admin command rejected by site dispatcher; read role cannot write; stale readiness stamp fails; wrong embedder fingerprint errors before dense SQL.
- **Deferrals introduced by this phase:** anything intentionally left incomplete, with a reason and follow-up phase.

I would also keep a one-page dependency/critical-path overview at the top, but make it brutally specific:

```text
shared-server roles -> writer readiness -> snapshot store -> response builders -> service -> thin client
contract/codec/render are enabling seams, not product milestones by themselves
syncd daemon depends on writer readiness, but query service can be developed against one-shot update
multi-corpus fan-out is the largest search semantics change and gets its own gate
```

Finally: keep "codex-reviewed + committed on GO" per phase, but if a phase has sub-gates, require review at each sub-gate. Otherwise phase 3 will be too broad for a useful review.

## Concrete revised phase list

If I were writing the implementation plan, I would use this as the phase spine:

1. **Contract/codec/render foundation and API inventory**
2. **Shared-server storage, pools, roles, and activation read-visibility postcondition**
3. **Writer-owned readiness + apply-time coverage gate + main fingerprint preflight**
4. **Snapshot `QueryStore` + side-effect-free response builders, single-corpus parity**
5. **Multi-corpus physical-generation search fan-out and fusion**
6. **Query service walking skeleton, then concurrent UDS/loopback service with health**
7. **syncd daemon loop over the existing planner/apply substrate**
8. **Thin client, LAN exposure, protocol skew handling, ops, and two-host acceptance**

That is one more phase than your proposal, but it matches the actual risk better. If you need seven, merge 7 and 8 only in numbering; keep their deliverables separate.

## Bottom line

Your guiding principles are sound, but the plan should be less bottom-up-pure and more risk-gated. The implementation plan should not let "extract base crates" hide request DTO dependency cycles, and it should not let "read-only-safe read path" hide four separate correctness changes. Add a walking skeleton, but use it to prove topology and crate boundaries, not to postpone readiness/snapshot correctness.

The single most important planning correction is: **split phase 3 and make every split produce a hard, source-grounded invariant test.**
