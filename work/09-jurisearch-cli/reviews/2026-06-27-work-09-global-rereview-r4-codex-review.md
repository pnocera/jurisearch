# Global work/09 rereview r4

## Findings

### WARN 1 - The P6 documents still attribute protocol-skew evidence to the checked-in single-host capture, but that capture does not exercise skew.

The current implementation correctly narrows the script itself: `scripts/single-host-acceptance.sh` always runs only the no-server client legs for unknown operation, unsupported `index_dir`, unsupported `zone`, unreachable service, and bad URL (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:65`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:73`). It has no old-server/non-versioned TCP leg, and the observed block likewise contains no skew capture (`work/09-jurisearch-cli/05-two-host-acceptance.md:101`-`work/09-jurisearch-cli/05-two-host-acceptance.md:135`). Skew is instead covered by the automated tests listed at the top of the runbook (`work/09-jurisearch-cli/05-two-host-acceptance.md:10`-`work/09-jurisearch-cli/05-two-host-acceptance.md:14`) and by the passing `site::tests::the_thin_client_rejects_an_old_servers_unversioned_reply` / transport response-envelope tests.

Two document passages still say otherwise. The runbook says the shipped client operator surface, including "connection/skew diagnostics", is proven by the checked-in single-host capture below (`work/09-jurisearch-cli/05-two-host-acceptance.md:18`-`work/09-jurisearch-cli/05-two-host-acceptance.md:24`), even though the next paragraph correctly says protocol-version skew is proven separately by automated tests (`work/09-jurisearch-cli/05-two-host-acceptance.md:26`-`work/09-jurisearch-cli/05-two-host-acceptance.md:33`). The implementation plan repeats the same over-attribution in Done-when, saying the shipped thin client has correct contract/connection/skew diagnostics with the parenthetical "checked-in single-host capture of the real binary" (`work/09-jurisearch-cli/04-implementation-plan.md:300`-`work/09-jurisearch-cli/04-implementation-plan.md:302`).

This is no longer the r3 false-green script bug: the actual script and tests now make the evidence split defensible. It is still an accuracy problem in the reviewed artifacts, because a reader can conclude that the checked-in operated capture proves a shipped-binary skew diagnostic that it does not run. Fix direction: remove `skew` from the single-host capture claims, or add a real shipped-binary skew leg to `single-host-acceptance.sh` and the observed block.

## Notes

The r3 script false-green findings are fixed in the current tree. `expect_reject` now requires both a non-zero exit and the expected diagnostic substring, so a regression that forwarded `index_dir`/`zone` to a dead server would fail instead of passing on connection refusal alone (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:50`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:73`). The data-leg branch now refuses to proceed without `--fetch-id`, so it cannot print an "all legs passed" success while omitting fetch/hash (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:134`-`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:143`).

The new source-level seam is coherent. `Operation::parse_args` owns strict site DTO parsing in `jurisearch-core`, including `status` as an empty strict DTO (`crates/jurisearch-core/src/site_request.rs:156`-`crates/jurisearch-core/src/site_request.rs:207`). The site handlers now use that seam before building query inputs (`crates/jurisearch-cli/src/site/handlers.rs:57`, `crates/jurisearch-cli/src/site/handlers.rs:83`, `crates/jurisearch-cli/src/site/handlers.rs:101`, `crates/jurisearch-cli/src/site/handlers.rs:147`, `crates/jurisearch-cli/src/site/handlers.rs:163`, `crates/jurisearch-cli/src/site/handlers.rs:179`, `crates/jurisearch-cli/src/site/handlers.rs:201`), and the thin client performs the same local preflight before connecting (`crates/jurisearch-client/src/main.rs:54`-`crates/jurisearch-client/src/main.rs:67`). I did not find a Rust correctness blocker in that path.

## Validation

- `git status --short`
- `git diff --stat`
- CodeGraph status/context/explore/node for `Operation::parse_args`, `SiteRequest`, `parse_command`, site handlers, and dispatcher wiring
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p jurisearch-core site_request -- --nocapture`
- `cargo test -p jurisearch-client --test cli_acceptance -- --nocapture`
- `cargo test -p jurisearch-client --test dependency_cone -- --nocapture`
- `cargo test -p jurisearch-transport response_envelope -- --nocapture`
- `cargo test -p jurisearch-cli site::handlers::tests::health_rejects_unsupported_status_args -- --nocapture`
- `cargo test -p jurisearch-cli site::tests:: -- --nocapture`
- `cargo test -p jurisearch-cli site::dispatcher::tests:: -- --nocapture`
- `bash -n work/09-jurisearch-cli/scripts/single-host-acceptance.sh`
- `work/09-jurisearch-cli/scripts/single-host-acceptance.sh` (client diagnostic legs passed; data legs skipped because DB/embedder prerequisites are absent on this workstation)

VERDICT: FIXES_REQUIRED
