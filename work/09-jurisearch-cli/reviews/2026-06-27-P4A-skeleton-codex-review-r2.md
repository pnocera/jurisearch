# Phase 4A Skeleton Re-review r2

## Findings

No findings.

## Verified

- The loose `fetch` argument parser is gone. `SiteFetchArgs` is a typed `ids: Vec<String>` DTO with `#[serde(deny_unknown_fields)]`, and malformed or unsupported args become `bad_input` before a snapshot is opened (`crates/jurisearch-cli/src/site/handlers.rs:17`, `crates/jurisearch-cli/src/site/handlers.rs:28`). The new handler tests cover mixed-type IDs, all-non-string IDs, unsupported `part`/`online`, and the empty-list branch (`crates/jurisearch-cli/src/site/handlers.rs:122`, `crates/jurisearch-cli/src/site/handlers.rs:133`, `crates/jurisearch-cli/src/site/handlers.rs:143`, `crates/jurisearch-cli/src/site/handlers.rs:158`). These tests are not false-green against the old behavior because the assertion requires the strict parser error text, and the fake snapshot panics if a rejected malformed request reaches a read.
- The framing failure policy now matches `P4-WORKING.md`: after `decode_site_envelope_line` fails, the listener writes one null-id error response, flushes, and breaks the connection loop (`crates/jurisearch-cli/src/site/listener.rs:48`). The regression test sends an unversioned frame followed by a valid versioned `fetch`, registers a panic handler for that second frame, and asserts only one response line is produced (`crates/jurisearch-cli/src/site/listener.rs:105`). That directly covers the r1 false-green hole.
- Site health now distinguishes the zero-active-corpus topology from a ready single-corpus topology via `0 => "no_active_corpus"`, `1 => "single_corpus"`, and `_ => "deferred"` (`crates/jurisearch-cli/src/site/handlers.rs:69`). The new zero-corpus test uses a fake `QueryStore` with no PG dependency and asserts both the empty topology and the readiness label (`crates/jurisearch-cli/src/site/handlers.rs:167`).
- Valid-but-unregistered operations now use a site-specific phase-agnostic `NotImplemented` error (`crates/jurisearch-cli/src/site/dispatcher.rs:83`, `crates/jurisearch-cli/src/site/dispatcher.rs:98`). The test no longer has the `|| !is_ok()` escape hatch; it asserts id correlation, `ErrorCode::NotImplemented`, and a message that names `search` (`crates/jurisearch-cli/src/site/dispatcher.rs:184`).
- The site e2e fetch test now proves more than the document id. It asserts the fetched citation, title, body, and chunk payload, then renders the returned `SessionResponse` through `jurisearch_render::render_session_response` and compares it to the one-shot body renderer over the same result (`crates/jurisearch-cli/src/site/tests.rs:127`, `crates/jurisearch-cli/src/site/tests.rs:151`). That is an adequate 4A parity check because the site handler calls the same `jurisearch-query` `build_fetch` body builder used by the dependency-light fetch path (`crates/jurisearch-cli/src/site/handlers.rs:36`, `crates/jurisearch-query/src/builders.rs:34`).
- The service still uses the read-role store path: `serve-site` constructs a `ReadHandle` from the requested database credentials (`crates/jurisearch-cli/src/site/serve.rs:20`), and `ReadHandle` now implements `QueryStore` by opening a `LocalSnapshot` on a checked-out read-role client (`crates/jurisearch-storage/src/query.rs:187`).

## Tests Run

- `cargo test -p jurisearch-cli site -- --nocapture` - passed (12 passed, including the new site handler/listener tests and the read-role e2e).

VERDICT: GO
