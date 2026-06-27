# Verdict: GO with adjustments

The P4 shape is sound: keep the site dispatcher separate from the existing local `serve_jsonl`, use the versioned site transport, make the server own the `QueryStore` and embedder, and prove the first service slice with the read-only identity. The adjustments are mostly about not accidentally smuggling local/self-managed assumptions into the site server.

The largest source-code mismatch is that P4's "read pool" does not exist yet. `StorageBackend` currently returns `ReadHandle`/`WriterHandle` providers, and `QueryStore` is implemented for `ManagedPostgres`, not for `ReadHandle`. So 4A needs a small read-role `QueryStore` adapter, or a size-1 read pool wrapper, before the service can honestly claim read-only DB identity.

1. **Home for 4A/4B**

   `jurisearch-cli` is the right composition root for 4A. The service needs the heavy stack: socket serving, storage backend construction, local embedding runtime, and server configuration. Keeping that out of `jurisearch-query` preserves the dependency-light builder crate and keeps P6's thin client cone clean.

   Do put it behind a new, separate module boundary, for example `crates/jurisearch-cli/src/site/`, with submodules for `dispatcher`, `handlers`, `listener`, and `health`. Do not extend the existing local `serve.rs` control flow beyond sharing low-level bind helpers if that stays clean. Local `serve_jsonl` injects `index_dir` and delegates to `dispatch_session_request`; the site service must never call that path.

   A separate service crate is not required for 4A. It may become useful if P5 wants a single long-running composition root shared by daemon and query service, but doing that now is extra churn before the service boundary has proven itself.

2. **Concurrency model**

   Use blocking threads, not async. The storage path uses blocking `postgres::Client`, and the embedding client path uses blocking HTTP. Introducing async now would mostly wrap blocking work in an executor and make connection ownership harder to reason about.

   For 4A, keep the server sequential or effectively size-1, but make it use the same request path as 4B: one request decodes one versioned envelope, opens one read snapshot through the server-owned store, dispatches one handler, and encodes one response line.

   For 4B, add the bounded worker pool, bounded request queue, and real bounded read connection pool. Do not share a `postgres::Client` across workers; each snapshot should own or check out exactly one read-role client.

   Adjustment before coding: add a `QueryStore` implementation over `ReadHandle` or a small `ReadPoolQueryStore`. Today `LocalSnapshot::open(self.client()?)` is available through `ManagedPostgres`, but the site service must not rely on a `ManagedPostgres` superuser connection for reads.

3. **Embedder thread-safety**

   The design is right, but verify it in the type system before sharing a singleton. `QueryEmbedder` currently has no `Send + Sync` bound, and `PreparedQueryEmbedder` wraps `OpenAiCompatibleClient`, which contains a `ureq::Agent` and tokenizer state. The implementation uses `&self`, but that is not enough proof for 4B.

   Add a service-facing bound such as:

   ```rust
   Arc<dyn QueryEmbedder + Send + Sync>
   ```

   and a compile-time assertion test for the concrete CLI embedder. If it passes, share one embedder instance behind an `Arc` and bound concurrent embedding calls with a semaphore or equivalent blocking limiter. If it does not pass, prefer per-worker embedders over a global `Mutex`; use a dedicated embed worker queue only if the endpoint itself requires serialized access or stronger backpressure.

4. **Search and cite builders**

   Do not call the existing CLI snapshot-bound adapters from the site handlers. Even inside the same crate, those adapters are part of the local CLI surface and are too easy to tie back to `index_dir`, environment setup, or local rendering assumptions.

   For 4A, choose an operation that already has a clean `jurisearch-query` builder, so this is not blocking. For 4B, move `search` and `cite` response construction into `jurisearch-query` before registering them as real site handlers. The site handler should be a thin adapter: validate args, open snapshot, call the builder/storage primitive, wrap the result in `SessionResponse`.

   I would also slightly change the handler trait shape. Let handlers return `Result<Value, ErrorObject>` and let the dispatcher attach the request id and construct `SessionResponse`. That keeps correlation and error wrapping centralized:

   ```rust
   trait OperationHandler {
       fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject>;
   }
   ```

