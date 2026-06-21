Both the CLI and the JSONL session `status` route through `status_payload`, so the leak applies to both surfaces. I have enough to write the review.

---

# Review — Phase 1.6 Embedding Config File Loading

- **Date:** 2026-06-21
- **Reviewer:** Claude (Opus 4.8), automated diff review
- **Scope:** Uncommitted diff in `/home/pierre/Work/jurisearch` — TOML runtime config loading for embedding settings (`JURISEARCH_CONFIG` / `$XDG_CONFIG_HOME` / `~/.config`), `JURISEARCH_EMBED_*` env override layering, secret redaction in `jurisearch status`, and `IMPLEMENTATION_PLAN.md` status note. Files: `Cargo.toml`, `Cargo.lock`, `crates/jurisearch-cli/Cargo.toml`, `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`.
- **Verification:** Built `jurisearch` and ran the binary directly against crafted configs to reproduce behavior. Trusted Codex's reported `cargo fmt`/`clippy`/`test` runs (not re-run here).

## Summary

The layering design is sound and matches the documented contract: file config seeds defaults, `JURISEARCH_EMBED_*` env vars are applied afterward as the final override layer, and `status` exposes useful diagnostics (`config_path`, `config_loaded`, `config_error`). Normal-path secret redaction is correct — `api_key` is never placed into the status JSON, and the new tests prove both a file secret and an env secret are absent from stdout. Test coverage for the happy paths (file load, env-over-file, in-process) is good.

However, there is **one confirmed, in-scope security defect that blocks commit**: a malformed config file leaks the `api_key` into `status` output via the rendered TOML parse error. This directly violates the slice's acceptance criterion "Secrets are never written into manifest or logs."

## Findings (by severity)

### 1. [BLOCKER — Security] TOML parse error leaks `api_key` into `status` stdout
`crates/jurisearch-cli/src/main.rs:1847-1851`

```rust
config_error = Some(format!(
    "failed to parse `{}`: {error}",
    location.path.display()
));
```

`toml::de::Error`'s `Display` renders the **offending source line** as part of the message. When the syntactically-invalid token is on (or within the rendered span of) the `api_key` line, the secret is embedded in `config_error`, which `status_payload` (`main.rs:2123`) writes to stdout — the very surface the slice is supposed to keep secret-free.

Reproduced with the built binary against `$XDG_CONFIG_HOME/jurisearch/config.toml`:

```toml
[embedding]
base_url = "https://embeddings.example.test/v1"
api_key = "super-secret-leaky-token   # unterminated string
model = "custom-embed"
```

`jurisearch status` (exit 0) emitted to **stdout**:

```json
"config_error": "failed to parse `.../config.toml`: TOML parse error at line 3, column 36\n  |\n3 | api_key = \"super-secret-leaky-token\n  |  ... ^\ninvalid basic string\n",
```

`grep` for the token in combined stdout/stderr → **1 match (leaked)**.

