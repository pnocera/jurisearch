## Findings

WARN: `validate_render_safety` applies the argv-safe path rule to config paths that `render.rs` does not currently emit. The new path list includes `system.runtime_dir`, `system.state_dir`, `database.admin_password_file`, and `license.token_json` at `crates/jurisearch-deploy/src/validate.rs:448`, but the renderer only emits `system.config_dir` via `EnvironmentFile`, `sync.source_root` via env/`ReadOnlyPaths`, `embedder.tokenizer_json` via env, `system.install_dir` as the `jurisearch*` binaries, `embedder.llama_server` as the bge-m3 binary, and `embedder.model_path` via env/`ExecStart` (`crates/jurisearch-deploy/src/render.rs:125`, `crates/jurisearch-deploy/src/render.rs:175`, `crates/jurisearch-deploy/src/render.rs:201`, `crates/jurisearch-deploy/src/render.rs:219`, `crates/jurisearch-deploy/src/render.rs:233`, `crates/jurisearch-deploy/src/render.rs:300`, `crates/jurisearch-deploy/src/render.rs:363`). This does not reopen the injection issue, but it can reject otherwise valid configs for paths that cannot affect generated env/unit files in M1-A, for example a password-file path containing a space. Fix by limiting `validate_render_safety` to fields actually rendered today, or by downgrading non-rendered paths to context-appropriate validation and applying argv-safe checks at the later milestone where those values are actually rendered or passed as argv.

## Security Re-Review

I did not find a remaining accepted-value path that can split or inject a generated env/unit line. Rendered free-text and path values are either identifier-allowlisted, numeric/bool/constant, or pass the shared control-character and argv-unsafe checks before rendering. The earlier newline/env-line injection case remains covered for env-file values.

I also did not find a remaining accepted-value path that can forge extra `ExecStart` argv tokens or trigger nested `$VAR`/`${VAR}` expansion. Cross-checking the generated commands in `render.rs` shows that the argv-bound values are covered:

- `site.bind`, `database.host`, `database.name`, `database.read_user`, and `site.workers` for `jurisearch serve-site`.
- `database.host`, `database.name`, `database.writer_user`, `database.read_user`, `database.owner_role`, each `sync.corpora[]`, `sync.source_root`, and `sync.interval_secs` for `jurisearch-syncd`.
- `embedder.llama_server`, `embedder.model_path`, `embedder.pooling`, and `embedder.port` for `llama-server`.
- `system.install_dir` for the `jurisearch` and `jurisearch-syncd` binary paths, and `system.config_dir` for the literal `EnvironmentFile=` paths.

The r2 regression tests target the right failure mode and are not obviously false-green: the exact `database.host` argv-forging string, space-bearing `embedder.model_path`, space-bearing Unix socket bind, nested `${EVIL}` expansion, and space-bearing `install_dir` all assert the new `render.*.argv_unsafe` diagnostics. The positive golden round-trip still asserts that the normal config renders unchanged.

## Verification

Ran:

- `git diff --check main`
- `cargo test -p jurisearch-deploy`
- `cargo fmt --check`
- `cargo clippy -p jurisearch-deploy --all-targets -- -D warnings`

All passed.

VERDICT: GO
