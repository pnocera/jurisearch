## Findings

BLOCKER: `validate_render_safety` still allows whitespace in values that systemd later expands as `ExecStart` arguments, so accepted configs can forge argv flags even though they cannot inject new file lines. `database.host` is only checked with `require_no_control` in `validate.rs:420`, but it is emitted to the env file and then used as a separate `${JURISEARCH_DB_HOST}` word in both site and syncd units (`render.rs:258`, `render.rs:308`). With systemd's `${VAR}` expansion, a value like `127.0.0.1 --max-concurrent-embeds 9999` is split into additional argv words, so validation accepts a config that changes the rendered service command. The same class applies to path/free-text values expanded through `${VAR}` or rendered as unquoted command words, including Unix `site.bind` (`render.rs:255`), `sync.source_root` (`render.rs:320`), `embedder.model_path` (`render.rs:368`), and executable paths built from `system.install_dir` / `embedder.llama_server` (`render.rs:234`, `render.rs:365`) while paths are only checked for control characters in `validate.rs:433`. Fix by either rendering each argv token with a systemd-aware escaping/quoting function and adding golden tests for spaces/metacharacters, or by making every value used in `ExecStart` pass an argv-safe allowlist/reject ASCII whitespace and systemd quoting metacharacters. Add regression tests that set `database.host = "127.0.0.1 --max-concurrent-embeds 9999"` and a space-bearing `embedder.model_path` / Unix socket path and assert validation fails or the rendered unit keeps each as one argv token.

## Resolved Checks

The original newline/control-character env-line injection is addressed for the rendered fields I audited: `render.rs` writes config-derived strings through site env, syncd env, bge-m3 env, `User=`, `Group=`, `EnvironmentFile=`, `ReadOnlyPaths=`, and `ExecStart`, and the corresponding string/path sets in `validate.rs:398`, `validate.rs:420`, and `validate.rs:433` cover those values, with numeric/enum fields inherently safe. The specific `model_name` hosted-URL newline bypass now fails validation via `render.value.control_char`.

The default-port divergence is addressed: `validate.rs:306` parses the loopback URL once, requires `http`/`https`, and compares `port_or_known_default()` with `embedder.port`, so `http://127.0.0.1` with `port = 8081` is now rejected.

The new regression tests exercise the prior newline/control-char cases and the default-port mismatch. I also ran `cargo test -p jurisearch-deploy`; all 43 tests passed.

VERDICT: FIXES_REQUIRED
