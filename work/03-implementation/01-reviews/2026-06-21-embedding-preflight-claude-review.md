# Claude Review: embedding preflight

Verdict: GO

## Findings

- **[Correctness — PASS] Preflight is centralized at the single network chokepoint.**
  `jurisearch-embed/src/lib.rs:296-297` — `preflight_input` runs inside
  `OpenAiCompatibleClient::embed_query`, after `ensure_matches_index` and before the
  URL is built or `send_json` is issued. Both embedding callers — `search_payload`
  (`jurisearch-cli/src/main.rs:367-369`) and the `embed_chunks_payload` loop
  (`main.rs:569-571`) — go through `embed_query`, so every endpoint call is covered by
  one guard with no duplicated/bypassable path. The unit test
  `oversized_input_is_rejected_before_endpoint_call` (`lib.rs:520-544`) proves the guard
  fires without any server listening (base_url is `127.0.0.1:9`, never connected), which
  is the correct way to assert "before the endpoint call."

- **[Correctness — PASS] Budget arithmetic is sound and defensive.**
  `lib.rs:185-206` — `chars = input.chars().count()` (Unicode scalar values, the right
  unit for a char ceiling), `chars_per_token = self.estimated_chars_per_token.max(1)`
  guards division-by-zero even though every constructor and the env parser already reject
  0, and `chars.div_ceil(chars_per_token)` rounds tokens up (conservative). Rejection uses
  `Option::is_some_and`, so a `None` ceiling correctly means "unbounded" rather than
  "always reject." `estimated_token_budget_is_enforced` (`lib.rs:546-568`) confirms
  `div_ceil(5,2) == 3 > 2` trips the token path with char checks disabled.

- **[Error mapping — PASS] `InputTooLong` is classified as caller-data error, not upstream.**
  `main.rs:836` maps `EmbeddingError::InputTooLong(_)` to `ErrorObject::bad_input`, distinct
  from the `Endpoint/InvalidResponse/EmptyResponse` → `upstream_unavailable` arm
  (`main.rs:837-839`). This is the right semantics: an oversized chunk/query is a client-side
  data problem, and the error text (`lib.rs:409-412`) is actionable — it names both the split
  remedy and the two tuning env vars.

- **[Metadata — PASS] Budget is surfaced in both status and ingest output, using config not fingerprint.**
  `status_payload` (`main.rs:770-772`) and `embed_chunks_payload` output (`main.rs:614-616`)
  both emit `max_input_chars`, `max_estimated_tokens`, `estimated_chars_per_token`. Correctly
  sourced from `embedding_config` rather than the fingerprint — the budget is an operational
  knob and is deliberately *not* part of the index fingerprint, so it does not perturb
  `storage_embedding_fingerprint()` or `ensure_matches_index`. The CLI contract test
  (`cli_contract.rs:60-62`) pins the three defaults (24000 / 8192 / 4).

- **[Env config — PASS] `parse_optional_usize` semantics are consistent with the existing env style.**
  `main.rs:866-872` — `"none"`/`"0"` disable a ceiling (`Some(None)`), a valid integer sets it
  (`Some(Some(n))`), and an unparseable value yields `None` so the outer `.unwrap_or(default)`
  (`main.rs:674-675,678-679`) silently keeps the default. That silent-fallback-on-garbage
  behavior matches the sibling vars (`JURISEARCH_EMBED_DIMENSION`/`NORMALIZE` at
  `main.rs:665,668`). `estimated_chars_per_token` is parsed via a let-chain that rejects 0 and
  non-integers (`main.rs:681-684`). Edition is `2024` (`Cargo.toml:12`), so the `&& let` chains
  and `is_some_and`/`div_ceil` are all valid.

- **[Plan accuracy — PASS] The plan delta matches the code.**
  `IMPLEMENTATION_PLAN.md:391-392` — the new "Done" line accurately claims configurable
  ceilings via the three env vars plus status/embed-chunks recording the active budget, all of
  which are present. The replacement "Remaining for Phase 1.2 hardening" line correctly frames
  the char/chars-per-token heuristic as conservative pending tokenizer-grade splitting, which is
  an honest description of the shipped approximation rather than an overclaim.

## Suggestions

- **[Low — operability] Batch preflight failure does not identify the offending chunk.**
  `main.rs:568-571` — in the `embed_chunks_payload` loop, an oversized chunk aborts the whole
  run with `InputTooLong`, but `EmbeddingInputStats` carries only char/token counts, not the
  `chunk_id` (which is in scope as `input.chunk_id`). The surfaced message says "split the
  document chunk" yet gives the operator no way to find *which* chunk. Consider wrapping the
  loop error to append `input.chunk_id` (and/or document length), so the batch failure is
  actionable. Non-blocking: the plan explicitly defers tokenizer-grade splitting to Phase 1.2,
  and fail-fast on the full corpus is acceptable for this slice.

- **[Info] The two default ceilings are partly redundant.**
  With the defaults (`lib.rs:11-13`) a 24000-char input estimates to 24000/4 = 6000 tokens,
  comfortably under the 8192 token ceiling, so at default config the char budget always trips
  first and `max_estimated_tokens` is never the binding constraint. This is a deliberately
  conservative design (the char limit is stricter than the model's true 8192-token sequence
  limit), not a bug — but worth a one-line comment noting the char budget is intentionally the
  primary guard so a future editor doesn't "fix" the apparent slack by raising it to ~32k chars.

- **[Low — test coverage] No direct test for the env-var override path.**
  `parse_optional_usize` (`"none"`/`"0"`/garbage) and the propagation of
  `JURISEARCH_EMBED_MAX_*` into `embedding_config_from_env` are exercised only indirectly via
  the pinned defaults in the status contract test. A small unit test over `parse_optional_usize`
  and a status assertion with the env vars set would lock in the disable-vs-default behavior.
  Non-blocking.

## Verification Notes

- `git show b02db2c --stat` / full diff — reviewed all four changed files against working tree.
- `cargo test -p jurisearch-embed` — 9 unit tests pass (incl.
  `oversized_input_is_rejected_before_endpoint_call`, `estimated_token_budget_is_enforced`); the
  live endpoint test stays `ignored` as designed.
- `cargo test -p jurisearch-cli --test cli_contract status_returns_json_without_index` — passes,
  confirming the new `max_input_chars`/`max_estimated_tokens`/`estimated_chars_per_token` status
  fields.
- `cargo clippy -p jurisearch-embed` — clean, no new lints.
- Confirmed `edition = "2024"` in `Cargo.toml:12`, so the let-chain env parsing and
  `is_some_and`/`div_ceil` usages compile as written.
