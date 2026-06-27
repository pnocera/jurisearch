# Target Architecture Review

Review scope: `work/09-jurisearch-cli/02-target-architecture.md`, checked against the current source in `/home/pierre/Work/jurisearch`.

## Findings

### BLOCKER 1 - A read-only query service cannot run the current search/readiness path

The architecture says the query service should connect as a read-only PostgreSQL role while syncd is the only writer. That target is correct, but the current query path is not read-only: `search_payload` calls `search_with_postgres`, which calls `ensure_query_readiness` before retrieval (`crates/jurisearch-cli/src/retrieval/search.rs:190-197`). `ensure_query_readiness` delegates to `load_or_compute_query_readiness` (`crates/jurisearch-cli/src/index_runtime.rs:97-105`), and on a fully-ready cache miss that storage helper writes `index_manifest` with `INSERT ... ON CONFLICT DO UPDATE` (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:225-239`). The shared read helpers for fetch/context/related/inspect also go through `open_query_index`, which runs the same readiness gate (`crates/jurisearch-cli/src/index_runtime.rs:44-55`).

With a real read-only role, the first query after activation, cache invalidation, or cache absence can fail even though the corpus is queryable. Granting that role write access to `index_manifest` would defeat the least-privilege property the architecture relies on.

Recommendation: make readiness a syncd/apply responsibility or provide a strictly read-only readiness path for `jurisearch serve`. The query service should only read an activation-time readiness stamp, or compute readiness without caching writes. Add a regression that creates a SELECT-only query role, clears the readiness cache, and proves `search`, `fetch`, `cite`, `related`, and `status` still behave as intended.

### BLOCKER 2 - The snapshot-consistency claim needs a transaction-bound read API

The document's central consistency claim is "single writer + pooled readers + generations behind views + snapshot-isolated reads => no torn reads." The generation switch itself is atomic: `activate_generation_with_guard` updates the registry, writes `corpus_state`, writes dense manifest rows, rebuilds views, and commits one transaction (`crates/jurisearch-storage/src/generations.rs:947-1133`). The current read side, however, does not provide the one-request snapshot the document assumes.

`ManagedPostgres::execute_read_sql` first reads `corpus_state` to choose active schemas, then runs `SET search_path` plus the caller SQL through a separate `execute_sql` invocation (`crates/jurisearch-storage/src/runtime.rs:293-307`). Readiness has the same shape on one connection but multiple READ COMMITTED statements: `apply_read_search_path` reads `active_generation`, sets `search_path`, then calls `active_read_signature` in another statement (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:50-73`). A generation swap between those statements can leave a request using an old search path while recording or trusting a new signature. A full search request also runs readiness, dense-probe lookup, and retrieval across separate connections/statements (`crates/jurisearch-storage/src/retrieval/sql.rs:21-28`, `crates/jurisearch-storage/src/retrieval/hybrid.rs:19-22`, `crates/jurisearch-storage/src/retrieval/hybrid.rs:137`).

That means the current helpers cannot simply be pooled and called concurrent while preserving the target's "one PG transaction/snapshot" invariant. The old generation usually remains until cleanup, so this is more likely to produce stale or failed reads than mixed rows, but it is still not the model the document claims.

Recommendation: make Phase 3 explicitly introduce a pooled read transaction abstraction: `BEGIN READ ONLY ISOLATION LEVEL REPEATABLE READ`, resolve `corpus_state` and active compatibility stamps inside that transaction, `SET LOCAL search_path`, then run readiness, probe metadata, and retrieval before commit. Retired-generation cleanup must be coordinated with active read transactions or rely on PostgreSQL locks with bounded retry. Add a concurrent apply/read test that swaps a generation between read-routing and retrieval and proves the request sees either the old or new generation consistently.

### BLOCKER 3 - Reusing `dispatch_session_request` wholesale is not safe for a LAN service

