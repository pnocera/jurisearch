# Global work/09 rereview r2

## Findings

### BLOCKER 1 - The replacement P6 acceptance evidence still has not exercised the shipped `serve-site` binary.

The plan was narrowed so the physical two-host run is now a field runbook, but the new checked-in acceptance still requires "a SINGLE-HOST operated capture of the shipped `serve-site` + `jurisearch-client` binaries" and says P6 done is proven by the automated in-process E2E plus that single-host operated capture (`work/09-jurisearch-cli/04-implementation-plan.md:279`, `work/09-jurisearch-cli/04-implementation-plan.md:293`). The acceptance document repeats that the single-host script drives the shipped `jurisearch serve-site` and client binaries (`work/09-jurisearch-cli/05-two-host-acceptance.md:23`).

The observed block currently checked in does not do that. Its data legs are explicitly skipped because both prerequisites are missing: no migrated/readiness-stamped site DB and no embedder configuration (`work/09-jurisearch-cli/05-two-host-acceptance.md:113`, `work/09-jurisearch-cli/05-two-host-acceptance.md:120`, `work/09-jurisearch-cli/05-two-host-acceptance.md:123`). The block even says to rerun on a host with both prerequisites to capture `status`/`fetch`/bm25 `search` and the fetch hash (`work/09-jurisearch-cli/05-two-host-acceptance.md:123`). That means the checked-in operated evidence only proves the shipped client-side contract/connection diagnostics; it still does not prove a shipped `jurisearch serve-site` process can start against a site DB, answer `status`/`fetch`/`search`, or match a concrete fetch checksum.

The in-process tests are useful and passed locally, but they are not the same evidence as the plan's shipped-binary operated capture. Fix direction: run `scripts/single-host-acceptance.sh` on a host with a migrated/readiness-stamped site DB and a valid embedder setup, then replace the skipped data-leg block with real `status`, `fetch`, bm25 `search`, and fetch hash output; or narrow the plan/doc again so the checked-in operated capture is only claimed to cover client diagnostics while the service path is explicitly test-only.

### WARN 1 - The single-host script can still produce false-green acceptance output once prerequisites pass.

The script improved the DB preflight by checking active topology and the `query_readiness` signature, but it still does not enforce the same readiness condition or the success of the positive data legs. The preflight only selects `value->>'signature'` from `public.index_manifest` and compares it with the active topology (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:68`, `work/09-jurisearch-cli/scripts/single-host-acceptance.sh:74`, `work/09-jurisearch-cli/scripts/single-host-acceptance.sh:78`). The actual site read gate parses the full cached readiness object and rejects a missing, malformed, or stale stamp before returning the report (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:542`, `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:553`, `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:557`). A stamp with a matching top-level `signature` but a malformed/missing `report` would be reported as `PREREQ-DB OK` by the script and then fail inside the service.

The live data-leg branch has the same false-green shape. The listener wait only greps for `listening on` and does not fail if the server exits or never binds (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:112`). The `run()` helper always ends with `echo`, so a failed `status`, `fetch`, or `search` still returns success to the script (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:51`, `work/09-jurisearch-cli/scripts/single-host-acceptance.sh:117`, `work/09-jurisearch-cli/scripts/single-host-acceptance.sh:124`). The script then prints the final "done" section regardless (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:126`). For an acceptance artifact, positive legs need a `must_run` path that exits non-zero and labels the run failed if `serve-site` did not start or if `status`/`fetch`/`search` fails.

## Notes

The previous `status` strict-validation gap is fixed in the current source: `Operation::Status.parse_args` now strict-parses an empty `SiteStatusArgs`, the site health handler invokes that seam, and the client preflight rejects non-empty status args locally.

The previous readiness preflight gap is partially fixed: active-but-unstamped and stale-signature DBs are now classified before data legs. The remaining issue is malformed-but-matching stamps and command/listener success enforcement.

The physical two-host blocker is addressed at the design level by reclassifying the two-host run as an operator field runbook rather than a CI/dev acceptance gate. The remaining blocker above is about the replacement single-host shipped-binary evidence now required by the amended plan.

## Validation

- `git status --short --branch`
- `git diff --stat`
- CodeGraph status/context/explore for the site request seam and readiness path
- `cargo fmt --all -- --check`
- `cargo test -p jurisearch-core site_request -- --nocapture`
- `cargo test -p jurisearch-client --test cli_acceptance -- --nocapture`
- `cargo test -p jurisearch-cli site::handlers::tests::health_rejects_unsupported_status_args -- --nocapture`
- `cargo test -p jurisearch-cli site::tests:: -- --nocapture`
- `cargo test -p jurisearch-client --test dependency_cone -- --nocapture`
- `cargo test -p jurisearch-transport response_envelope -- --nocapture`
- `bash -n work/09-jurisearch-cli/scripts/single-host-acceptance.sh`
- `work/09-jurisearch-cli/scripts/single-host-acceptance.sh` (current workstation run skipped data legs for the same missing DB/embedder prerequisites recorded in the doc)

VERDICT: FIXES_REQUIRED
