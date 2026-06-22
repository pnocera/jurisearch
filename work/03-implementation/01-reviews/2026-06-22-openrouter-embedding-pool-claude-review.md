I've now verified the full data flow against all six constraints. Here is the review.

## Review: OpenRouter embedding pool support

### 1. Findings (ordered by severity)

No correctness or security blockers found. All findings are non-blocking observations.

**[Low] Precedence is silent when both `JURISEARCH_EMBED_BASE_URLS` and `JURISEARCH_EMBED_POOL` are set** — `main.rs:2252` / `apply_embedding_env_overrides:2903-2917`. When both are set, `config.base_urls` is populated from `BASE_URLS` while `pool_endpoints` is populated from `POOL`; `embedding_endpoint_pool_configs` then dispatches over the pool (`pool_endpoints` non-empty wins) and ignores `base_urls`. But `status`/`setup` JSON still surface `base_urls` alongside `pool`, so the output suggests the `BASE_URLS` endpoints are in play for ingest when they are not. Cosmetic/observability only — consider a one-line log noting the pool supersedes `base_urls`, or documenting the precedence.

**[Low] Same-layer `provider = in_process` + `pool` resolves to `openai_compatible`** — `apply_embedding_file_config:2834-2837,2877` and `apply_embedding_env_overrides:2912-2954`. Setting a pool forces `provider = OpenAiCompatible`, and the trailing `if InProcess { pool_endpoints.clear() }` guard only fires when the provider *ended* as `InProcess`. So an in-process request made in the same config layer as a pool is silently overridden to `openai_compatible`. Cross-layer (env `in_process` after a file pool) correctly clears the pool, so the fail-safe direction is fine. Arguably correct behavior (asking for a pool implies a remote provider), but the asymmetry is worth a comment.

**[Info] `base_url` dedup is string-exact** — `dedupe_embedding_pool_endpoints:2338-2342`. `http://x/v1` and `http://x/v1/` dedup as distinct even though the client trims the trailing slash (`lib.rs:411`), so a trailing-slash typo could double-count one node in the least-outstanding accounting. Negligible impact.

**[Info] Stray tab in test TOML** — `cli_contract.rs` (the `reembeddable = false` config block) has a literal `\t` before the closing `"#`. TOML tolerates it and the test passes; cosmetic.

### Confirmed-correct behaviors (the constraints that matter)

- **Canonical identity preserved.** `fingerprint()` (`lib.rs:154-169`) is built from `model`, not `request_model`; `storage_embedding_fingerprint()` (`lib.rs:277-282`) is `model:dimension:normalize:bool` and excludes `base_url_class`. Stored rows use `embedding_config.model` (`main.rs:2419`) = canonical `bge-m3`. So http(local) vs https(OpenRouter) and the `baai/bge-m3` alias both land as `bge-m3:1024:normalize:true`. The per-endpoint check (`main.rs:2282-2294`) deliberately omits `base_url_class`, which is what allows the remote node in without weakening model/dim/normalize/pooling/storage-fp equality.
- **Alias only on the wire.** `request_model()` (`lib.rs:217-223`) is used solely for the request body (`lib.rs:428`); confirmed by `request_model_alias_does_not_change_stored_fingerprint`.
- **Key isolation is real and fail-closed.** Each endpoint gets its own cloned `EmbeddingConfig` with `api_key` set strictly from its own `api_key_env` (`main.rs:2266-2280`); an endpoint with no `api_key_env` has `api_key = None` and emits no `Authorization` header (`lib.rs:417-424`). A pool endpoint that declares `api_key_env` but resolves empty/unset is rejected (`main.rs:2272-2277`) rather than silently sent unauthenticated. A global `JURISEARCH_EMBED_API_KEY` is *not* propagated to pool endpoints (the else-branch overrides the clone to the endpoint's own `api_key`). The ingest test asserts the local node receives no `authorization:` and OpenRouter receives `bearer openrouter-secret-token`.
- **No secrets in output, and none at rest.** `embedding_pool_endpoints_status_json` (`main.rs:3140-3152`) emits only `api_key_configured: bool` + the env-var *name*. `EmbeddingPoolEndpointConfigFile` (`#[serde(deny_unknown_fields)]`, only `base_url`/`request_model`/`api_key_env`) makes it impossible to inline a literal key in a config file — keys can only come from the environment. Both leak tests pass.
- **Query-time stays single + local.** The query path (`main.rs:757`) calls `embedding_config_from_env()`, which returns `.config` only and discards `pool_endpoints`; nothing sets a top-level `request_model`, so queries hit the single configured `base_url` with `model = bge-m3` and no pool fan-out.
- **Legacy path intact.** `legacy_embedding_pool_endpoints` (`main.rs:2305-2331`) reproduces the old `base_urls`→endpoints behavior with `request_model: None` and the global `api_key`, used whenever no explicit pool is configured.

### 2. Open questions / residual risks

- **`JURISEARCH_EMBED_MODEL` is still a footgun unrelated to this change**: setting it to `baai/bge-m3` (instead of using the pool's `request_model`) would re-stamp the canonical model and change the stored fingerprint. The diff doesn't introduce this, and the docs steer users to `request_model`, but it remains the one way to accidentally break canonical identity. No action required.
- **Phase 2 egress**: the docs correctly flag that OpenRouter egress is acceptable for public LEGI text but must be reconsidered for pseudonymized Judilibre decisions. Tracked in the doc; nothing to enforce in code now.
- **Per-worker client fan-out**: each worker thread builds one client per endpoint (`main.rs:2481-2484`), so `pool_concurrency × endpoints` ureq agents exist concurrently. Cheap and correct; noting for awareness.

### 3. Verification notes

- Read and confirmed the surrounding (non-diff) context for: `fingerprint()`/`storage_embedding_fingerprint()` (excludes `request_model` and `base_url_class`), the auth-header gate and request-body model selection in `OpenAiCompatibleClient::embed`, the per-endpoint fingerprint check, the worker dispatch/least-outstanding logic, and `nonempty_string` (trims, so the `;`/`|`-delimited env parser tolerates whitespace/newlines).
- Traced the query path (`main.rs:756-769`) to confirm the pool cannot reach query-time.
- Relied on the reported PASS results (unit + CLI contract + clippy `-D warnings` + workspace + live OpenRouter probe at dim 1024 / norm ≈ 1.0 / `model BAAI/bge-m3`, and the status smoke showing `secret_leaked=no`). I did not re-run the suite; findings above are from code reading, and they corroborate the passing tests.
- The untracked `...claude-review.md.tmp` is a prior review artifact, outside this diff and out of scope.

### 4. Verdict

The change satisfies all six stated constraints: canonical `bge-m3:1024:normalize:true` identity preserved, `baai/bge-m3` alias on the wire only, `OPENROUTER_API_KEY` scoped to its endpoint with fail-closed handling, no key values in status/output/tests/docs, query-time single local endpoint, and `JURISEARCH_EMBED_BASE_URLS` legacy behavior preserved. The findings are non-blocking polish.

VERDICT: GO
