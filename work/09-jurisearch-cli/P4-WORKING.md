# P4 working notes — codex design GO-with-adjustments (qa/20260627-153008)

P4 = query service. Split into **4A walking skeleton** (one review gate) then **4B concurrent + health**.
Keep the site dispatcher SEPARATE from local `serve_jsonl`; the site service NEVER calls
`dispatch_session_request` (the local index_dir-injecting path).

## CRITICAL pre-work (do FIRST)
`ReadHandle` is NOT a `QueryStore` yet — `QueryStore` is impl'd only for `ManagedPostgres` (superuser).
4A must use the READ-ONLY identity. Add a read-role `QueryStore` adapter (a `ReadPoolQueryStore` over
`ReadHandle`, or a size-1 read pool wrapper) whose `begin_snapshot` checks out ONE read-role client and
opens `LocalSnapshot` on it. Never read through a superuser `ManagedPostgres` in the service.

## Binding adjustments

1. **Home.** `crates/jurisearch-cli/src/site/` (submods: `dispatcher`, `handlers`, `listener`, `health`)
   + a new `serve-site` subcommand. Heavy stack stays in CLI; `jurisearch-query` stays light. NO separate
   crate for 4A. Share only low-level bind helpers with serve.rs if clean.
2. **Concurrency = blocking threads** (not async — storage is blocking libpq, embed is blocking HTTP). 4A
   sequential/size-1 but using the SAME request path as 4B (decode envelope → one snapshot → dispatch →
   encode response line). 4B: bounded worker pool + bounded request queue + real bounded read pool; each
   snapshot owns/checks-out ONE read-role client (NEVER share `postgres::Client`).
3. **Embedder.** Add `Arc<dyn QueryEmbedder + Send + Sync>` at the service boundary + a COMPILE-TIME
   assertion test that the concrete CLI `PreparedQueryEmbedder` is Send+Sync (it wraps
   `OpenAiCompatibleClient` = `ureq::Agent` + tokenizer — VERIFY). If passes → share one `Arc` + a
   semaphore concurrency limit. If not → per-worker embedders (NOT a global Mutex). 4A doesn't need it.
4. **Search/cite builders.** Do NOT call the CLI snapshot-bound adapters from site handlers (too tied to
   index_dir/env/local render). 4A: pick an op with a clean `jurisearch-query` builder. 4B: MOVE
   search/cite response construction into `jurisearch-query` before registering them.
   **Handler trait shape:** `fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value,
   ErrorObject>` — the DISPATCHER attaches the request id + builds `SessionResponse` (centralize
   correlation + error wrapping).
5. **4A skeleton op = `fetch`** (real builder + snapshot + read-only identity + parity + server-owned
   context, NO embedder). Add a minimal NEW `status`/health handler — NOT the local `status_payload`
   (it reads index_dir + probes model/cache). If exactly one op: `fetch`.
6. **index_dir boundary = in the DISPATCHER, BEFORE handler validation.** REJECT any client-owned
   data-source field (`index_dir`) with an `ErrorObject` before dispatch (stronger + easier to test than
   silent stripping). Test: a sentinel `index_dir` (valid for the LOCAL dispatcher) → boundary error
   WITHOUT reaching a handler.
7. **Health (diagnostic, not a recompute path).** Report TRUE topology: per active corpus
   (corpus/generation/schema/sequence/fingerprint); server embedder fingerprint if configured;
   single-corpus readiness stamp when exactly 1 corpus; explicit `multi_corpus_readiness: deferred` when
   >1 (until aggregate/per-corpus stamp exists); read-pool status ONLY when a real pool (4A: report
   provider/size-1, not invented idle counts). Query ops still fail closed on their own checks.
8. **Dispatcher (OCP).** `SiteDispatcher { handlers: HashMap<Operation, Box<dyn OperationHandler>> }`,
   `ServerContext { store: &dyn QueryStore, embedder: &dyn QueryEmbedder }`. Allowlist = only the 7
   `Operation`s registered; a non-registered command → `ErrorObject`, NEVER the local dispatcher, NEVER an
   index_dir-aware payload. Valid-but-unimplemented-in-4A ops → unregistered → clear "not implemented"
   ErrorObject.

## Transport/wire notes
- Decode via `decode_site_envelope_line` (versioned). Bounded line + LOUD rejection of unversioned/skewed
  frames BEFORE dispatch. Response = compact `SessionResponse` JSONL line (NOT local human render on the
  server). render-parity is a CLIENT/TEST assertion over the returned response, not server wire format.
- Transport error policy: session error line when the request id is recoverable; close the connection for
  framing-level failures. Unversioned site frames NEVER reach the dispatcher.

## Minimal 4A checklist (one review gate)
serve-site subcommand + separate `site/` module · UDS + loopback only (no remote) · versioned decode +
unversioned-rejection · `Operation::parse_command` allowlist · read-role `QueryStore` adapter · dispatcher
index_dir rejection · `fetch` handler (over the `jurisearch-query` builder) + minimal NEW health · response
as SessionResponse JSONL · tests: non-site cmd → ErrorObject; unversioned frame rejected; client index_dir
rejected; fetch parity via `jurisearch-render`; (compile-time Send+Sync embedder assert can wait for 4B).

## 4B (second gate)
worker pool + bounded read pool · full op set · search/cite → `jurisearch-query` · shared/per-worker
embedder Send+Sync + concurrency limit · concurrent-client independent-snapshot tests · health real pool
status · fingerprint-mismatch tests THROUGH the site dispatcher.

## Status
- [x] pre-work: `impl QueryStore for ReadHandle` (read-role store) in storage `query.rs`.
- [x] 4A IMPLEMENTED + in codex review (2026-06-27-P4A-skeleton-codex-review.md): `crates/jurisearch-cli/
  src/site/` {dispatcher (allowlist + index_dir rejection + OCP handler map), handlers (FetchHandler over
  build_fetch + HealthHandler), listener (versioned framing), serve (UDS/loopback runner)} + `serve-site`
  subcommand. Tests: dispatcher (table-driven allowlist, unregistered-op, index_dir) + e2e (versioned
  fetch through read role, health, unversioned-frame + index_dir rejection). CLI bins 77, byte-parity/
  session intact. UNCOMMITTED until GO.
- [ ] 4B (after 4A GO): worker/read pools, full op set, search/cite → jurisearch-query, shared embedder
  Send+Sync + concurrency limit, concurrent-client tests, real pool health, fingerprint-mismatch via dispatcher.