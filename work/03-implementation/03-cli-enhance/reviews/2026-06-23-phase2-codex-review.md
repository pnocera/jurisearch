# Phase 2 CLI Enhancement Review

Scope reviewed: `git diff 4f534e9 HEAD`, focused on Phase 2 tasks T2.1-T2.4 per `/tmp/codex-review-phase2.md`.

## Findings

### 1. `serve --tcp` can expose the unauthenticated agent protocol off-host

Severity: High

`serve` binds whatever address the user passes directly with `TcpListener::bind(addr)` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5516)). There is no loopback enforcement, auth, or remote-bind guard before accepting session requests and dispatching them through `dispatch_session_request` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5481), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5484)). Binding `--tcp 0.0.0.0:PORT` or a LAN address therefore exposes all implemented session commands, including local index access and artifact-producing commands, to the network. The Phase 2 instructions frame this as a localhost dev daemon with stubbed authn/z/rate limiting, so silently accepting remote binds is too permissive.

Concrete fix: parse the TCP bind address before binding and reject non-loopback hosts by default. If remote access is intentionally needed later, require an explicit `--allow-remote` plus a real auth token or mTLS gate before dispatch. Add tests for `127.0.0.1`, `[::1]`, `0.0.0.0`, `::`, and a non-loopback interface address.

### 2. `serve --socket` deletes the requested path before verifying it is a stale socket

Severity: High

The Unix socket path branch unconditionally calls `fs::remove_file(path)` before `UnixListener::bind(path)` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5531), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5532)). If a user mistypes the socket path and points at a regular file, symlink, or another application's file, `serve` deletes it before it knows whether the path is actually a stale jurisearch socket.

Concrete fix: inspect the path first. If it does not exist, bind. If it exists and is a Unix socket, optionally try connecting to detect a live server; remove only a confirmed stale socket. For any regular file, directory, or symlink, return `bad_input`/dependency error without removing it. Add tests around regular files and stale socket files.

### 3. Search tuning flags are implemented but absent from the agent schema

Severity: Medium

The CLI and session payload accept request-scoped tuning fields: `--rrf-lexical-weight`, `--rrf-dense-weight`, and `--probes` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:260), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:320)). Those are threaded into `RetrievalOptions` and ultimately into storage ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:272), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:2446)). However, `SearchRequest` in the compiled schema lists only `query`, `kind`, `mode`, `group_by`, `format`, `top_k`, `cursor`, and `as_of` ([crates/jurisearch-core/src/schema.rs](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema.rs:78)). This violates the six-touchpoint rule: agents discovering the command through `help schema --json` cannot discover or validate the new T2.1 tuning surface.

Concrete fix: add `rrf_lexical_weight`, `rrf_dense_weight`, and `probes` to `SearchRequest`, including minimum/finite/integer constraints and defaults. Extend the contract/schema invariant so every CLI/session field added to an implemented command is represented in the schema.

### 4. Tuning inputs are not validated before they reach SQL

Severity: Medium

The new tuning fields are plain `Option<f64>`/`Option<u32>` with no validation in the one-shot or session search path ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:260), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:320)). Storage formats invalid weights by silently clamping non-finite or negative values to `0.0` ([crates/jurisearch-storage/src/retrieval.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/retrieval.rs:37)), while `ivfflat.probes` is emitted directly into `SET ivfflat.probes = ...` ([crates/jurisearch-storage/src/retrieval.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/retrieval.rs:202)). That means `--rrf-dense-weight -1` changes behavior silently instead of returning `bad_input`, and `--probes 0` can produce a database error rather than a user-level validation error. `eval tune` has the same finite/integer gap: parsed `f64` values are not checked for finiteness, and `probes` sweep points are cast to `u32` after `value.max(1.0)` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:1609), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:1648)).

Concrete fix: validate in the CLI/session layer before opening the index: weights must be finite and `>= 0`, probes must be `>= 1` and within a documented upper bound. For `eval tune --sweep probes=...`, reject non-integer probe values or document and implement a deliberate rounding policy. Do not silently clamp user-supplied invalid values in `format_sql_f64`; reserve defensive clamping for already-validated internal defaults.

### 5. `versions` and `diff` return successful null/empty payloads for unknown or out-of-window IDs

Severity: Medium

`inspect_payload` converts a missing document into `no_results` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5955), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5958)), but `versions_payload` and `diff_payload` deserialize and return storage output without checking whether anything resolved ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5993), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:6001)). Storage returns `versions: []` when the family CTE is empty ([crates/jurisearch-storage/src/retrieval.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/retrieval.rs:1008)) and `from_version: null`, `to_version: null`, `changed: false` when neither date resolves ([crates/jurisearch-storage/src/retrieval.rs](/home/pierre/Work/jurisearch/crates/jurisearch-storage/src/retrieval.rs:1055)). A typo in the ID or dates outside the known family therefore looks like a successful "no change" diff, which is misleading for temporal inspection.

Concrete fix: have `document_versions_json` include a `found`/`count` signal and make `versions_payload` return `no_results` when `count == 0`. For `diff`, return `no_results` when the family is empty, and either return `bad_input`/`no_results` when one of the requested dates has no in-force version or include an explicit `coverage` object that distinguishes `missing_from`, `missing_to`, and `changed`. Add fixtures for unknown IDs and out-of-window dates.

### 6. The socket server has no request-size or slow-client guard

Severity: Medium

`serve_jsonl` reads each request with `BufRead::lines()` ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5476)). That allocates until newline with no maximum line size. The server also handles one connection to completion before accepting the next ([crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5521), [crates/jurisearch-cli/src/main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5540)), so a client can hold the single-client daemon indefinitely by sending a very large line or never completing a line. The review instructions explicitly call out malformed-input handling and obvious localhost DoS/resource issues; this is an easy one to trigger even locally.

Concrete fix: replace `lines()` with bounded `read_line` logic that rejects lines over a documented maximum, for example 1-8 MiB. Set read/write timeouts on accepted TCP streams and consider an idle timeout for Unix streams if available. Return a JSONL `bad_input` error for oversize requests and close that connection so the listener can accept the next client.

## Notes

I did not run the test suite because the instruction was to avoid modifying any files other than this review artifact; a cargo test run would write under `target/`. The review above is source-based against the requested diff.

VERDICT: FIXES_REQUIRED