The document says to keep the JSONL contract and `dispatch_session_request` so the daemon fans requests across a pool. The current dispatcher is a local-session dispatcher, not a site-service boundary. It exposes more than the target query surface: `model fetch`, `eval phase1`, `setup`, `doctor`, `stats`, `inspect`, `versions`, and `diff` are session-callable alongside search/fetch/cite/context/related/compare (`crates/jurisearch-cli/src/session.rs:127-145`). The request DTOs also carry `index_dir`, and the current socket server only injects its default when the client omitted it (`crates/jurisearch-cli/src/serve.rs:20-32`, `crates/jurisearch-cli/src/serve.rs:99-103`). A client-supplied `index_dir` therefore remains client-controlled.

That is incompatible with the target service owning one shared PG pool and thin clients owning no database/model. If the existing dispatcher is exposed off-host, an authenticated client can steer server-side requests toward arbitrary local index directories or invoke local/admin/model/session commands that were never designed as a LAN API. The planned read-only PG role also does not protect this path because today's payloads open managed Postgres instances from `index_dir` (`crates/jurisearch-cli/src/index_runtime.rs:39-42`).

Recommendation: keep the wire envelope, but introduce a site-service dispatcher/adapter with an explicit allowlist and server-owned context. It should reject or strip all client `index_dir` fields, use only the configured shared PG pool, and expose only intentionally supported operations. Admin/model/doctor/eval/local commands should require a separate local management surface, not the thin-client API.

### WARN 1 - "Never blocks readers" overstates the generation-swap behavior

The advisory apply lock itself does not block readers, but activation also rebuilds the stable `jurisearch_server` views with `CREATE OR REPLACE VIEW` inside the switch transaction (`crates/jurisearch-storage/src/generations.rs:843-872`, `crates/jurisearch-storage/src/generations.rs:1131`). PostgreSQL DDL takes locks on those views. Non-indexed reads that go through the stable views can briefly wait behind the switch, and a long reader can make activation hit the `5s` `lock_timeout` set in `activate_generation_with_guard` (`crates/jurisearch-storage/src/generations.rs:967-971`).

Recommendation: change the architecture text from "never blocks readers" to a bounded-lock model: hot physical-generation reads remain online; view-backed reads may briefly wait; syncd treats lock-timeout activation failures as retryable. Add load/swap validation that includes long view-backed reads, not only hot indexed search.

### WARN 2 - The embedding-fingerprint guard must be active-generation preflight, not only row filtering

The current search-time embedding step is discrete as claimed: `PreparedQueryEmbedder::from_env` builds an expected/storage fingerprint and `embed` returns the query vector plus storage fingerprint (`crates/jurisearch-cli/src/embedding_runtime/mod.rs:25-50`). The dense query passes that fingerprint into retrieval (`crates/jurisearch-cli/src/retrieval/search.rs:303-319`), and SQL filters dense rows by `ce.embedding_fingerprint = <query fingerprint>` (`crates/jurisearch-storage/src/retrieval/sql.rs:119-124`, `crates/jurisearch-storage/src/retrieval/sql.rs:217-222`).

That row-level filter is not the same as the target's structured "active generation was built for a different query shape" refusal. If the server's embedder fingerprint differs from `corpus_state.embedding_fingerprint`, dense results can silently disappear or hybrid retrieval can degrade toward lexical results instead of returning a precise compatibility error.

Recommendation: before serving dense/hybrid search, read the active corpus stamps in the same read transaction and compare them with the server embedder fingerprint. Return a structured `embedding_fingerprint_mismatch`-style session error before retrieval when they differ, and expose the active/server fingerprints in health/status. Cover multi-corpus behavior explicitly, because `index_manifest` dense metadata is still global in the current source (`crates/jurisearch-storage/src/generations.rs:891-896`).

### WARN 3 - Thin-client build separation is feasible, but much larger than a simple feature flag

The document correctly says a thin build should avoid storage, embedding, ingest, and model assets. The current `jurisearch-cli` binary is not close to that shape: `crates/jurisearch-cli/Cargo.toml` unconditionally depends on `jurisearch-embed`, `jurisearch-ingest`, `jurisearch-official-api`, `jurisearch-storage`, and `postgres` (`crates/jurisearch-cli/Cargo.toml:12-25`); `main.rs` imports all of those at the crate root (`crates/jurisearch-cli/src/main.rs:18-107`); and even argument definitions import ingest/storage enums (`crates/jurisearch-cli/src/args.rs:14-17`).