5. **4A skeleton op**

   Pick `fetch` as the main skeleton operation. It exercises the real builder crate, the read snapshot, the read-only DB identity, JSON result parity, and server-owned context without needing the embedder.

   I would still add a minimal `status`/health handler in 4A if it is cheap, but do not use the existing local `status_payload`. That local path reads `index_dir`, probes model/cache state, and opens local runtime surfaces. Site health should be a new server-context health response.

   If forced to implement exactly one operation in 4A, use `fetch`; status alone proves less of the actual query topology.

6. **Server-owned context and `index_dir` stripping**

   Put the boundary in the dispatcher, before handler validation. Do not rely on individual builders ignoring unknown fields. A robust 4A rule is: site requests may not supply client-owned data-source fields such as `index_dir`; reject them with an `ErrorObject` before dispatch.

   That is stronger and easier to test than silent stripping. The design allows ignored or stripped fields, but rejection proves the same invariant: client filesystem hints cannot influence the site service.

   Test this with a request containing a sentinel `index_dir` that would be valid for the local dispatcher, and assert the site dispatcher returns the boundary error without reaching a handler. A second unit test with a fake handler can assert sanitized/validated args contain no `index_dir` if you choose stripping instead of rejection.

7. **Health and readiness under multi-corpus**

   Do not report fake aggregate readiness. The current strict readiness lookup still has single-corpus assumptions, while P3C lifted search fan-out. Site health should report the true topology:

   - active corpus, generation, schema, sequence, and fingerprint for every active corpus;
   - server embedder fingerprint, if configured;
   - single-corpus readiness stamp when exactly one corpus is active;
   - an explicit `multi_corpus_readiness: unavailable/deferred` state when multiple corpora are active, until an aggregate or per-corpus readiness stamp exists;
   - read-pool status only when there is a real pool; in 4A, report provider/size-1 mode rather than invented idle counts.

   Query operations should still fail closed where their own readiness/fingerprint checks require it. Health should be diagnostic, not a recompute path.

8. **Minimal 4A vs 4B**

   Minimal 4A should include:

   - new `serve-site` subcommand and separate site module;
   - UDS and loopback-only binding, with no remote mode in the skeleton;
   - versioned request decode via `decode_site_envelope_line`;
   - bounded line handling and loud rejection of unversioned/skewed frames before dispatch;
   - response encoding as a session response line, not local human rendering on the server side;
   - `Operation::parse_command` based allowlist;
   - implemented `fetch` handler, plus optional minimal health/status;
   - read-role `QueryStore` adapter over `ReadHandle` or a size-1 read pool;
   - dispatcher-level rejection or stripping of `index_dir`;
   - tests for non-site commands returning `ErrorObject`, unversioned site frames rejected, client `index_dir` rejected/ignored, and fetch parity through `jurisearch-render`.

   4B should include:

   - bounded worker-thread pool and bounded read pool;
   - full site operation set;
   - search/cite builder move into `jurisearch-query`;
   - shared or per-worker embedder with proven `Send + Sync` behavior and concurrency limiting;
   - concurrent client tests proving independent snapshots;
   - health with real pool status;
   - fingerprint mismatch tests through the site dispatcher, not only through storage/CLI.

   For valid site operations not yet implemented in 4A, either leave them unregistered and return a clear `not implemented` `ErrorObject`, or register placeholder handlers that do the same. Keep the outer allowlist closed to the seven `Operation` variants either way. Non-site commands must never fall through to the local dispatcher.

## Additional Phase 4 Risks

- **`ReadHandle` is not yet a `QueryStore`.** This is the first thing to fix for a truthful 4A. Otherwise the service will accidentally test against self-managed/superuser behavior.
- **Transport errors need a response policy.** Decide whether malformed/oversize/version errors produce a session error line when an id is recoverable, or close the connection for framing-level failures. The key invariant is that unversioned site frames never reach the dispatcher.
- **Wire response vs render parity can get confused.** The server should encode compact `SessionResponse` JSONL. `jurisearch-render` parity is a client/test assertion over the returned response, not necessarily the server's wire formatting.
- **Local status is unsafe to reuse.** It is `index_dir`-centric and environment-aware. Site health needs its own server-context implementation.
- **Trait object thread bounds will surface in 4B.** Add `Send + Sync` requirements at the service boundary early enough that the worker-pool refactor does not become a large surprise.

