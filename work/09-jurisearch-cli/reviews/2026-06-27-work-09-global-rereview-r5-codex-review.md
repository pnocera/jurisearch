# Global work/09 rereview r5

## Findings

### NIT 1 - The single-host observed block says "captured verbatim", but the pasted output is normalized.

`work/09-jurisearch-cli/05-two-host-acceptance.md:95` introduces the block as a real run of `scripts/single-host-acceptance.sh`, and `work/09-jurisearch-cli/05-two-host-acceptance.md:102` says the client legs are "captured verbatim". The block itself is abbreviated/normalized: it uses `$ jurisearch-client ...` commands, puts `exit=2` on the diagnostic line, and uses ellipses in the `zone`, connection-refused, and bad-URL diagnostics (`work/09-jurisearch-cli/05-two-host-acceptance.md:114`-`work/09-jurisearch-cli/05-two-host-acceptance.md:122`). The actual script prints `+ <absolute target/debug binary> ...`, captures stderr, and then prints `exit=<code>` on its own line (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:55`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:58`); my rerun produced that expanded shape.

The evidence is directionally accurate, and the script itself passed, but the document should either paste the exact captured output or label the block as a normalized excerpt. This is not a behavior blocker.

### NIT 2 - ShellCheck flags one unchecked directory change in the acceptance script.

`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:39`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:40` derives `ROOT` from `BASH_SOURCE[0]` and then runs `cd "$ROOT"` without handling failure. ShellCheck reports SC2164 and recommends `cd "$ROOT" || exit`. In normal direct use this is low-risk because the path is derived from the script location, and the script ran successfully, but it is still a simple hardening fix.

## Notes

The previous r4 warning about protocol-skew evidence is resolved in the main P6 text. The implementation plan now assigns skew coverage to the automated in-process/transport tests and assigns the single-host operated capture only to the shipped client's contract seam plus connection/URL diagnostics (`work/09-jurisearch-cli/04-implementation-plan.md:280`-`work/09-jurisearch-cli/04-implementation-plan.md:305`). The runbook says the same thing: skew is listed in automated coverage, and the single-host capture explicitly excludes skew (`work/09-jurisearch-cli/05-two-host-acceptance.md:18`-`work/09-jurisearch-cli/05-two-host-acceptance.md:40`).

The core request seam is sound. `jurisearch-core::site_request` now owns strict site DTO parsing, including `status` as an empty strict DTO (`crates/jurisearch-core/src/site_request.rs:156`-`crates/jurisearch-core/src/site_request.rs:207`). The site handlers use `Operation::parse_args` before building shared CLI/query inputs (`crates/jurisearch-cli/src/site/handlers.rs:57`-`crates/jurisearch-cli/src/site/handlers.rs:207`), and the thin client runs the same local preflight before connecting (`crates/jurisearch-client/src/main.rs:54`-`crates/jurisearch-client/src/main.rs:67`). I did not find a Rust correctness blocker in this path.

The earlier false-green risks in the operated script are fixed. `expect_reject` now requires both a non-zero exit and the expected diagnostic substring, so forwarding `index_dir`/`zone` to a dead port would fail rather than pass on connection refusal alone (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:50`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:73`). The data-leg branch refuses to proceed without `--fetch-id`, so it cannot report a complete data run while silently omitting fetch/hash (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:134`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:143`).

## Validation

- `git status --short --branch`
- `git diff --stat`
- CodeGraph status/context/explore for the site request DTOs, handlers, client preflight, and request adapters
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p jurisearch-core site_request -- --nocapture`
- `cargo test -p jurisearch-client --test cli_acceptance -- --nocapture`
- `cargo test -p jurisearch-client --test dependency_cone -- --nocapture`
- `cargo test -p jurisearch-cli site::handlers::tests:: -- --nocapture`
- `cargo test -p jurisearch-cli site::tests:: -- --nocapture`
- `cargo test -p jurisearch-cli site::dispatcher::tests:: -- --nocapture`
- `cargo test -p jurisearch-transport rejects -- --nocapture`
- `bash -n work/09-jurisearch-cli/scripts/single-host-acceptance.sh`
- `shellcheck work/09-jurisearch-cli/scripts/single-host-acceptance.sh` (reported SC2164, captured above)
- `work/09-jurisearch-cli/scripts/single-host-acceptance.sh` (client diagnostic legs passed; data legs skipped because DB/embedder prerequisites are absent on this workstation)

VERDICT: GO (non-blocking nits only)
