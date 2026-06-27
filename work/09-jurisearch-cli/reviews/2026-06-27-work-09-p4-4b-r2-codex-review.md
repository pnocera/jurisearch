## Findings

No blocking findings.

The prior `serve-site` initialization-order issue is fixed in the current source. `run_serve_site` now calls `bind_site_listener(args.tcp.as_deref(), args.socket.as_deref())` before constructing the `ReadHandle`, before calling `PreparedQueryEmbedder::from_env()`, and before building the rest of the service state. The listener helper emits the expected `bad_input` and returns `Ok(None)` for missing/both listener arguments, non-loopback TCP addresses, existing non-socket paths, and already-listening Unix sockets; only a successfully bound TCP or Unix listener reaches the embedder probe.

The new regression test exercises the false-green path from the prior review: it installs a deliberately broken embedding environment, then verifies missing listener arguments and non-loopback TCP are rejected with listener errors rather than tokenizer/embedding errors. That test would fail if the eager embedder construction moved back above listener validation.

## Tests Run

```text
cargo test -p jurisearch-cli --test cli_site_contract --no-default-features
cargo test -p jurisearch-cli site --no-default-features
```

VERDICT: GO
