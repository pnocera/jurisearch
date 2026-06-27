## Findings

### P2 - `serve-site` checks the embedding runtime before rejecting invalid listener arguments

`run_serve_site` builds the shared `PreparedQueryEmbedder` at `crates/jurisearch-cli/src/site/serve.rs:218-227` before it validates the mutually exclusive transport options at `crates/jurisearch-cli/src/site/serve.rs:236-239` or resolves/refuses a non-loopback TCP bind at `crates/jurisearch-cli/src/site/serve.rs:240-252`. That means an invalid command such as "no `--tcp`/`--socket`" can fail on tokenizer/model/embedding configuration before returning the expected `bad_input` about the listener arguments.

I reproduced this by forcing an invalid tokenizer path and omitting the listener:

```text
JURISEARCH_EMBED_PROVIDER=openai_compatible \
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:9 \
JURISEARCH_EMBED_TOKENIZER_JSON=/definitely/not/here \
cargo run -q -p jurisearch-cli --no-default-features -- \
  serve-site --db-name jurisearch --db-user jurisearch_read
```

The command returned `dependency_unavailable` for the tokenizer instead of `serve-site requires exactly one of --tcp or --socket`. The same ordering can mask non-loopback bind rejection or an already-listening/stale Unix socket check. This is a new 4B regression from introducing the eager service embedder before the transport validation. Move the listener selection/bind validation ahead of `PreparedQueryEmbedder::from_env()` so malformed invocations and bind conflicts fail before probing the embedding stack.

## Notes

I did not find a byte-shape regression in the moved `search`/`cite` response construction during source review. The CLI adapters now resolve boundary inputs and call the `jurisearch-query` builders, while the existing retrieval/session contract tests still pass.

## Tests Run

```text
cargo test -p jurisearch-cli site --no-default-features
cargo test -p jurisearch-cli --test cli_byte_parity --no-default-features
cargo test -p jurisearch-cli --no-default-features
```

VERDICT: FIXES_REQUIRED
