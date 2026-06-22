I have enough to complete the review. I've verified the embed library batch path, the CLI pool implementation, the storage coverage gate (`finalize_dense_rebuild`), the per-batch durable insert (`insert_chunk_embeddings`), and the chunk loader (`load_chunk_embedding_inputs`).

---

# Review — build-time embedding endpoint pool for `ingest embed-chunks`

## 1. Findings (ordered by severity)

### M1 — Doc promises "re-queue failed batches on node failure"; implementation aborts the whole run on first failure
`embed_and_insert_chunks_with_pool` sets `stop_requested` and captures `first_error` on the first batch failure or insert failure (`crates/jurisearch-cli/src/main.rs:2363-2374`, `2370-2373`). Workers then drain and exit; the failed batch's chunks are **not** re-dispatched to another endpoint. So a single transient blip on any of the three nodes during the ~1.8 h bulk pass aborts the entire command. The reviewed doc claims otherwise: `work/03-implementation/00-setup/embeddings-endpoint.md` ("re-queue failed batches on node failure") and `IMPLEMENTATION_PLAN.md` ("resumable via projection-coverage").

This does **not** violate the non-negotiable ("insert completed batches durably and finalize only after full coverage") — completed batches are committed and a re-run converges — but the doc over-claims in-run failover that isn't there. Recommend either adding per-batch retry/failover to a healthy endpoint, or softening the doc to "failed batches abort the run; re-run converges."

### M2 — `embed-chunks` re-embeds the full corpus every run; doc claims "skip already-embedded chunks"
`load_chunk_embedding_inputs` selects **all** chunks with no filter on embedding presence (`crates/jurisearch-storage/src/dense.rs:34-78`), and `embed_chunks_payload` feeds the whole set into the pool (`main.rs:2106`). Inserts are idempotent (`ON CONFLICT (chunk_id) DO UPDATE`, `crates/jurisearch-storage/src/projection.rs:648-652`), so a re-run *converges*, but it re-embeds everything — a late failure on the 1.85 M-chunk pass wastes the whole prior run. The doc (`embeddings-endpoint.md`, `IMPLEMENTATION_PLAN.md`) explicitly says "skip already-embedded chunks," which the code does not do.

Compatible fix (Codex can apply): filter the loader to chunks lacking a matching `chunk_embeddings` row for the target fingerprint (the same predicate `finalize_dense_rebuild` already uses for its `missing` count, `dense.rs:109-121`). `finalize_dense_rebuild`'s full-coverage gate makes this safe — partial coverage still hard-fails before the index/manifest is advertised.

### L3 — Serialized inserter + per-batch connection + unbounded channel may bottleneck/accumulate
All DB inserts run on the single receiver thread (`main.rs:2342-2375`), and `insert_chunk_embeddings` opens a fresh `postgres::Client::connect` per batch (`projection.rs:640-641`). At batch-size 32 over 1.85 M chunks that's ~58 k connect/commit cycles on one thread, which can cap throughput below the ~288 t/s pooled embed rate the doc targets. Because the `mpsc` channel is unbounded, if inserts lag the workers, completed embedding literals (~8 KB each as pgvector text) accumulate in memory. Not a correctness defect; worth a follow-up (reuse one connection / larger insert batches / bounded channel) if the inserter proves to be the gate.

### L4 — Per-endpoint fingerprint check is effectively a tautology
`embedding_endpoint_pool_configs` builds each endpoint config as `config.clone()` with only `base_url`/`base_urls` swapped, then compares provider/model/dimension/normalize/pooling/storage-fingerprint, deliberately excluding `base_url_class` (`main.rs:2261-2276`). Since every field it compares is an unchanged clone, the check can never return `Err`. The transport-only invariant is genuinely upheld (the stored `storage_embedding_fingerprint` = `model:dim:normalize:bool` excludes the URL, confirmed at `crates/jurisearch-embed/src/lib.rs:265-270`), but this guard validates nothing about the *live* endpoints. Real divergence is only caught at embed time via `DimensionMismatch`/`NormalizationMismatch` (`lib.rs:438-455`); a different model serving the same 1024-d normalized vectors is undetectable here (relies on the manually-verified cross-node cosine 0.99997 in the doc). Acceptable, but the static guard reads as stronger validation than it is.