Recommendation: make Phase 5 price the extraction explicitly. The lower-risk path is a dedicated thin binary or crate depending on `jurisearch-core` plus transport/rendering code, while the current heavy binary remains the local/server/admin binary. If feature gating is chosen, first split request DTOs, rendering, and client commands away from storage/embed/ingest-only args, then enforce it with `cargo tree`/binary-size checks.

### WARN 4 - Shared-secret-over-TCP is not a complete LAN auth story

The target correctly refuses unauthenticated off-host binds, but "shared-secret / token (or mTLS)" leaves the unsafe option too open. A raw shared secret over JSONL/TCP is replayable and observable on a hostile LAN, gives no per-client revocation, and makes audit/rate-limiting coarse. The same service will hold corpus access and embedding/provider credentials, so this boundary needs a stronger default.

Recommendation: make UDS with filesystem permissions the local default and TLS-protected mTLS or bearer-token-over-TLS the LAN default. Document token rotation, per-client identity/audit, request-size/idle/rate limits, and whether the service is allowed to expose any online API-backed operations.

### NIT 1 - Phase ordering should make the unauthenticated intermediate state explicit

The roadmap puts "Concurrent read-only query service" before "Client transport + auth + protocol versioning", while the body describes the query service as concurrent and authenticated. That is fine only if the Phase 3 service remains loopback/UDS-only and cannot bind off-host.

Recommendation: add that constraint to Phase 3, then make Phase 4 the first phase allowed to expose LAN TCP.

## Grounding Notes

The document's main starting-point claims are accurate:

- `jurisearch serve` is currently single-client/sequential, loopback-protected by default, unauthenticated, and delegates JSONL requests to `dispatch_session_request` (`crates/jurisearch-cli/src/serve.rs:1-4`, `crates/jurisearch-cli/src/serve.rs:72-80`, `crates/jurisearch-cli/src/serve.rs:125-156`).
- The JSONL request/response contract is in `jurisearch-core` (`crates/jurisearch-core/src/session.rs:6-47`), and `dispatch_session_request` dispatches the current session commands (`crates/jurisearch-cli/src/session.rs:121-159`).
- The CLI and syncd currently start durable managed Postgres instances; there is no external shared-server attach path in the reviewed source (`crates/jurisearch-cli/src/index_runtime.rs:39-42`, `crates/jurisearch-syncd/src/main.rs:88-92`, `crates/jurisearch-storage/src/runtime.rs:163-230`).
- Query embedding is a discrete search-time step through `PreparedQueryEmbedder` (`crates/jurisearch-cli/src/embedding_runtime/mod.rs:25-50`, `crates/jurisearch-cli/src/retrieval/search.rs:303-319`).
- The work/08 package-distribution guarantees exist in code: signed manifests and trust anchors (`crates/jurisearch-syncd/src/trust.rs:34-53`), entitlement preconditions (`crates/jurisearch-syncd/src/trust.rs:55-101`), schema/extension/client-version gates (`crates/jurisearch-syncd/src/apply.rs:139-145`, `crates/jurisearch-syncd/src/apply.rs:274-356`), build-before-activation and atomic switch (`crates/jurisearch-syncd/src/apply.rs:219-264`, `crates/jurisearch-storage/src/generations.rs:947-1133`), cursor-gated incremental apply (`crates/jurisearch-syncd/src/apply.rs:851-982`), and the closed §6.3 reject-code vocabulary (`crates/jurisearch-package/src/reject.rs:10-113`).

## Roadmap Assessment

The high-level order is right: external shared-server storage has to land before daemonized syncd and pooled serve; thin packaging should wait until the server protocol and auth are real. The roadmap needs to add the missing prerequisites above to the early phases: read-only-safe query paths, transaction-bound read routing, server-side request allowlisting, role/grant/default-privilege setup for future generation schemas, and explicit loopback-only constraints before auth.

VERDICT: FIXES_REQUIRED
