# Phase 4A Skeleton Review

## Findings

### BLOCKER: The site `fetch` wire parser accepts malformed and unsupported requests instead of failing the site contract

`crates/jurisearch-cli/src/site/handlers.rs:26-42` parses `ids` by taking the JSON array and `filter_map`ing only string values. That means `{"ids":["cass:SITE",123]}` is accepted as `["cass:SITE"]`, and `{"ids":[123]}` is reported as an empty-id request instead of an invalid wire shape. The site `FetchRequest` schema requires `ids` to be an array whose items are strings (`crates/jurisearch-core/src/schema/search.rs:185-193`), so a malformed request can currently be partially executed.

The same parser also ignores every unsupported fetch option except the dispatcher-level `index_dir` ban. 4A deliberately exposes base fetch only, but `{"ids":["cass:SITE"],"part":"motivations"}` or `{"ids":["cass:SITE"],"online":true}` silently returns the base document as if the option were not supplied. That is worse than not implementing `--part`: a caller can believe the service honored an option it actually dropped.

Concrete fix: replace the loose `Value` parser with a site-specific DTO such as `SiteFetchArgs { ids: Vec<String> }` using `serde` and `#[serde(deny_unknown_fields)]`, or manually validate that the args object contains exactly `ids` and that every element is a string. Keep `index_dir` rejected in the dispatcher before handler validation. Add tests for mixed-type `ids`, all-non-string `ids`, and unsupported `part`/`online` fields.

### BLOCKER: Version/framing failures write an error but keep the connection open despite the selected close-on-framing-failure policy

`work/09-jurisearch-cli/P4-WORKING.md:50-55` selects the transport policy: return a session error line when recoverable, but close the connection for framing-level failures. In `crates/jurisearch-cli/src/site/listener.rs:38-50`, `decode_site_envelope_line` failures produce a null-id error response and then the loop continues. So an unversioned, skewed, or malformed site frame can be followed by another frame on the same connection and the listener will keep serving it.

That may be a valid policy if explicitly chosen, but it is not the policy documented for 4A. The current e2e test at `crates/jurisearch-cli/src/site/tests.rs:152-159` only asserts that an unversioned frame is rejected; it would still pass if the connection incorrectly continued after that framing failure.

Concrete fix: after writing/flushing the decode-error response, break the loop for `decode_site_envelope_line` failures. Add a listener test that sends an unversioned frame followed by a valid versioned `fetch` and asserts only one error line is written and the valid request is not dispatched. If the intended policy is actually to continue after malformed frames, update `P4-WORKING.md` and add a test for that exact behavior.

### WARN: Health reports `single_corpus` when there are zero active corpora

`crates/jurisearch-cli/src/site/handlers.rs:68-78` maps every topology with `corpora.len() <= 1` to `multi_corpus_readiness: "single_corpus"`. The P4 note says to report a single-corpus readiness stamp when exactly one corpus is active and to report the true topology (`work/09-jurisearch-cli/P4-WORKING.md:39-43`). With zero active corpora, the response would be `active_corpora: []` plus `"single_corpus"`, which is contradictory and can make an unactivated site database look ready.

Concrete fix: use an explicit `match corpora.len()` with a separate zero-corpus value such as `"no_active_corpus"` or `"public_working_set"`, keep `1 => "single_corpus"`, and keep `>1 => "deferred"`. Add a health test for the zero-active-corpus case, ideally with a tiny fake `QueryStore`/`ReadSnapshot` so it does not need another PG fixture.

### WARN: The unregistered-operation test is false-green for the `not_implemented` contract

`crates/jurisearch-cli/src/site/dispatcher.rs:170-177` is named `a_valid_but_unregistered_operation_is_not_implemented`, but the assertion is:

```rust
assert!(error.message.to_lowercase().contains("not") || !response.is_ok());
```

Because `response.error().expect(...)` already proves the response is an error, the `|| !response.is_ok()` side makes this test pass for any error class, including `bad_input` or a generic handler failure. It does not protect the 4A contract at `crates/jurisearch-cli/src/site/dispatcher.rs:83-85`, where a valid-but-unregistered `Operation` must become `ErrorCode::NotImplemented`.

Concrete fix: assert `error.code == ErrorCode::NotImplemented`, assert the response id is preserved, and assert the message names the unregistered command. Remove the `|| !response.is_ok()` fallback.

### WARN: The site e2e fetch test does not prove the promised render/parity surface

The 4A working checklist calls out fetch parity through `jurisearch-render` (`work/09-jurisearch-cli/P4-WORKING.md:57-62`), but the site e2e only checks that a response is ok and that one returned document has `document_id == "cass:SITE"` (`crates/jurisearch-cli/src/site/tests.rs:127-139`). It would still pass if the site path dropped chunks, body, citation/title fields, or returned a shape that the thin-client renderer could not print with one-shot parity.

Concrete fix: after the site round trip, render the `SessionResponse` through `jurisearch_render::render_session_response` and compare it with the expected pretty JSON body for the same result, or compare against the local fetch builder/one-shot output over the same seeded data. At minimum, assert the fields that prove the full `FetchResponse` shape, not just the top-level document id.

### NIT: `not_implemented` still talks about a Phase 0 scaffold

The 4A dispatcher uses `ErrorObject::not_implemented(operation.as_command())` for valid-but-unregistered site operations (`crates/jurisearch-cli/src/site/dispatcher.rs:83-85`), but the shared message says the command is not implemented in a "Phase 0 scaffold" and points to `IMPLEMENTATION_PLAN.md §10` (`crates/jurisearch-core/src/error.rs:35-43`). That is stale for the Phase 4 site service.

Concrete fix: make the shared message phase-agnostic, or add a site-specific not-implemented helper whose message says the operation is valid but not registered in the current site service slice.

## Verified

- `ReadHandle` now implements `QueryStore` and opens `LocalSnapshot` through the role-scoped handle (`crates/jurisearch-storage/src/query.rs:184-190`), so the site path is no longer forced through `ManagedPostgres`.
- The dispatcher rejects top-level client `index_dir` before operation parsing/handler dispatch (`crates/jurisearch-cli/src/site/dispatcher.rs:64-75`).
- The dispatcher allowlist is the `Operation::parse_command` result plus the handler map, and `build_skeleton_dispatcher` registers only `fetch` and `status` for 4A (`crates/jurisearch-cli/src/site/mod.rs:22-31`).
- `serve-site` refuses non-loopback TCP binds and keeps UDS/TCP handling separate from the local `serve` dispatcher (`crates/jurisearch-cli/src/site/serve.rs:32-60`).

## Tests Run

- `cargo test -p jurisearch-cli site::dispatcher -- --nocapture` — passed.
- `cargo test -p jurisearch-cli site::tests::the_site_service_serves_a_versioned_fetch_through_the_read_role -- --nocapture` — passed.
- `cargo test -p jurisearch-core operation -- --nocapture` — passed.

VERDICT: FIXES_REQUIRED
