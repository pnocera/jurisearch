# work/09 P6 Review

## Findings

### High: the P6 acceptance test bypasses the actual `jurisearch-client` binary

The single-host acceptance test is named and documented as proving that the structurally separate thin client queries the site over TCP, but the test never executes the shipped `jurisearch-client` artifact. In `crates/jurisearch-cli/src/site/tests.rs:500` through `crates/jurisearch-cli/src/site/tests.rs:529`, it calls `jurisearch_client::parse_endpoint` and `jurisearch_client::send_request` directly, then renders through `jurisearch_render::render_session_response`. That bypasses the binary wrapper in `crates/jurisearch-client/src/main.rs:35` through `crates/jurisearch-client/src/main.rs:68`, which owns the user-facing CLI contract: `--server` / `--local` / env selection, positional command and JSON-args parsing, stdout rendering, stderr diagnostics, and exit-code mapping.

That leaves a false-green hole in the final-phase proof. A regression in the actual artifact that host C runs, for example broken clap positional parsing, incorrect JSON-object validation, wrong exit codes for served errors, or stdout/stderr drift, would not fail the P6 acceptance test. The dependency-cone test proves the client crate stays thin, but it does not prove that the binary actually works against the site URL.

Fix: add at least one acceptance test that spawns the `jurisearch-client` binary against a real versioned TCP test server and asserts the rendered stdout bytes and success/failure exit codes. The existing PG-backed site test can keep using the library for low-level protocol assertions, but the final topology proof should include the executable artifact.

### Medium: `parse_endpoint` accepts endpoints outside its documented URL contract

`crates/jurisearch-client/src/lib.rs:76` through `crates/jurisearch-client/src/lib.rs:93` documents that only `tcp://host:port` and `unix:///absolute/path` are accepted, but the implementation only strips the scheme and checks for a non-empty remainder. As a result, `unix://relative.sock` is accepted and later used as a cwd-relative Unix socket path, and malformed TCP inputs such as `tcp://localhost` are accepted as `SiteEndpoint::Tcp` before failing later as an "unreachable" socket address. The tests at `crates/jurisearch-client/src/lib.rs:210` through `crates/jurisearch-client/src/lib.rs:215` cover bare/unknown/empty schemes but not relative Unix paths or missing TCP ports.

This weakens the P6 "addressed by explicit, copyable service URL" contract and produces the wrong class of operator error for malformed endpoints. I confirmed the behavior with `cargo run -q -p jurisearch-client -- --server unix://relative.sock status`, which attempted to connect to `unix://relative.sock` instead of rejecting the endpoint as malformed.

Fix: make `parse_endpoint` enforce its grammar. For Unix sockets, require `unix:///...` and `Path::is_absolute()`. For TCP, reject at least missing `host:port` syntax before constructing `SiteEndpoint::Tcp`, and add negative tests for `unix://relative.sock`, `unix://./sock`, and `tcp://localhost`.

## Verification

- `cargo test -p jurisearch-transport`
- `cargo test -p jurisearch-client`
- `cargo test -p jurisearch-cli the_thin_client_rejects_an_old_servers_unversioned_reply`
- `cargo test -p jurisearch-cli the_thin_client_queries_the_site_over_tcp_with_one_shot_render_parity`
- `cargo test -p jurisearch-cli site::tests::the_site_service_serves_the_full_operation_set_through_the_read_role`
- `cargo test -p jurisearch-cli site::serve::tests`
- `cargo test -p jurisearch-cli site::listener::tests`
- `cargo test -p jurisearch-cli site::tests::concurrent_dispatch_opens_one_independent_snapshot_per_request`

VERDICT: FIXES_REQUIRED
