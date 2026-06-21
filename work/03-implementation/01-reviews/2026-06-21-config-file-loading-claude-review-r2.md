I've verified the full diff, the live source, and cross-checked each prior R1 finding against the current code. Here is my review.

---

# Review R2 — Phase 1.6 Embedding Config File Loading

- **Date:** 2026-06-21
- **Reviewer:** Claude (Opus 4.8), automated diff review
- **Scope:** Uncommitted diff in `/home/pierre/Work/jurisearch` — TOML runtime config loading for embedding settings (`JURISEARCH_CONFIG` / `$XDG_CONFIG_HOME` / `~/.config`), `JURISEARCH_EMBED_*` env override layering, secret redaction in `jurisearch status`, and `IMPLEMENTATION_PLAN.md` status note. Files: `Cargo.toml`, `Cargo.lock`, `crates/jurisearch-cli/Cargo.toml`, `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`.
- **Verification:** Inspected the live diff and the surrounding source (`main.rs:1788-2090`, `cli_contract.rs` additions). Trusted Codex's reported `cargo fmt`/`clippy`/`test` runs (not re-run here).

## Summary

This R2 diff closes the R1 blocker and addresses every non-blocking R1 finding. The prior secret leak is gone: `config_error` is now produced exclusively by `toml_parse_error_message` (`main.rs:2063-2073`), which **never** renders `toml::de::Error`'s `Display` — it emits only the config path plus a line/column derived from `error.span()`. The offending source line (and therefore any `api_key` value) can no longer reach `status` output. A regression test exercises the malformed-`api_key` case across **both** the one-shot `status` and the JSONL `session status` surfaces, asserting neither the token nor the literal `api_key` appears in stdout.

The layering contract is intact and consistent: file config seeds defaults, then `JURISEARCH_EMBED_*` env vars override as the final layer, with in-process providers clearing `base_url`/`api_key` on every path. Remaining observations are minor/forward-looking and do not block commit.

## R1 finding disposition

| R1 finding | Status in R2 | Evidence |
|---|---|---|
| #1 BLOCKER — parse error leaks `api_key` | **Fixed** | `toml_parse_error_message` (`main.rs:2063`) uses span→line/col only; regression test `status_malformed_embedding_config_does_not_leak_api_key` covers `status` + JSONL |
| #2 `0` budget semantics differ from env | **Fixed** | `nonzero_usize` applied to file `max_input_chars`/`max_estimated_tokens` (`main.rs:1940-1945`); `in_process` test asserts `0` → `null` |
| #3 No `deny_unknown_fields` | **Fixed** | `#[serde(deny_unknown_fields)]` on both structs (`main.rs:1803`, `1809`) |
| #4 Provider aliases rejected in file | **Fixed** | `deserialize_embedding_provider_option` (`main.rs:2036`) routes through the shared `parse_embedding_provider`; `in_process` test uses `provider = "local"` |
| #5 `JURISEARCH_CONFIG` trim/doc nits | **Addressed** | Path now built from `trimmed` (`main.rs:1886`); plan documents `none`/`0`/empty |
| #6 `in_process` + `api_key` re-set | **Fixed** | `clear_unused_in_process_secret_fields` called at end of both apply paths (`main.rs:1963`, `2016`); test asserts `unused-local-secret` absent |

## Findings (by severity)

### 1. [Low — forward-compat] `deny_unknown_fields` on the *root* table makes the whole embedding config brittle to future sections
`crates/jurisearch-cli/src/main.rs:1803-1805`

`RuntimeConfigFile` currently has only `embedding`, with `deny_unknown_fields`. That's correct for the R1 ask, but note the consequence: the day a user (or a later slice) adds any sibling top-level section to the same file (e.g. `[storage]`, `[search]`), an older binary rejects the **entire** file — `config_loaded` flips to `false` and embedding config silently falls back to defaults+env, with a "TOML syntax error" the user can't easily interpret. `deny_unknown_fields` on the inner `[embedding]` table is clearly desirable (catches `dimention` typos); on the root table it's a coarser tradeoff. **Recommendation (non-blocking):** keep it for now, but when a second config section appears, relax the root table (e.g. tolerate/ignore unknown sections, or split per-section parsing) so one subsystem's keys can't disable another's.

### 2. [Nit] `JURISEARCH_CONFIG` is decoded via `to_string_lossy`, corrupting non-UTF-8 paths
`crates/jurisearch-cli/src/main.rs:1879-1888`