This is not a contrived trigger: API keys frequently contain characters that break TOML basic strings (`"`, `\`), and an unescaped/mis-quoted secret is exactly the kind of mistake that produces a parse error *on the `api_key` line*. The same path is reached by the JSONL session `status` command (`session_status_payload` → `status_payload`, `main.rs:866`), where orchestrators are even more likely to capture/log the output.

**Recommendation:** Do not surface the raw `toml::de::Error` `Display` in a shareable field. Options:
- Report only a coarse, source-free diagnostic — e.g. `format!("failed to parse `{}` (TOML syntax error at line {}, column {})", path, line, col)` using `error.span()`/location, omitting the rendered snippet entirely; or
- Scrub the rendered message before storing (strip any line matching `api_key`), though location-only reporting is the safer, simpler fix.
- Add a regression test asserting that a malformed config whose `api_key` line is invalid does **not** leak the secret into stdout (mirrors the existing redaction tests).

### 2. [Low] File config cannot express "unbounded" budgets, and `0` means the opposite of the env path
`crates/jurisearch-cli/src/main.rs:1939-1944`

Env parsing routes `max_input_chars`/`max_estimated_tokens` through `parse_optional_usize`, where `0`/`none` → `None` (unbounded) — see the existing `status_reports_embedding_budget_env_overrides` test (`JURISEARCH_EMBED_MAX_INPUT_CHARS=0` → `null`). The file path stores the value literally: `max_input_chars = 0` becomes `Some(0)` (reject all input), and there is no way to set an unbounded ceiling from the file. Same-named knob, opposite meaning depending on source. **Recommendation:** treat `0` as `None` in the file path too (or document the asymmetry), and provide a file-level way to express unbounded if that's intended.

### 3. [Low] No `#[serde(deny_unknown_fields)]` — typos silently ignored
`crates/jurisearch-cli/src/main.rs:1802-1823`

`RuntimeConfigFile`/`EmbeddingConfigFile` derive `Deserialize` without `deny_unknown_fields`, so a misspelled key (`dimention = 768`, or an embedding key placed at the top level) is silently dropped and `config_loaded` still reports `true`. Given `status` is the primary feedback channel, silent drops are a real footgun. **Recommendation:** add `deny_unknown_fields` (note: this would turn unknown keys into parse errors, so land it together with finding #1's fix so the resulting error message is itself secret-safe).

### 4. [Low] Provider value accepted by env but rejected by file
`crates/jurisearch-cli/src/main.rs:2016-2024` vs `1809`

The env path uses lenient `parse_embedding_provider` (`openai`, `remote`, `openai-compatible`, `local`, `in-process`, …). The file path relies on the derived enum `Deserialize`, which only accepts exact `"openai_compatible"` / `"in_process"`. So `provider = "local"` in a file is a parse error while `JURISEARCH_EMBED_PROVIDER=local` works. **Recommendation:** add a `#[serde(alias = ...)]` set or a custom deserializer so file and env accept the same spellings.

### 5. [Nit] `JURISEARCH_CONFIG` disable-handling details
`crates/jurisearch-cli/src/main.rs:1878-1888`

- The `none`/`0`/empty disable check is applied to a *trimmed* copy, but the path is built from the *untrimmed* OsString, so `JURISEARCH_CONFIG="  /path  "` is treated as a real path with surrounding whitespace and fails to open.
- The plan/docs mention only `JURISEARCH_CONFIG=none`; the code also treats `0`, empty, and case-insensitive `NONE` as disable. Worth documenting the full set.

### 6. [Nit] `in_process` + `api_key` in file
`crates/jurisearch-cli/src/main.rs:1913-1926`

Selecting `provider = "in_process"` clears `api_key`, but a subsequent `api_key` key in the same table re-sets it. Harmless (never surfaced, not used by the in-process provider), but slightly surprising; consider ignoring `api_key`/`base_url` when the effective provider is in-process.

## Positives

- Layering order is correct and tested: file → env, env wins (`status_env_overrides_embedding_config_file_and_redacts_env_secret`).
- Default-path absence is handled cleanly (`NotFound && !explicit` is swallowed; explicit `JURISEARCH_CONFIG` misses are reported).
- XDG precedence is correct, including the empty-`XDG_CONFIG_HOME` fallback to `$HOME/.config`.
- New tests assert secrets are absent from stdout for the normal file and env cases, and the test harness scrubs ambient `JURISEARCH_*`/`XDG_CONFIG_HOME` for determinism (`jurisearch_command_without_embedding_env` + `JURISEARCH_CONFIG=none`).
- `IMPLEMENTATION_PLAN.md` note is accurate and correctly scopes remaining work (model cache/fetch, reachability checks).

## Recommendations

1. **(Blocking)** Fix finding #1: emit a source-free parse diagnostic (location only), and add a regression test for the malformed-`api_key`-line case across both `status` and JSONL `status`.
2. Reconcile the `0`/unbounded budget semantics between file and env (#2).
3. Add `deny_unknown_fields` (alongside #1's safe error message) (#3).
4. Unify provider spellings across file/env via serde aliases (#4).
5. Address the `JURISEARCH_CONFIG` trimming/doc nits (#5) opportunistically.

## Verdict: NO-GO

A confirmed secret-leak in the redaction-critical `status` output (finding #1) falls squarely within this slice's scope and violates its stated acceptance criterion ("Secrets are never written into manifest or logs"). It is reproducible against the built binary with a realistic trigger and has no test coverage. Fix #1 (with a regression test); the remaining findings are non-blocking and can be folded in or deferred. Once the leak is closed, this slice is otherwise in good shape to commit.
