# Codex Review: M5-A Thin Client Configure/Doctor

## Findings

### BLOCKER: explicit `--server` / env forwarding is blocked by a malformed low-priority config

- `crates/jurisearch-client/src/main.rs:190`
- `crates/jurisearch-client/src/main.rs:191`

`run_forward` loads and parses `client.toml` before applying endpoint precedence. That means a stale or malformed persisted config makes ordinary forwarded commands fail even when the user supplied `--server`, `--local`, or `JURISEARCH_SITE_URL`, all of which are documented to outrank the config. This is a regression from the previous forward path: an explicit endpoint should not be hostage to an unused fallback file.

I reproduced the behavior with a malformed `$XDG_CONFIG_HOME/jurisearch/client.toml` and an explicit `--server tcp://127.0.0.1:1`; the client exits on the TOML parse error before it attempts the explicit endpoint.

Fix: only load `client.toml` when neither `--local` nor `--server` is present and `JURISEARCH_SITE_URL` is absent. Preserve the existing behavior that a present-but-malformed env var is an endpoint error rather than falling through to config.

### WARN: `doctor` treats missing config as fatal even when `JURISEARCH_SITE_URL` selects the endpoint

- `crates/jurisearch-client/src/main.rs:127`
- `crates/jurisearch-client/src/main.rs:129`
- `crates/jurisearch-client/src/main.rs:151`

The comment says an absent config is advisory when a flag/env still resolves, and endpoint precedence documents `$JURISEARCH_SITE_URL` above `client.toml`. The implementation only considers `cli.server.is_some() || cli.local` when deciding whether missing config is OK, so `JURISEARCH_SITE_URL=tcp://... jurisearch-client doctor` reports `[FAIL] no client config...` even though endpoint resolution then uses the env URL.

That makes env-configured deployments fail `doctor` solely because they chose not to persist a file. If the live status handshake succeeds, this still exits 2 because `healthy` was already set false.

Fix: include the env selector in the missing-config advisory condition, ideally via the same endpoint-selection helper/path used for the handshake.

### WARN: config save can install a non-0600 file from a stale temp path

- `crates/jurisearch-client/src/config.rs:101`
- `crates/jurisearch-client/src/config.rs:103`
- `crates/jurisearch-client/src/config.rs:105`
- `crates/jurisearch-client/src/config.rs:107`
- `crates/jurisearch-client/src/config.rs:120`

`save_config_at` uses a deterministic temp name based only on the process id and opens it with `create(true).truncate(true).mode(0o600)`. On Unix, `mode(0o600)` only applies when the file is newly created. If the temp file already exists with wider permissions, the code truncates it, fsyncs it, and renames it into `client.toml` while preserving the old wider mode. That violates the required 0600 config-file invariant.

The current permission test only covers the fresh-temp path, so it will not catch this false green.

Fix: use `create_new(true)` with a unique temp name, or unlink any stale temp before opening and verify/chmod the temp file to 0600 before rename. A regression test should pre-create the temp path with 0644 and assert the final config mode is 0600.

## Notes

- Thin dependency cone is preserved in the checked normal dependency tree. `cargo tree -e normal --prefix none -p jurisearch-client` shows the three base crates plus lightweight dependencies; the forbidden heavy crates (`jurisearch-storage`, `jurisearch-embed`, `jurisearch-ingest`, `jurisearch-cli`, `jurisearch-official-api`, `jurisearch-package`, `jurisearch-package-build`, `jurisearch-syncd`, `postgres`, `tokenizers`, `ureq`) are absent. The existing `dependency_cone` test checks the normal tree, so it catches transitive normal-dependency pulls.
- `configure` validates through `parse_endpoint` before saving and rejects `--local`.
- XDG resolution source logic ignores relative/empty `XDG_CONFIG_HOME` and falls back to absolute `$HOME/.config`, though the included XDG unit test does not actually mutate env and is mostly a shape check.
- `doctor` uses the existing `status` transport path and distinguishes unreachable/protocol-skew/served-error outcomes.

## Validation

- `cargo test -p jurisearch-client` passed: 24 tests.
- `cargo tree -e normal --prefix none -p jurisearch-client` inspected for the dependency cone.

VERDICT: FIXES_REQUIRED
