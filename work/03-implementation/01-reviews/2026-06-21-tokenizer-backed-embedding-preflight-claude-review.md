# Claude Review: tokenizer-backed embedding preflight (Phase 1.2)

Verdict: GO

Scope reviewed: uncommitted working-tree changes adding a configured Hugging Face
`tokenizer.json` to the embedding preflight — `crates/jurisearch-embed/src/lib.rs`,
`crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`,
`crates/jurisearch-embed/Cargo.toml`, root `Cargo.toml`, `Cargo.lock`, and the plan
delta in `work/03-implementation/IMPLEMENTATION_PLAN.md`. No code was modified.

This slice builds on the prior char/estimated-token preflight (reviewed in
`2026-06-21-embedding-preflight-claude-review.md`) and notably resolves that review's
top suggestion — batch failures now name the offending chunk.

## Phase 1.2 satisfaction

The plan's "Done" line for this slice makes four claims; all four are present and
verified in code:

1. **Preflight can use a configured `tokenizer.json` to count real tokens before any
   request.** `EmbeddingConfig.tokenizer_path` (`lib.rs:77`) is loaded once at client
   construction via `load_tokenizer` (`lib.rs:516-526`, called from
   `OpenAiCompatibleClient::new` `lib.rs:357-362`) and threaded into
   `preflight_input_with_tokenizer` (`lib.rs:207-247`), which runs inside `embed_query`
   *before* the URL is built or the request is sent (`lib.rs:371-372`). Both endpoint
   callers — `search_payload` (`main.rs:430-433`) and the `embed_chunks_payload` loop
   (`main.rs:1415-1421`) — go through `embed_query`, so the tokenizer guard covers every
   embedding call with no bypassable path.
2. **Char / estimated-token budgets remain the default fallback and are still reported.**
   The legacy `preflight_input` delegates with `tokenizer = None` (`lib.rs:203-205`),
   preserving exact prior behavior. The char budget (`max_input_chars`) and
   `estimated_chars_per_token` still apply and are still emitted in both payloads.
3. **`status` and `ingest embed-chunks` expose the active token-count method and tokenizer
   path.** `configured_token_count_method()` (`lib.rs`) plus `tokenizer_path` are added to
   `status_payload` (`main.rs:1638-1639`) and `embed_chunks_payload` (`main.rs:1467-1468`).
4. **Oversized tokenizer-counted inputs fail as bad input with the offending chunk ID in
   batch embedding.** The budget check compares the real token count
   (`tokens > max_tokens`, `lib.rs:239-240`); `InputTooLong` maps to `ErrorObject::bad_input`
   (`main.rs:1929`) and the batch loop wraps it with the chunk id via
   `embedding_error_object_with_context(error, &input.chunk_id)` (`main.rs:1419-1421`).

The new unit test `tokenizer_budget_is_enforced_when_configured` (`lib.rs:671-705`) is the
right kind of proof: it sets `estimated_chars_per_token = 100` and `max_input_chars = None`
so only the tokenizer path can trip, then asserts `tokens == 3` (real) vs
`estimated_tokens == 1` (char estimate) and `token_count_method == Tokenizer`. It connects
to no server (`127.0.0.1:9`), proving rejection happens before the endpoint call.

## Blocking findings

None.

## Non-blocking suggestions

- **[Correctness — recommend] Loaded tokenizer truncation/padding is not explicitly
  disabled.** `load_tokenizer` (`lib.rs:516-526`) returns the tokenizer exactly as
  deserialized and `encode(input, false)` reports `get_ids().len()`. If a supplied
  `tokenizer.json` carries a baked-in `truncation` config (max length), `encode` will
  silently truncate and report the *truncated* count — so a genuinely oversized input would
  pass preflight, defeating the feature's core purpose. The canonical bge-m3 HF
  `tokenizer.json` ships with `truncation: null`/`padding: null`, so the intended path is
  safe and this is why it's not blocking — but the guard is one operator-supplied file away
  from becoming a silent no-op. Hardening: call `tokenizer.with_truncation(None)` and
  `with_padding(None)` after load so the count always reflects the full input regardless of
  the file's embedded config.

