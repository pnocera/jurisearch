## Findings

No BLOCKER/WARN/NIT findings.

## Checks Passed

The on-host execution risk from the prior review is now retired early enough. The plan adds Spike C before M1,
requires the journal/timer probes to run as the eventual dashboard identity, and separately proves a minimal
`Bun.serve` bind on the configured tailnet address (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:57-67`).
That addresses the previous gap where M2-M5 could pass over fixtures before discovering box-specific journal or
bind failures. The plan's premise still matches the current baseline: producer units render only
`User={user}`/`Group={user}` with no supplementary journal group (`crates/jurisearch-producer/src/render.rs:53-58`),
and `deploy.sh` currently only creates/preserves the `jurisearch` user without adding journal access
(`deploy.sh:294-300`).

Spike A is now a real packaging decision gate. On PASS, the self-contained binary path continues; on FAIL, the
plan requires M5/M6a/M6b and the DoD to be revised before implementation to define the adjacent-asset layout,
service working directory, audit allowances, checksums, staging/install, and deploy verification
(`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:28-41`, `:111-117`, `:159-162`). That is
coherent with `dist.sh` as it exists today: the tarball helper only packages `bin`, `config`, `systemd`,
`completions`, and `SHA256SUMS` (`dist.sh:329-335`), and the forbidden-asset audit rejects common runtime asset
names including `manifest.json` (`dist.sh:84-101`).

The `running` RunRecord contract is now non-optional and correctly tied to source behavior. Spike B requires
either a real captured running record or a source-derived synthetic validated against `RunRecord::started`, and
M1/M4 must assert neutral/in-progress plus null duration
(`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:43-55`, `:84-89`, `:104-109`,
`:167-169`). This matches the producer source: an in-flight record has `ended_at = None`,
`outcome = Running`, and `exit_class = "running"` (`crates/jurisearch-producer/src/runrecord.rs:69-88`), while
class-only mapping would otherwise classify unknown strings as permanent failure (`crates/jurisearch-producer/src/exit.rs:40-51`).

The former oversized M6 is now split at the right fault line. M6a is limited to `dist.sh` and bundle generation,
requiring a two-binary update-server bundle with both binaries in `SHA256SUMS` and `manifest.toml`, exact
`--version` audit, `bash -n`, `shellcheck`, and Codex GO (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:119-126`).
M6b separately covers `deploy.sh`: stage/verify/install both binaries, add the dashboard service/config, enable
it, and fail closed on active service, tailnet-only bind, and journal access before any live run
(`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:128-135`). That split lines up with the
current script boundaries: `dist.sh` is still Cargo-only for `BUILD_BINS` and one update-server binary
(`dist.sh:71-74`, `:217-265`, `:347-350`, `:446-449`), while `deploy.sh` still stages, swaps, and verifies only
`jurisearch-producer` (`deploy.sh:150-170`, `:261-267`, `:321-331`, `:449-507`).

The `--version` compatibility contract is now explicit at the right milestones. M0 requires the dashboard
binary to print exactly `jurisearch-dashboard <workspace-version> (<12-char-commit>, <target>)` using the same
workspace version, `JURISEARCH_BUILD_COMMIT`, and target values (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:73-82`),
M5 repeats that requirement for the compiled artifact (`:111-117`), and M6a proves both update-server binaries
pass the release audit (`:119-126`). That matches the current `dist.sh` exact-match audit
(`dist.sh:274-293`) and `deploy.sh`'s installed-vs-bundle version comparison (`deploy.sh:165-170`, `:502-507`).

The sequencing is coherent after the fixes. The plan runs all three spikes before M0/M1, gates M5 on Spike A,
keeps M4 parallel only after M1 shared contracts exist, runs M6a before M6b, and requires M6b's Codex GO before
any live deploy run (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:153-162`). The risk
table and Phase 1 DoD now restate the same gates without contradicting the milestone plan (`:164-180`).

VERDICT: GO
