# Global work/09 rereview

## Findings

### BLOCKER 1 - P6 still does not have the required operated two-host acceptance evidence.

Phase 6 still defines the operated run as a deliverable and verification item: the plan requires "A two-host acceptance run (producer -> site server -> thin client) as ops evidence" and says done means "A thin client on a second machine queries the site service by URL and renders identically; the two-host acceptance is recorded" (`work/09-jurisearch-cli/04-implementation-plan.md:279`, `work/09-jurisearch-cli/04-implementation-plan.md:285`, `work/09-jurisearch-cli/04-implementation-plan.md:288`). The current acceptance doc still labels itself the "GENUINE two-physical-host operated run" and says the observed blocks are to be filled when operated (`work/09-jurisearch-cli/05-two-host-acceptance.md:18`), but the two-host observed section remains a template with empty host identities, package digest, status output, fetch hashes, and negative-check results (`work/09-jurisearch-cli/05-two-host-acceptance.md:132`).

The newly added single-host section is useful, but it does not satisfy that done condition. Its live data legs explicitly did not run: the captured DB and embedder prerequisites are both missing, and the block ends with "data legs SKIPPED" plus a request to rerun on a host with both prerequisites (`work/09-jurisearch-cli/05-two-host-acceptance.md:111`). That means the checked-in evidence still has not proven the operated producer -> site server -> thin client path, including a real site DB populated by syncd, the system services, the LAN bind, local bge-m3 startup, a real `serve-site` response from host C, or byte/hash parity across host C and host S.

Fix direction: either run the real two-host acceptance and replace the placeholder with concrete observed evidence, or narrow the plan/design so P6 no longer claims that operated two-host deployment as done.

### WARN 1 - `status` bypasses the strict shared request-args validation that the new contract seam claims.

`Operation::parse_args` documents itself as the contract-owned boundary where unsupported fields become `bad_input` (`crates/jurisearch-core/src/site_request.rs:170`). That is true for `search`, `fetch`, `cite`, `related`, `context`, and `compare`, but `Operation::Status` returns `SiteRequest::Status` without inspecting `args` at all (`crates/jurisearch-core/src/site_request.rs:186`). The handler also ignores the args value (`crates/jurisearch-cli/src/site/handlers.rs:202`). As a result, arbitrary status fields are silently accepted by the server and are not rejected by the client-side preflight, even though the client comment says typos or unsupported fields fail fast through this same seam (`crates/jurisearch-client/src/main.rs:54`).

I confirmed the behavioral gap with the shipped client binary: `target/debug/jurisearch-client --server tcp://127.0.0.1:1 status '{"bogus":true}'` did not fail locally with an unknown-field diagnostic; it proceeded to the network and reported connection refused. The same probe against `search '{"query":"x","bogus":true}'` failed locally with the strict DTO unknown-field error. This leaves `status` outside the "one authority for request defaults + field validation" claim and makes the single-host evidence overstate that the strict DTO seam rejects typos for the site surface as a whole.

Fix direction: add a strict empty `SiteStatusArgs` DTO, parse it in `Operation::parse_args`, and add both core/client and site-handler coverage for `status` rejecting non-empty args.

### WARN 2 - The single-host acceptance script does not actually preflight the readiness-stamped DB prerequisite it reports.

The acceptance doc says data legs run only when the DB is "migrated + role-provisioned" and has an "ACTIVE, readiness-stamped corpus", and that missing prerequisites are reported exactly (`work/09-jurisearch-cli/05-two-host-acceptance.md:24`). The script's DB preflight only counts rows in `jurisearch_control.corpus_state` whose `active_generation` is non-null; if that count is at least one, it prints `PREREQ-DB OK` (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:64`). It never checks that `public.index_manifest['query_readiness']` exists, is well-formed, and matches the active topology.

An active but unstamped or stale site DB would therefore be reported as DB-ready, then the data legs would fail later inside `serve-site`/handlers instead of identifying the stated missing prerequisite. That weakens the usefulness of the operated capture script and contradicts the doc's claim about exact preflight gating.

Fix direction: make the DB preflight validate the same readiness stamp condition the site read gate uses, or downgrade the script/doc wording so it only claims "active corpus present" and lets readiness failures happen during the live data legs.

## Notes

The prior global multi-corpus blocker appears resolved in the current source. `load_query_readiness_in_snapshot` now validates an aggregate active-topology signature instead of rejecting multiple active corpora, and `site::tests::the_site_serves_a_multi_corpus_topology_through_the_read_role` passed locally.

The prior global WARN about the missing core-owned request DTO seam is mostly addressed by the new `jurisearch_core::site_request` module and the handler/client use of `Operation::parse_args`; the remaining gap is the `status` exception above.

## Validation

- `git status --short`
- `git diff --stat`
- `cargo test -p jurisearch-core site_request -- --nocapture`
- `cargo test -p jurisearch-client --test cli_acceptance -- --nocapture`
- `cargo test -p jurisearch-cli site::tests::the_site_serves_a_multi_corpus_topology_through_the_read_role -- --nocapture`
- `cargo test -p jurisearch-client --test dependency_cone -- --nocapture`
- `cargo test -p jurisearch-transport response_envelope -- --nocapture`
- `bash -n work/09-jurisearch-cli/scripts/single-host-acceptance.sh`
- Manual client probes for `status '{"bogus":true}'` versus `search '{"query":"x","bogus":true}'`

VERDICT: FIXES_REQUIRED
