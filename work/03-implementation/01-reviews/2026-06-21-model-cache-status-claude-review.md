I've completed a thorough inspection of the live diff, the referenced code paths (the embed-crate guard, error/exit-code mapping, URL classification, schema), and independently re-ran the key new tests (all pass). Here is the review.

---

# Code Review — Phase 1.6 Model Cache and Status Checks

- **Date:** 2026-06-21
- **Reviewer:** Claude (Opus 4.8), automated code review
- **Scope:** Uncommitted diff in `/home/pierre/Work/jurisearch` for the Phase 1.6 model-cache/status slice: `JURISEARCH_MODEL_DIR` cache contract, `jurisearch status` cache + loopback endpoint reporting, `setup` / `model fetch` CLI + JSONL behavior, the in-process missing-model guard on dense/embed paths, and the schema/contract/plan updates.
- **Files reviewed:** `crates/jurisearch-cli/Cargo.toml`, `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `crates/jurisearch-core/src/contract.rs`, `crates/jurisearch-core/src/schema.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`, plus `crates/jurisearch-embed/src/lib.rs` (guard + URL classification).

## Summary

The slice is well-constructed and internally consistent. The cache-state machine (`not_required` / `ready` / `missing`), the `model fetch` / `setup` payloads, JSONL routing, and the loopback-only endpoint probe all line up with the schema, the contract status flips, and the plan notes. SSRF is correctly avoided: the probe only runs when the URL classifies as loopback, and classification + probe use the same `url::Url::parse` host, so userinfo tricks (`http://127.0.0.1@evil.com/`) classify as hosted and are never dialed. Independently re-ran the four key tests — all pass.

No correctness, contract, security, or coverage issue rises to blocking. Findings below are advisory.

## Findings (by severity)

### Low

1. **Divergent exit codes for the same condition: "in-process model missing."**
   `model fetch` without `--allow-download` returns `bad_input` → exit **2** (`main.rs:2416-2425`), while the dense-search / embed-chunks guard surfaces `MissingLocalModel`, which `embedding_error_object`'s catch-all maps to `dependency_unavailable` → exit **4** (`main.rs:3197-3206`, guard at `main.rs:506`, `main.rs:1672`). Both are defensible (user-fixable input vs. unmet local dependency), but an agent scripting against exit codes sees two different codes for one root cause. Worth a one-line contract note, or aligning intentionally.

2. **CLI-level wiring of `ensure_embedding_runtime_ready` is not exercised by an integration test.**
   The guard logic itself is unit-tested in the embed crate (`lib.rs:802-807`), but the new calls that wire it into the dense-query and `embed-chunks` paths (`main.rs:506`, `main.rs:1672`) have no CLI test asserting that a configured in-process provider with a missing cache actually blocks a search/embed before client construction. A full test needs a managed Postgres index, so this is understandably heavier — but the wiring (which model_present value is threaded, and that it runs *before* the client is built) is currently unverified end-to-end. Low risk given the one-line shape; flagging as a coverage gap.

3. **`status` / `setup` now perform network I/O; DNS for `localhost` is outside the timeout budget.**
   `loopback_endpoint_reachable` bounds the TCP connect to 250 ms (`main.rs:2271`, `LOOPBACK_ENDPOINT_CONNECT_TIMEOUT`), but `(host, port).to_socket_addrs()` resolution is not covered by that timeout. For literal `127.0.0.1` / `::1` this is instant; for a `localhost` domain it depends on the resolver. In practice negligible and the behavior is documented in the plan, but `status` going from pure-read to network-touching is a behavior change worth keeping in mind.

4. **Cache readiness is presence-only — no content/integrity check.**
   `model_cache_status` and `model fetch` treat any `model.onnx` / `tokenizer.json` that `is_file()` as `ready` / `already_cached` (`main.rs:2128-2132`). The tests stage `b"placeholder"` and `b"{}"` and get `ready`. That's correct for the current pre-staging contract, but when the real `--allow-download` backend lands (the documented "Remaining" item), it must validate size/hash so a truncated download isn't reported as ready. Recommend tracking that requirement with the download work.

### Informational / nits

5. **`model_cache_key` collisions and `..` handling** (`main.rs:2188-2203`): `/` and `\` both map to `__`, so `a/b` and `a__b` collide; `..` maps to `..`. I verified there's **no path traversal** — every separator is replaced, so the key is always a single component, and a `..` key resolves to `model_dir` itself (harmless stat). Collisions are improbable for real model ids; no action needed beyond awareness.

6. **Doc/schema completeness nits:**
   - `IMPLEMENTATION_PLAN.md:691` documents the default as `~/.cache/jurisearch/models` and the `JURISEARCH_MODEL_DIR` override but omits the `XDG_CACHE_HOME/jurisearch/models` layer that `model_cache_dir` actually checks (`main.rs:2152-2160`).
   - `SetupResponse.embedding` is typed as a bare `{ "type": "object" }` (`schema.rs`), whereas `StatusResponse.embedding` fully specifies `model_cache` / `endpoint` via `$ref`. Both payloads are produced by the same helpers, so the setup schema is under-describing a known shape. Cosmetic.

## Recommendations

- Non-blocking: decide whether the missing-model exit code should be unified (finding 1), and add a short note to the contract either way.
- Non-blocking: when the download backend is implemented, add a CLI integration test for the dense/embed guard (finding 2) and content validation for cached files (finding 4).
- Optional: update the plan's model-dir precedence to mention `XDG_CACHE_HOME`, and tighten `SetupResponse.embedding` to match `StatusResponse` (finding 6).

## Verification performed

- Re-ran `model_fetch_and_setup_report_in_process_model_cache`, `status_reports_loopback_endpoint_reachability`, `status_reports_in_process_embedding_config_file`, `help_schema_json_is_valid_and_lists_commands` → **4 passed, 0 failed**.
- Confirmed schema `$ref` targets (`ModelCacheStatus`, `EmbeddingEndpointStatus`) exist, `EmbeddingProvider` serializes to the schema's enum spellings, `url` is a real workspace dependency (`Cargo.toml:37`, lock entry added), and the SSRF-avoidance path (classify-then-probe using the same parsed host) holds.
- Confirmed no stale `not_implemented` tests remain for `model` / `setup`, and `related` / `ingest` / `sync` are still stubbed.

---

**Verdict: GO** — acceptable to commit. No blocking correctness, contract, security, or coverage defect; all findings are advisory and can be addressed alongside the documented "Remaining" download-backend work.