The R1 trim nit is resolved by building the path from `trimmed`, but doing so requires `to_string_lossy()`, which replaces non-UTF-8 bytes with U+FFFD. On Linux a config path can be arbitrary bytes, so a non-UTF-8 `JURISEARCH_CONFIG` would now silently point at a different (lossy) path and fail to open rather than reading the intended file. Extremely rare in practice. **Recommendation (non-blocking):** if you want to be strict, special-case the disable tokens on the `OsStr` and otherwise build the `PathBuf` directly from the original `OsString` (trimming only ASCII whitespace), preserving exact bytes for real paths.

### 3. [Nit] `base_url` overrides an explicit `provider` (file and env)
`crates/jurisearch-cli/src/main.rs:1921-1924` (and env at `1976-1981`)

A file containing both `provider = "in_process"` and a non-empty `base_url` ends up as `OpenAiCompatible`: the provider handler clears the URL, then the `base_url` handler re-sets it and flips the provider back. This is internally consistent with env semantics (`JURISEARCH_EMBED_BASE_URL` also forces `OpenAiCompatible`), so it's defensible, but a user who explicitly wrote `provider = "in_process"` may be surprised that a stray `base_url` silently wins. **Recommendation (non-blocking):** either let an explicit `provider` take precedence, or document that `base_url` implies the OpenAI-compatible provider.

### 4. [Nit] Minor test/coverage gaps
`crates/jurisearch-cli/tests/cli_contract.rs`

The new tests are solid. Two small gaps worth closing opportunistically, neither blocking:
- No test asserts that an **unknown key** (now rejected by `deny_unknown_fields`) yields `config_loaded == false` with a source-free `config_error` — the plan claims this semantics but only the syntax-error path is exercised.
- No test for `JURISEARCH_EMBED_PROVIDER=in_process` + `JURISEARCH_EMBED_API_KEY=...` confirming the env secret is cleared via `clear_unused_in_process_secret_fields` (`main.rs:2016`). The code path is correct; it's just unguarded by a test.

### 5. [Nit] `base_url` credentials are not redacted (pre-existing, out of slice scope)
`crates/jurisearch-cli/src/main.rs` status payload (`base_url` field)

`api_key` is correctly never serialized into `status`. `base_url`, however, is printed verbatim, so a user-supplied URL of the form `https://user:secret@host/v1` would surface credentials. This is pre-existing, consistent with the env path, and `base_url` is treated as non-secret config throughout — noting only for completeness, not as a defect of this slice.

## Positives

- **Blocker truly closed by construction:** `config_error` is *only* ever set from `toml_parse_error_message` (source-free) or an `io::Error` read failure (no file contents). There is no remaining code path that renders `toml::de::Error`'s `Display`, so the leak cannot recur for syntax errors, type errors, *or* `deny_unknown_fields` rejections.
- Regression test covers both surfaces (`status` and JSONL `session status`) and asserts absence of both the secret and the literal `api_key`.
- `0`/unbounded budget semantics now match env exactly; the `in_process` test pins `max_*=0 → null`.
- Provider spellings are unified across env and file through one `parse_embedding_provider`; the custom deserializer surfaces an actionable (and secret-free) "unsupported embedding provider" message.
- In-process secret hygiene is enforced defensively at the end of both apply paths, so ordering of keys within the file can't leave a stale `api_key`/`base_url`.
- Test harness scrubs all ambient `JURISEARCH_*`/`XDG_CONFIG_HOME` and pins `JURISEARCH_CONFIG=none` for deterministic default-path tests.
- `IMPLEMENTATION_PLAN.md` note is accurate, records the new semantics (including the `0`-means-unbounded and alias details), and scopes the genuinely remaining work (model cache/fetch, reachability checks).

## Recommendations

1. None blocking. Land the slice.
2. Opportunistically: relax root-table `deny_unknown_fields` before a second config section ships (#1); preserve exact bytes for non-UTF-8 `JURISEARCH_CONFIG` (#2); decide/document `provider` vs `base_url` precedence (#3); add the two small tests in #4.

## Verdict: GO

The R1 blocker — `api_key` leaking through the rendered TOML parse error into `status` — is fixed at the source (location-only diagnostics, no `Display` rendering anywhere) and is covered by a regression test on both the CLI and JSONL surfaces. All five non-blocking R1 findings are also resolved. The remaining items are forward-compat considerations and nits that can be folded in later. The slice meets its acceptance criterion that secrets are never written into status output, and is acceptable to commit.
