I've reviewed the full diff against the source context. Here is my assessment.

## 1. Findings (by severity)

### F1 — Medium · Truncation bounds only the char budget, but preflight also enforces the token budget
`embedding_request_text` (`main.rs:2664`) truncates outbound text to `max_input_chars` only. But `embed_batch` still runs `preflight_input_with_tokenizer` (`lib.rs:404` → `:254-262`), which rejects on **either** `chars > max_input_chars` **or** `tokens > max_estimated_tokens`. Both are set by default (`lib.rs:105-107`, `126-128`): `max_input_chars = 24_000`, `max_estimated_tokens = 8_192`.

Consequences when the two budgets are inconsistent:
- If a **tokenizer** is configured (`JURISEARCH_EMBED_TOKENIZER_JSON`, `main.rs:3044`) — which the comment at `lib.rs:16-17` explicitly anticipates ("a configured tokenizer can apply the endpoint-specific token budget exactly") — preflight counts *real* tokens. A chunk truncated to 24,000 chars of token-dense French legal text can still exceed 8,192 tokens.
- If an operator sets `JURISEARCH_EMBED_MAX_INPUT_CHARS=none|0` (`parse_optional_usize`, `main.rs:4498` → `Some(None)`) while leaving `max_estimated_tokens` at 8,192, truncation becomes a **no-op** (`embedding_request_text` returns the input unchanged when `max_input_chars` is `None`) yet the token check still fires.

In both cases the result is `EmbeddingError::InputTooLong`, which is **not** in `retryable_embedding_error` (`main.rs:2649`) and maps to `bad_input` (`main.rs:4454`) → the batch hard-fails and the run aborts. That is precisely the "restart and continue" robustness this change is meant to provide, so the truncation is only as robust as the config consistency.

For the **default / currently-described run** (estimated chars, no tokenizer) this is safe: 24,000 chars ÷ 4 chars/token = 6,000 estimated tokens, a ~36% margin under 8,192, so truncated inputs always clear preflight. Not blocking, but the truncation should ideally bound by whichever budget preflight will actually apply, or at minimum the docs/comment should state the safety relies on `max_input_chars ≤ max_estimated_tokens × estimated_chars_per_token`.

### F2 — Low · All `Endpoint` errors are treated as transient
`retryable_embedding_error` (`main.rs:2649`) retries every `Endpoint` and `InvalidResponse`. Several `Endpoint` errors are deterministic, not transient: post-truncation context-length rejections, `401/403` auth failures, and model-not-found all flow through `endpoint_error` (`lib.rs:520`) or the new 200-error path (`lib.rs:439`) as `Endpoint`. These get retried 3× (≈750 ms of `thread::sleep` backoff) before the inevitable abort. Harmless to correctness, mild wasted wall-clock; acceptable given the classification can't cheaply distinguish 529 from 400.

### F3 — Low · `embed_batch_with_retries` post-loop arm is unreachable; panics if `MAX_ATTEMPTS == 0`
In `main.rs:2630-2647`, the retry arm's guard requires `attempt < MAX_ATTEMPTS`, so the final attempt always exits via the catch-all `Err(error) => return Err(error)`. The loop therefore never falls through to `Err(last_error.expect(...))`, making `last_error` an effectively write-only accumulator and the final line dead code. With `EMBEDDING_ENDPOINT_MAX_ATTEMPTS = 3` this is fine, but if that constant were ever set to `0`, the empty range falls through and `last_error.expect(...)` panics. Latent footgun; cosmetic today.

### F4 — Low/Info · Body bounding is not applied uniformly
The new HTTP-200 error path bounds the echoed body to 1,000 chars via `truncate_response_body` (`lib.rs:439-442`, `444-450`), satisfying the "don't leak full huge response bodies" constraint. The pre-existing HTTP-status path `endpoint_error` (`lib.rs:520-533`) still embeds the **full untrimmed** `body` in the message. Not introduced by this diff, but it's the adjacent path and contradicts the same stated constraint — worth bounding it with the same helper for consistency.

### F5 — Info · `into_json` → `into_string` buffers the whole body
`lib.rs:433` now reads the full response into a `String` before parsing (needed to inspect for error-shape). ureq's `into_string` caps at ~10 MB; current batches (≤128 × 1024 f32 ≈ <2 MB JSON) are well under, so fine. If batch sizes grow toward producing >10 MB responses, this would surface as a spurious `InvalidResponse` (and then get retried 3×). Flag for future batch-size tuning.

## 2. Open questions / residual risks
- **Does the motivating 72,124-char chunk, truncated to 24,000 chars, actually pass OpenRouter's bge-m3 8,192-token limit?** This is the real-data question behind F1. If any LEGI chunk's first 24,000 chars tokenize to >8,192 (i.e. denser than ~2.93 chars/token), it returns the error-shaped 200 → `Endpoint` → retried 3× → run aborts. Recommend one live confirmation that the largest real chunks succeed post-truncation before declaring the full corpus run robust.
- **Coverage gap:** the replaced test removed the only CLI-level coverage of the over-budget hard-fail, and no test exercises "truncated but still over the *token* budget" (F1). Lib-level `InputTooLong` tests (`lib.rs:762+`) remain, so the path itself is still tested at the unit level.
- `embedding_inputs_truncated` is **per-run**, not corpus-cumulative across resumes (already-embedded chunks are skipped and not recounted). Expected, but operators reading the count should know it reflects only this run.

## 3. Verification notes
- **Truncation math** (`embedding_request_text`): verified UTF-8-boundary-safe and off-by-one-correct — keeps exactly `max_input_chars` chars, no truncation at exactly the limit, truncates at limit+1. Test inputs `abcde→abcd`, `alpha→alph`, `beta→beta` (untouched) all consistent.
- **`truncate_response_body`**: boundary-safe; exact-1000 yields no ellipsis, 1001+ appends `...`; bounded to ~1,000 chars (≤~4 KB). No input text and no API key appear in any new message.
- **Error-shape detection**: `OpenAiEmbeddingErrorResponse` requires field `error`, so normal success bodies fail to deserialize and fall through correctly; `{"error":null,...}` success bodies are handled by the `!is_null()` guard.
- **Identity preserved**: `expected_fingerprint` and `request_model` untouched; no `CANONICAL_SCHEMA_VERSION`/`CLI_CODE_VERSION` bump; truncation only affects the outbound request text, never stored chunk text/IDs. Canonical `bge-m3:1024:normalize:true` intact. Tests assert the OpenRouter `"model":"baai/bge-m3"` alias and per-endpoint counts.
- **Concurrency**: during retry backoff the endpoint stays `outstanding` (release happens after `embed_batch_on_endpoint` returns), so least-outstanding dispatch correctly routes other workers away from a flaky endpoint. The single caller of `release_embedding_endpoint` was updated for the new `truncated_inputs` parameter.
- Relied on the stated PASS for the live/Postgres-gated tests and `clippy -D warnings` (let-chain at `lib.rs:435` is fine on the nightly 1.96 toolchain in use).

None of the findings are defects in the code as written for the default configuration and the described run; F1 is a real but conditional robustness gap whose safety currently rests on the internally-consistent default budgets, and the only thing I'd confirm against live data is that the largest truncated chunks actually clear the token limit.

VERDICT: GO
