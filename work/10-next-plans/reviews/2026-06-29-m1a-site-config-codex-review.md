# Codex Review: M1-A site config parser, validation, deterministic rendering

## Findings

### BLOCKER: Accepted config values can inject generated environment/unit content, including bypassing the loopback embedder guard

`render_site_env` writes the validated loopback URL first, then writes later string fields with no escaping or control-character rejection (`crates/jurisearch-deploy/src/render.rs:162`, `crates/jurisearch-deploy/src/render.rs:163`, `crates/jurisearch-deploy/src/render.rs:409`). Validation only checks that `embedder.model_name` is non-empty (`crates/jurisearch-deploy/src/validate.rs:332`) and does not reject embedded newlines/control characters. A TOML value such as an escaped newline in `embedder.model_name` can render:

```text
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8081
JURISEARCH_EMBED_MODEL=bge-m3
JURISEARCH_EMBED_BASE_URL=https://openrouter.ai/api/v1
```

That means `site validate` can pass the loopback-only check, but the generated `EnvironmentFile` can still set a hosted embedder URL after the checked one. The same raw-rendering pattern affects unit files too: `sync.corpora` only rejects blank entries (`crates/jurisearch-deploy/src/validate.rs:206`) but is inlined into `ExecStart` without escaping (`crates/jurisearch-deploy/src/render.rs:317`), so whitespace/newlines can corrupt arguments or inject additional unit lines. Similar risks exist for service user/group, DB names/users, paths, and other rendered strings.

Actionable fix: add one shared validation/encoding boundary for every value that is rendered into systemd env files or unit files. Either reject control characters/newlines and constrain identifiers/corpus names to a conservative allowlist, or render through correct systemd quoting/escaping helpers. Add regression tests where `embedder.model_name` attempts to inject `JURISEARCH_EMBED_BASE_URL=https://openrouter.ai/...`, and where a corpus/path/user value contains whitespace/newline, asserting validation fails before render.

### WARN: `embedder.base_url` can omit a port and silently diverge from the local llama-server port

The mismatch check only runs when `Url::port()` returns an explicitly specified port (`crates/jurisearch-deploy/src/validate.rs:300`). Therefore `base_url = "http://127.0.0.1"` with `port = 8081` validates: the site service will read `JURISEARCH_EMBED_BASE_URL=http://127.0.0.1` from `site.env`, while `jurisearch-bge-m3.service` starts llama-server on `--port 8081` (`crates/jurisearch-deploy/src/render.rs:162`, `crates/jurisearch-deploy/src/render.rs:371`). The generated deployment is locally loopback-only but operationally broken, because the query service posts to the scheme default port instead of the managed bge-m3 service.

Actionable fix: parse `embedder.base_url` once during validation, require `http`/`https` as appropriate, and compare `port_or_known_default()` to `embedder.port` or require an explicit URL port. Add a negative test for `http://127.0.0.1` + `port = 8081`.

## Notes

The loopback classifier is site-scoped in `jurisearch-deploy` and reuses `jurisearch_embed::base_url_class` rather than moving the policy into the producer embed path. The strict TOML schema uses `deny_unknown_fields` on the reviewed config structs, and the golden tests check stable byte output for the happy path. Secret file writes re-apply `0600`, and existing password-file validation rejects group/other-accessible files when present.

VERDICT: FIXES_REQUIRED
