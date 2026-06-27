# Verdict: GO with adjustments

The direction is right: move `search`/`cite` construction into `jurisearch-query`, keep the site handlers as thin adapters, and keep the P4 service on blocking workers. The main adjustment is the readiness gate: do not recompute coverage on the read path, but also do not drop readiness validation entirely. Use a snapshot-bound writer-stamp lookup at the service boundary.

1. **Q1 — Site Readiness Gate**

   I would not choose pure **(c)** as stated. There is no normal MVCC window in the official writer path: activation updates `corpus_state`, stamps `public.index_manifest['query_readiness']`, grants visibility, probes the read role, and commits atomically; incremental apply advances `sequence` and restamps in the same cursor-gated transaction. A read snapshot should see either the old stamped topology or the new stamped topology.

   But dropping the explicit stamp check loses the P3A invariant that a missing/stale readiness stamp is a writer/apply fault surfaced by every read path. With pure (c), a manually corrupted DB, an old pre-P3A active generation, a missing/stale `query_readiness` row, or the current multi-corpus non-aggregate stamp gap can still be served if the tables are visible. The dense/hybrid fingerprint preflight only catches model mismatch; it does not prove projection coverage, and it does not protect `fetch`, `cite`, or BM25-only search from an under-projected active generation.

   Use a fourth cut:

   - Open the read snapshot first.
   - Validate the writer-owned readiness stamp inside that snapshot, using the snapshot's resolved `active_corpora()` signature and a read-only lookup of `public.index_manifest`.
   - Do not compute coverage and do not write.
   - Keep the check outside the builders, in a site/read-gate helper called by handlers before invoking the builder.

   This keeps one snapshot per request, avoids a second read-role connection, avoids TOCTOU with the request snapshot, and keeps the builder surface focused on response construction. A good storage helper would be something like:

   ```rust
   pub fn load_query_readiness_in_snapshot(
       snapshot: &mut dyn ReadSnapshot,
   ) -> Result<IngestReadinessReport, StorageError>
   ```

   That helper can derive the signature from `snapshot.active_corpora()` and `SELECT value::text FROM public.index_manifest WHERE key = 'query_readiness'` through `snapshot.read_text`. It should fail on `public`, missing, malformed, or stale signatures.

   Multi-corpus is the caveat. The current source still documents `stamp_query_readiness` as single-corpus coverage with an aggregate signature, and `load_query_readiness_with_client` rejects `count(corpus_state) > 1`. For P4B, either:

   - defer readiness-gated multi-corpus site serving until there is an aggregate/per-corpus readiness stamp, or
   - add the multi-corpus readiness model now.

   Do not silently use health-only reporting as the read gate for multi-corpus. Health can report the gap; request handling should not pretend it has proven aggregate readiness.

2. **Q2 — `build_search` / `build_cite` Boundary**

   The proposed adapter/builder cut is mostly right. Keep adapter-side work to wire validation, defaulting, parsing, and side-effect boundaries; keep response semantics in `jurisearch-query`.

   For `build_search`, I would keep these adapter-side:

   - empty query / `top_k == 0` / retrieval-option validation;
   - zone routing decision, unless zone is also moved in this slice;
   - cursor parsing into `ParsedSearchCursor`;
   - `as_of` default resolution;
   - kind conversion into contract/storage vocabulary;
   - authority preconditions, especially `--kind decision` and first-page-only/no-cursor;
   - lexical pre-tokenization and the "must contain at least one searchable token" error precedence.

   Passing a pre-resolved `authority_weight: Option<f64>` into the builder is fine. It avoids duplicating env/default interpretation and lets the builder own the actual rerank/envelope behavior. Cursor parsing should remain adapter-side; it is boundary validation and should fail before a snapshot is opened.

   Avoid letting `jurisearch-query` depend on CLI enums. Use `jurisearch_core::contract::{LegalKind, OutputFormat}` and `jurisearch_core`/`jurisearch_storage` retrieval types. If `DecisionFilters` needs to cross the builder boundary, prefer an owned query-crate input struct with `Option<String>` fields, then borrow it into storage's `DecisionFilters<'_>` inside `build_search`. A storage `DecisionFilters<'a>` value in a public input struct is awkward because its lifetime is tied to the caller's request object.

   Include the output format explicitly, not only `detailed: bool`, unless you also carry the exact response label. The builder writes the `"format"` field, so `SearchInput` should carry either `OutputFormat` or an equivalent canonical label.

   For `cite`, passing `online_requested` into the builder is the cleaner byte-parity move. The current response state is not fully online-agnostic:

   - `source_unavailable` depends on `req.online` for statutory citations with no local matches;
   - malformed citations get an online-not-sent note;
   - decision citations get the Judilibre-not-wired note;
   - the base `"online": { "requested", "checked": false, ... }` block is part of the response envelope.

   That belongs with citation response construction, not with the network adapter. Keep `apply_online_citation_confirmation` in the CLI adapter after `build_cite`; it can overwrite `response["online"]` exactly as today.

   For the site handler, make an explicit product choice for `online: true`. If the site never performs the network probe, the safest P4B contract is to reject `online: true` at the site boundary. If you decide to accept it, pass `online_requested: true` to the builder and make the checked-false note explicit; do not silently look like the Légifrance probe ran.