- **[Correctness — minor] `add_special_tokens = false` under-counts vs the model.**
  `encode(input, false)` (`lib.rs:212`) excludes the special tokens the endpoint actually
  prepends/appends (bge-m3 / XLM-RoBERTa adds `<s>` … `</s>`, i.e. +2). With
  `max_estimated_tokens` set to the model's true 8192 ceiling, an input of exactly 8192
  content tokens passes preflight but the model sees ~8194 and truncates. In the default
  config the 24 000-char ceiling is the binding guard so this never bites; it only matters
  if an operator disables `max_input_chars` and relies on the token budget alone. Consider
  either `add_special_tokens = true` (count what the model sees) or a short comment that the
  token budget is treated as a content budget with intended headroom.

- **[Consistency — minor] `token_count_method` is derived from config, not from a
  successful load.** `configured_token_count_method()` returns `Tokenizer` whenever
  `tokenizer_path.is_some()` (`lib.rs`), so `status` advertises `"tokenizer"` with a path
  even when that path is missing/unreadable — the failure only surfaces later as
  `TokenizerLoad` on the first embed/search. This keeps `status` cheap and side-effect free
  (the CLI contract test deliberately points at a non-existent `/tmp` path and still
  passes), which is defensible, but an operator reading `status` cannot tell a healthy
  tokenizer config from a broken one. Optional: note in the field name/docs that it reflects
  configured intent, not verified availability.

- **[Dependency — note] `tokenizers` is the right crate; the build footprint grew.** The
  choice is well-made: `tokenizers = { version = "0.23.1", default-features = false,
  features = ["fancy-regex"] }` (`Cargo.toml:34`) correctly avoids the oniguruma `onig` C
  dependency (confirmed absent from `Cargo.lock`) in favor of pure-Rust `fancy-regex`. It
  is also the canonical/only mature crate for parsing HF `tokenizer.json`, so there is no
  lighter equivalent. Trade-off worth recording: it still pulls `esaxx-rs` (C++ via `cc`,
  training-only), `rayon`, `ahash`, `compact_str`, etc. — ~30 transitive crates and a C++
  toolchain requirement at build time. If build cost/portability becomes a concern, gating
  tokenizer support behind a Cargo feature is a future option; not needed now.

- **[Contract — note] New JSON fields are additive; `SCHEMA_VERSION` ("1") is unchanged.**
  Adding `token_count_method` / `tokenizer_path` to the `status` and `embed-chunks` payloads
  is backward-compatible and consistent with how the predecessor slice added budget fields
  without a bump. No external schema doc enumerates these fields (only the contract test
  pins them), so nothing else needs updating — just flagging that downstream consumers must
  tolerate additive fields.

- **[Minor] `parse_optional_path_buf` cannot represent a file literally named `0`/`none`.**
  (`main.rs:1976-1983`) Empty / `none` / `0` resolve to `None`. This mirrors the existing
  `parse_optional_usize` disable convention, so it's the right call for consistency; just
  noting the (negligible) edge that those exact filenames can't be configured.

## Verification notes

- `cargo test -p jurisearch-embed --lib` — 11 unit tests pass, including the two new ones
  (`tokenizer_budget_is_enforced_when_configured`, `tokenizer_load_error_names_path`) and
  the unchanged legacy guards (`oversized_input_is_rejected_before_endpoint_call`,
  `estimated_token_budget_is_enforced`).
- `cargo test -p jurisearch-cli --test cli_contract -- status` — 5 pass, including
  `status_returns_json_without_index` (asserts default `token_count_method ==
  "estimated_chars"` and null `tokenizer_path`) and
  `status_reports_embedding_budget_env_overrides` (asserts the env override flips the method
  to `"tokenizer"` and echoes the path).
- `cargo clippy -p jurisearch-embed --all-targets` — clean, no new lints.
- `cargo clippy -p jurisearch-cli --bin jurisearch` — clean.
- Regression check: the `EstimatedChars` branch of `EmbeddingInputStats::Display`
  (`lib.rs:297-326`) reproduces the prior human-readable format byte-for-byte, and the two
  added public struct fields (`tokens`, `token_count_method`) are additive within the
  workspace (the struct is constructed only in `lib.rs`); existing consumers reading
  `estimated_tokens` are unaffected.
- Confirmed `Cargo.lock` no longer references `onig`; `tokenizers` resolves with
  `fancy-regex` as the regex backend.
