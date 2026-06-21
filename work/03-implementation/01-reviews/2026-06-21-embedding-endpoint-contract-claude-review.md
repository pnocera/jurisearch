VERDICT: GO

# Review — Embedding Endpoint Contract (commit eb5d29d)

The implementation faithfully realizes plan task 0.4 and DESIGN §11.2/§14. The OpenAI-compatible request/response handling, fingerprint and dimension enforcement, base-url classification, in-process placeholder gating, and the `status`/schema contract are all correct and well-tested. Dependency choice is appropriate. Marking 0.4 complete is justified. No blocking issues found.

## What was verified

**OpenAI request/response correctness** — `embed_query` POSTs `{"model","input"}` JSON to `{base_url}/embeddings` (trailing-slash-trimmed), so `…/v1` + `/embeddings` yields the correct `…/v1/embeddings` (asserted by the in-test server, `lib.rs:402`). Response deserializes `data[0].embedding`; unknown fields (`usage`, `object`, `index`) are tolerated since `deny_unknown_fields` is not set. Non-2xx is surfaced via ureq's `Error::Status` → `EmbeddingError::Endpoint`. `EmptyResponse` covers an empty `data` array. The single-string `input` and float `encoding_format` default match the verified llama.cpp endpoint.

**Fingerprint/dimension enforcement** — `ensure_matches_index` compares the full configured fingerprint against the index `expected` and runs *before* any network call (`fingerprint_mismatch_fails_before_endpoint_call`). Returned vector length is then hard-checked against `expected.dimension` with an actionable error containing `rebuild/re-embed` (`wrong_dimension_is_actionable_error`). Fingerprint fields (provider, base_url-class, model, dimension, normalize, pooling) exactly match DESIGN §11.2 line 473. Secrets are correctly excluded from `EmbeddingFingerprint`/`EmbeddingManifest`; `api_key` is only used for the `Authorization` header, with a `"no-key"` sentinel treated as no-auth — good W7 hygiene.

**Endpoint/base-url classification** — `127.x`, `localhost`, `[::1]` over http/https → `LocalLoopback`, else `Hosted`; the provider stays `openai_compatible` regardless, so a `127.0.0.1` endpoint is correctly treated as a configured remote endpoint, not an in-process shortcut (`loopback_endpoint_is_classified_as_configured_remote_provider`).

**In-process placeholder** — `OpenAiCompatibleClient::new` rejects `InProcess` with `UnsupportedProvider`; `ensure_in_process_ready` refuses a missing local model unless `model_present` or `allow_download` (`in_process_mode_refuses_missing_model_without_explicit_permission`). This is a guard/placeholder only, matching the task scope.

**Status/schema contract** — `status_payload` emits all nine embedding fields; `schema.rs` `StatusResponse.embedding` enumerates them with matching enums (`provider`, `base_url_class`). `cli_contract.rs::status_returns_json_without_index` pins provider/class/model/dimension/pooling/provisional/reembeddable. `provider` serde rename (`openai_compatible`) is consistent across enum, schema, and test.

**Dependency choice** — `ureq 2.12.1 (json)` is a synchronous client (no async runtime pulled in; the `tokio` in the lockfile is transitive via `postgres`, not this crate). It brings pure-Rust TLS (`rustls` + `ring` + `webpki-roots`), no `openssl`/`native-tls` system dependency — a good fit for the local-first, CLI-only, portable posture.

**0.4 completion** — every task and acceptance bullet maps to code + a test: client, hosted/loopback URLs, provisional `bge-m3`, full fingerprint, hard mismatch failures, llama.cpp doc + ignored live test, in-process placeholder, and `reembeddable: true`. Justified.

## Suggestions / Recommendations (non-blocking, may be applied without re-review)

1. **No request timeouts** (`lib.rs:215`). The default `ureq` agent sets no connect/read timeout, so a hung or slow endpoint blocks a one-shot call or a warm JSONL `session` indefinitely. Build the request through an `ureq::AgentBuilder` with explicit connect/read timeouts.

2. **HTTP error detail is dropped** (`lib.rs:230`). `EmbeddingError::Endpoint(error.to_string())` yields only e.g. `http status: 400`; the server body (model-not-found, bad pooling, etc.) is lost. On `ureq::Error::Status(code, resp)`, read `resp.into_string()` into the error message for actionable diagnostics.

3. **`provisional` recompute semantics** (`main.rs:402`). Overriding `JURISEARCH_EMBED_MODEL` to anything other than `bge-m3` sets `provisional = false`, which reads as "finalized model" even though no Phase-1 eval has confirmed it. In Phase 0 *every* embedding model is pre-1.7-provisional. Consider keeping `provisional = true` for any non-eval-confirmed model, or splitting the concept into `is_benchmark_default` vs `eval_confirmed`. The default (`bge-m3`) path correctly reports `provisional = true`, so this only affects manual overrides.

4. **`base_url_class` uses prefix matching** (`lib.rs:266-279`). `http://localhost.attacker.example` or `http://127.0.0.1.attacker.example` would be misclassified as `LocalLoopback` because of `starts_with`. Since `base_url_class` is part of the fingerprint and shown in `status`, prefer parsing the host (the `url` crate is already a transitive dep via ureq) and matching the exact host/IP. Low severity — provider classification is unaffected.

5. **`normalize` is declared but not enforced** (`lib.rs`). `normalize: true` is recorded in the fingerprint, but the production client neither verifies nor applies normalization to returned vectors; an endpoint returning un-normalized vectors would be stored silently under a "normalized" fingerprint. The live test checks `l2_norm ≈ 1.0`, but the runtime path does not. Consider verifying `l2_norm ≈ 1.0` (or normalizing client-side) when `normalize == true`.

6. **Doc port drift.** DESIGN §14's config example uses `:8080`, while `embeddings-endpoint.md` and the live test use `:8097`. Harmless, but aligning the example avoids confusion.