3. **Q3 — Concurrency / Worker Count as Read Bound**

   Using the worker count as the read-connection bound is acceptable for 4B, given the current `ReadHandle` design. The code now implements `QueryStore` for `ReadHandle`, and each `begin_snapshot()` opens a fresh least-privilege read-role `postgres::Client`. If each request opens and drops one snapshot, then `worker_count` is the hard upper bound on simultaneous read-role connections.

   Be precise in naming and health output: this is not a reusable connection pool yet. It is a bounded worker model with bounded fresh connection creation. Report it as `max_workers` / `max_read_connections`, not idle/checked-out pool stats.

   The tradeoff is connection churn. That is fine for the 4B slice if the configured worker count is modest and connect timeout stays bounded. If PostgreSQL connection setup shows up in latency, add a real reusable pool later without changing the handler/builder boundary.

   A few implementation constraints matter:

   - The current `ServerContext<'a> { store: &'a dyn QueryStore }` needs to grow an embedder reference for full search/compare. In workers, hold `Arc<ReadHandle>` or `Arc<dyn QueryStore + Send + Sync>` and `Arc<dyn QueryEmbedder + Send + Sync>`, then build a borrowed `ServerContext` per request or per connection.
   - `QueryEmbedder` itself has no `Send + Sync` bound today. Add service-bound trait objects and compile-time assertions for `PreparedQueryEmbedder` / `OpenAiCompatibleClient`. If the assert fails, use per-worker embedders.
   - `SiteDispatcher` is shareable via `Arc` only if every handler is `Send + Sync`; the trait already requires that, so let the compiler prove the full dispatcher object.
   - A `sync_channel` of accepted connections bounds accepted sockets, but a slow or idle persistent JSONL client occupies a worker for the lifetime of that connection. Keep read/write timeouts, and consider a max idle or max requests per connection. Request-level fairness can wait; connection-level workers are acceptable for loopback/UDS P4B.
   - Use a blocking semaphore for embeds. A small `Mutex + Condvar` permit guard is enough; no async runtime is warranted.

## Minimal P4B Cut

For the minimal testable slice, I would implement:

- move pure `search`/`cite` helpers into `jurisearch-query`, re-exporting from CLI only to keep existing local references compiling;
- add `build_search` and `build_cite` with input structs that use contract/storage vocabulary, not CLI enums;
- keep `cite --online` network enrichment in the CLI adapter;
- add snapshot-bound readiness-stamp validation in the site handlers before builders;
- register all seven `Operation`s only when their handlers are real, or return `NotImplemented` for any operation still intentionally deferred;
- add worker-thread concurrency with worker-count read bounding and embed concurrency limiting;
- add tests for site `search`/`cite` parity, site rejection or explicit handling of `online: true`, missing/stale readiness stamp rejection, embedder fingerprint mismatch through the site dispatcher, and worker-bound behavior with a fake store counter.

The only item I would not postpone is the readiness lookup. Without it, the site path is fast and clean, but it no longer enforces the writer-owned readiness contract that P3A introduced.

