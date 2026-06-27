# Global work/09 rereview r3

## Findings

### WARN 1 - The single-host client capture still can false-green the contract and skew diagnostics it claims to prove.

The amended P6 plan now says the checked-in single-host capture proves the shipped `jurisearch-client`
operator surface, including the contract seam plus connection/skew diagnostics
(`work/09-jurisearch-cli/04-implementation-plan.md:281`,
`work/09-jurisearch-cli/04-implementation-plan.md:282`,
`work/09-jurisearch-cli/04-implementation-plan.md:283`). The runbook repeats that this single-host capture
asserts the client's contract and connection diagnostics
(`work/09-jurisearch-cli/05-two-host-acceptance.md:26`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:27`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:28`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:29`).

The script's negative helper only checks for any non-zero exit
(`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:53`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:54`). That is not enough for the most important
contract-seam legs, because they are run against `tcp://127.0.0.1:1`
(`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:64`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:65`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:66`). If `jurisearch-client` regressed and
forwarded `index_dir` or `zone` to the server instead of rejecting them locally, the dead-port connection
refusal would still be non-zero and the script would pass those legs. The observed block currently shows
the right messages, and `cli_acceptance` covers them, but the operated capture script itself does not
assert the diagnostic it claims to assert.

The same gap applies to protocol skew. The plan says the single-host capture proves skew diagnostics, but
the script's no-server legs cover unknown command, unsupported fields, unreachable service, and bad URL
only (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:61`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:68`). There is no shipped-binary skew leg in the
captured block either (`work/09-jurisearch-cli/05-two-host-acceptance.md:97`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:117`). Fix direction: make the helper assert expected
stderr substrings/exit code per leg, and add a real old-server/non-versioned TCP response leg for the
shipped binary; or narrow the P6 text so skew is explicitly automated-test evidence, not single-host
operated capture evidence.

### WARN 2 - The serve-site data-leg branch can still report success without the required fetch/hash leg.

The plan and runbook describe the prerequisite-gated shipped `serve-site` process run as
`status`/`fetch`/`search` plus a fetch hash
(`work/09-jurisearch-cli/04-implementation-plan.md:284`,
`work/09-jurisearch-cli/04-implementation-plan.md:285`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:30`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:130`,
`work/09-jurisearch-cli/05-two-host-acceptance.md:131`). The script defaults `FETCH_ID` to empty and makes
the fetch and hash block conditional
(`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:24`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:154`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:160`). With a ready DB and embedder but no
`--fetch-id`, it would run `status` and bm25 `search`, skip `fetch` and the hash, then print the final
"all legs passed" section (`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:164`,
`work/09-jurisearch-cli/scripts/single-host-acceptance.sh:168`).

That does not affect the current workstation capture because the data legs were prerequisite-skipped, and
the plan now correctly treats the shipped `serve-site` process run as a field step rather than a CI/dev
gate. It does mean the field runbook can still produce incomplete acceptance evidence while claiming the
data legs passed. Fix direction: require `--fetch-id` before entering the data-leg branch, derive a known
ID from `status`/DB preflight, or downgrade the script's final success text when no fetch/hash was run.

## Notes

The previous `status` strict-validation gap is fixed in the current source. `Operation::Status.parse_args`
now strict-parses `SiteStatusArgs`, `HealthHandler` calls that seam, and the client preflight rejects
non-empty status args before any connection.

The previous data-leg command/listener false-green is mostly fixed: the script now has `must_run`,
checks that `serve-site` actually binds, validates active/stamped/stale readiness before attempting data
legs, and fails if a positive status/search command returns non-zero.

The prior blocker about requiring a checked-in two-host or shipped `serve-site` capture is addressed at the
plan level: the plan now classifies the shipped `serve-site` process run and the two-physical-host run as
prerequisite-gated field runbook steps, not automated gates.

## Validation

- `git status --short`
- `git diff --stat`
- CodeGraph status/context/explore for `Operation::parse_args`, `SiteRequest`, site handlers, and the
  readiness gate
- `cargo fmt --all -- --check`
- `cargo test -p jurisearch-core site_request -- --nocapture`
- `cargo test -p jurisearch-client --test cli_acceptance -- --nocapture`
- `cargo test -p jurisearch-cli site::handlers::tests::health_rejects_unsupported_status_args -- --nocapture`
- `cargo test -p jurisearch-cli site::tests:: -- --nocapture`
- `cargo test -p jurisearch-client --test dependency_cone -- --nocapture`
- `cargo test -p jurisearch-transport response_envelope -- --nocapture`
- `bash -n work/09-jurisearch-cli/scripts/single-host-acceptance.sh`
- `work/09-jurisearch-cli/scripts/single-host-acceptance.sh` (current workstation run still skipped data
  legs because DB/embedder prerequisites are absent)

VERDICT: FIXES_REQUIRED
