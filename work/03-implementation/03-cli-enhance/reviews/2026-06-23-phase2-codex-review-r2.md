# Phase 2 CLI Enhancement Review r2

Scope reviewed: HEAD `8f36eb697e1374c212642f01050303528a96ace4` on `main`, focused on the r2 fixes for the six findings from `work/03-implementation/03-cli-enhance/reviews/2026-06-23-phase2-codex-review.md`.

## Findings

### 1. Unix `serve --socket` accepted streams still have no write timeout

Severity: Medium

The TCP serve path now sets both read and write timeouts on every accepted stream before calling `serve_jsonl` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5620), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5621)). The Unix socket path only sets a read timeout ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5658)) and then passes the same stream as the writer into `serve_jsonl` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5660)). `serve_jsonl` writes full JSON responses synchronously and flushes them before accepting more work ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5584), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5585)); because the daemon is single-client sequential, a Unix socket client that sends a request and then stops reading can still block the server indefinitely on a large response. `std::os::unix::net::UnixStream` supports `set_write_timeout`, so this is not a platform limitation in the current toolchain.

Concrete fix: set a write timeout on Unix accepted streams as well, matching the TCP path, before constructing the `BufReader`. Add a socket transport test that issues a response-producing request from a client that does not drain the response, or at least a unit/contract test pinning that both accepted-stream timeout calls are applied through a small helper.

## Resolved r1 Findings

- Finding 1 is resolved for TCP binds: `run_serve` resolves the requested address and rejects a non-loopback `SocketAddr` unless `--allow-remote` is present.
- Finding 2 is resolved for regular files, directories, symlinks, and live sockets: the Unix path uses `symlink_metadata`, refuses non-sockets, refuses a socket that accepts `UnixStream::connect`, and only removes a stale socket file.
- Finding 3 is resolved: `SearchRequest` now advertises `rrf_lexical_weight`, `rrf_dense_weight`, and `probes`, and the CLI contract test pins the new schema surface.
- Finding 4 is resolved for the CLI/session search and eval-run boundaries: `validate_retrieval_options` rejects non-finite or negative weights and probes outside `1..=4096`; `eval tune` rejects non-finite sweep bounds and non-integer or sub-1 probes sweeps, then calls the same validation through `eval_run_payload`.
- Finding 5 is resolved: `versions` now returns `no_results` when the family count is zero; `diff` returns `no_results` for an empty family and adds `family_count`, `missing_from`, and `missing_to` to distinguish missing coverage from unchanged versions.
- Finding 6 is partially resolved: request lines are bounded to 8 MiB and oversize requests return `bad_input` before the connection closes; TCP streams now have both read and write timeouts, but Unix streams are still missing the write timeout above.

## Notes

I did not run the full test suite because the requested output was review-only and tests would write build artifacts outside the review file. I did inspect the HEAD diff and the current source for the affected CLI, schema, and storage paths.

VERDICT: FIXES_REQUIRED
