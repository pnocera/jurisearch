# A3 Config Validation Re-Review

## Findings

### WARN: The acceptance matrix still lacks an allowed positive-weight contract test

The implementation now correctly threads `authority_weight` through the clap surface, shared
`SearchRequest`, `RetrievalOptions`, session deserialization, schema discovery, and the main/zone
routing checks (`crates/jurisearch-cli/src/args.rs:197`, `crates/jurisearch-cli/src/request.rs:50`,
`crates/jurisearch-cli/src/query_support.rs:43`, `crates/jurisearch-cli/src/retrieval/search.rs:55`,
`crates/jurisearch-cli/src/retrieval/zone.rs:63`, `crates/jurisearch-core/src/schema/search.rs:24`).
Runtime spot checks also show that an allowed positive request (`search ... --kind decision
--authority-weight 0.5`, no cursor) gets past A3 and reaches the normal index requirement, and the same
is true through JSONL session search.

The remaining gap is that the new regression tests only cover positive-weight rejection combinations
(`kind=all`/`code`, inbound cursor, zone cursor) plus invalid numeric values and the `0.0` inert path
(`crates/jurisearch-cli/tests/cli_retrieval_contract.rs:1285-1435`). They do not lock the explicitly
allowed positive case from the A3 acceptance matrix: positive finite weight with `kind=decision` and no
cursor. A future change could accidentally reject all positive authority weights on the main path while
the current tests still pass.

Concrete fix: add a small no-index contract test asserting
`search clause --kind decision --authority-weight 0.5` returns `index_unavailable` rather than
`bad_input`, and add the matching JSONL session case if the session parity test is intended to cover
accepted as well as rejected values. This is cheap and uses the same pre-index pattern as the existing
`0.0` test.

## Verification

- `cargo test -p jurisearch-cli authority_weight -- --nocapture` passed.
- `cargo test -p jurisearch-cli help_schema_json_is_valid_and_lists_commands -- --exact` passed.
- `cargo check -p jurisearch-cli` passed.
- Manual spot checks confirmed allowed positive CLI/session requests reach `index_unavailable` rather
  than A3 `bad_input` when no index is configured.

VERDICT: FIXES_REQUIRED
