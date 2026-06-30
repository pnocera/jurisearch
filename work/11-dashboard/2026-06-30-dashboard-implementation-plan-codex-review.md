## Findings

WARN - The two spikes do not retire the on-host execution risks soon enough. Spike B captures `journalctl` and
`systemctl` output from CT 111, but it does not prove those commands work under the eventual dashboard identity
(`User=jurisearch` with `SupplementaryGroups=systemd-journal`) or that `Bun.serve` can bind the configured
tailnet address on CT 111 (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:35-42`,
`:67-72`, `:86-96`). The current source confirms the journal permission issue is real: producer units render as
`User={user}`/`Group={user}` with no supplementary journal group (`crates/jurisearch-producer/src/render.rs:55-56`),
and `deploy.sh` currently creates/uses the `jurisearch` user without adding journal access (`deploy.sh:294-300`).
If this is only discovered in M6, M2-M5 can all be green over fixtures while the Logs/Timers panels fail on the
box. Concrete fix: add a third pre-M1 spike, or extend Spike B, to run the exact probes as the dashboard user:
`journalctl -u jurisearch-producer-legislation.service -o json -n 1`, `systemctl list-timers ... -o json`, and a
minimal Bun server bound to the configured tailnet address; record the commands and outputs as fixture/proof
artifacts. Keep the M6 deploy-time checks, but do not make them the first proof.

WARN - The fallback from Spike A is not carried through the milestone plan. Spike A permits falling back to
shipping a `dist/` directory beside the binary (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:27-33`),
but M5, M6, and the Phase-1 DoD still require a single self-contained binary with no filesystem `dist/`
(`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:81-84`, `:134-137`). The existing
`dist.sh` tarball helper only includes `bin`, `config`, `systemd`, `completions`, and `SHA256SUMS`
(`dist.sh:329-335`), and its bundle audit forbids common runtime asset names such as `manifest.json`
(`dist.sh:84-101`), so an asset-directory fallback would need explicit packaging, audit, checksum, deploy, and
service working-directory rules. Concrete fix: make Spike A a decision gate: if embedding passes, continue with
the current M5-M7; if it fails, update M5-M7 before implementation to define the adjacent-asset bundle layout,
allowed audit paths, SHA/version verification, remote staging/install, and deploy verification. Otherwise the
fallback is documented but not executable.

WARN - The fixture contract can still miss the most important transient run state. Spike B says to capture a
`running` RunRecord only "if catchable" (`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:35-40`),
while the producer explicitly persists in-flight records with `outcome = Running`, `ended_at = None`, and
`exit_class = "running"` (`crates/jurisearch-producer/src/runrecord.rs:70-82`). Because `exit_code_for` would
bucket unknown strings as permanent failure if severity were derived from the class alone
(`crates/jurisearch-producer/src/exit.rs:40-51`), this case needs a non-optional contract test. Concrete fix:
require either a real captured running record or a source-derived synthetic fixture checked against
`RunRecord::started`'s shape, and make M1/M4 tests assert `running` renders as neutral/in-progress with null
duration.

WARN - M6 is scoped as one large script milestone even though it touches multiple high-risk deployment axes:
non-Cargo build integration, bundle manifest changes, two-binary local verification, remote staging/install,
config templating, systemd service lifecycle, journal access, and bind verification
(`work/11-dashboard/02-update-server-dashboard-implementation-plan.md:86-96`). The current scripts are strongly
single-binary in several places: `UPDATE_SERVER_BINS` has only `jurisearch-producer` (`dist.sh:71-74`),
`BUILD_BINS` is Cargo-only (`dist.sh:217-265`), the update-server manifest lists one binary (`dist.sh:446-449`),
and `deploy.sh` validates/stages/swaps/verifies only `/usr/local/bin/jurisearch-producer`
(`deploy.sh:150-170`, `:261-267`, `:321-331`, `:449-507`). Concrete fix: split M6 into two reviewable gates:
M6a `dist.sh`/bundle integration that proves both binaries are in `SHA256SUMS` and `manifest.toml` with exact
`--version` stamps, then M6b `deploy.sh` integration that stages/installs/verifies both binaries plus the
dashboard config and unit. Keep the extra Codex gate before any live run on both sub-milestones.

NIT - The plan references `--version` parity but does not state the exact compatibility contract the scripts
currently enforce. `dist.sh` exact-matches `<binary> <workspace-version> (<12-char-commit>, <target>)`
(`dist.sh:274-293`), and `deploy.sh` captures the bundle binary's `--version` before comparing the installed
binary back to that bundle output (`deploy.sh:165-170`, `:502-507`). A Bun binary with a looser or differently
formatted version string could pass M0/M5 locally and still fail the release audit once M6 tries to generalise
the scripts. Concrete fix: add to M0/M5 DoD that `jurisearch-dashboard --version` exactly matches the existing
release format and is stamped from the same workspace version, `JURISEARCH_BUILD_COMMIT`, and target values used
by `dist.sh`.

## Checks Passed

The plan correctly characterises the current script baseline as Cargo-only and single-binary. `dist.sh` builds
all current release binaries with `cargo build --release --target ... --bin ...` (`dist.sh:217-265`), copies only
the update-server binaries from that Cargo output (`dist.sh:347-350`), writes per-bundle `SHA256SUMS`
(`dist.sh:324-326`, `:382`), and emits the top-level bundle manifest (`dist.sh:429-490`). `deploy.sh` similarly
performs a single producer binary SHA/version preflight, remote stage, guarded swap, and final installed identity
check (`deploy.sh:150-170`, `:261-267`, `:321-331`, `:502-507`).

The producer contract assumptions in the plan match source. `status` is built from on-disk state and the served
manifest without DB/network access (`crates/jurisearch-producer/src/status.rs:107-213`), reads the signed
`RemoteManifest` wrapper via `signed.payload` (`crates/jurisearch-producer/src/status.rs:245-264`), and exposes
the status fields the dashboard needs (`crates/jurisearch-producer/src/status.rs:94-104`). `RunRecord` contains
the stated persisted fields and no stored duration (`crates/jurisearch-producer/src/runrecord.rs:41-67`).
The exit-class set should be sourced from `SUCCESS_CLASSES`, `exit_code_for`, and `ProducerError::class`
(`crates/jurisearch-producer/src/exit.rs:15-51`; `crates/jurisearch-producer/src/error.rs:75-100`), as the plan
requires in M1.

VERDICT: FIXES_REQUIRED