### L5 — New pool test routing assertion is scheduling-dependent (flake risk)
`ingest_embed_chunks_uses_endpoint_pool_and_finalizes_dense_index` asserts the slow server receives `["alpha"]` and the fast server `["beta"]` (`crates/jurisearch-cli/tests/cli_contract.rs`). Routing depends on which worker reaches `acquire_least_outstanding_endpoint` first after popping from the queue — there is no synchronization guaranteeing the alpha-holding worker acquires endpoint index 0 before the beta-holding worker does. The robust assertions (`chunks_considered==2`, `embeddings_inserted==2`, summed endpoint `chunks==2`, dense rebuild counts) are fine; the per-endpoint `input` asserts can flake under load. Consider asserting only that each endpoint served exactly one batch and the two inputs were covered.

### N6 — Nits
- `EmbeddingBatchSuccess.endpoint_index` is dead: produced in `embed_batch_on_endpoint` (`main.rs:2523`) and discarded via `let _ = success.endpoint_index;` (`main.rs:2368`). Per-endpoint accounting flows through `endpoint_states`, so the field can be dropped.
- `status_payload` and the embed payload echo `base_urls` verbatim (`main.rs:3322`, `main.rs:2157`), like the existing `base_url`. If any URL embedded userinfo it would not be redacted — pre-existing pattern, low risk, but now multiplied across the pool.

## 2. Open questions / residual risks
- **Resumability semantics**: the non-negotiable contract (durable per-batch insert + finalize-after-full-coverage) is met, but the documented behavior (skip-already-embedded + in-run failover) is not. Decide whether the docs should be tightened or the code enriched (M1/M2). Given the docs are part of this diff, the mismatch is the main residual risk.
- **No connectivity preflight for pool members**: `ensure_embedding_runtime_ready` runs once against the base config (`main.rs:2084`); the two remote endpoints aren't health-checked before the run, so a dead remote surfaces only at embed time → abort (compounds M1).
- **Insert throughput vs. pooled embed throughput** (L3): unverified whether the single-threaded inserter sustains the pool's output for the full corpus.

## 3. Verification notes
- Confirmed the **transport-only** invariant: `storage_embedding_fingerprint` (`lib.rs:265-270`) excludes base-url/base-url-class; pooling changes neither the stored fingerprint nor `chunks.embedding_fingerprint` (`projection.rs:657-660`).
- Confirmed **query-time isolation**: `OpenAiCompatibleClient` only ever reads `self.config.base_url` (`lib.rs:393-400`); `base_urls` is consulted exclusively by `embedding_endpoint_pool_configs`, called only from `embed_chunks_payload`. Search/query paths are unaffected.
- Confirmed **finalize-after-full-coverage**: `finalize_dense_rebuild` hard-fails on any `missing` chunk before creating the index/manifest (`dense.rs:109-129`), and is reached only after `embed_and_insert_chunks_with_pool` returns `Ok` (`main.rs:2122-2143`).
- Confirmed **durable per-batch insert**: each batch commits its own transaction atomically (`projection.rs:642-701`); a failed chunk-fingerprint update rolls back that batch only.
- Confirmed **least-outstanding dispatch** (not round-robin): `min_by_key((outstanding, requests))` with increment-on-acquire / decrement-on-release, all under the mutex; no lock held across network I/O (`main.rs:2454-2486`). The slow localhost node organically receives fewer batches.
- Confirmed **no deadlock/hang**: main thread drops its sender post-spawn; workers drop clones on exit (queue-empty / stop / send-fail), so the receiver loop terminates.
- Confirmed **zero-knob rejection precedes index open** (`main.rs` `emit_ingest` guards run before `embed_chunks_payload`), matching `ingest_embed_chunks_rejects_zero_pool_knobs_before_opening_index`.
- I did **not** re-run tests (review-only). I relied on the provided validation log (fmt/clippy/test-workspace all green) plus source reading; the new public `EmbeddingConfig.base_urls` field compiling cleanly across the workspace is consistent with that log.

The implementation satisfies every non-negotiable: 3-endpoint build-time pool, single-endpoint query-time, transport-only fingerprint, least-outstanding dispatch, and resumable (durable batches + finalize-after-coverage). The findings above are doc-accuracy reconciliation (M1/M2), efficiency/robustness follow-ups (L3), and defensive/test cleanups (L4–N6) — all compatible recommendations, none blocking.

VERDICT: GO
