# Codex Review: M4 Site Deploy

## Findings

### BLOCKER: `site readiness` is green with no active corpus, so `site install` can start an empty site

The readiness classifier returns `Warn` for `active.is_empty()` (`readiness.no_active_corpus`) at `crates/jurisearch-deploy/src/ops/readiness.rs:53`, and `DiagnosticReport::is_green()` treats every non-`Fail` status as green at `crates/jurisearch-deploy/src/ops/mod.rs:115`. `site install` feeds that boolean directly into the hard start gate at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:388`.

That means a site with no active corpus can satisfy `readiness_green=true`; if the local embed endpoint is otherwise healthy, `may_start_site` allows `jurisearch-site` to start even though the acceptance gate says readiness/catch-up must not be green with no active corpus. The test at `crates/jurisearch-deploy/src/ops/readiness.rs:144` currently locks in the false-green behavior by asserting no active corpus is only advisory.

Actionable fix: make the readiness command/start gate use a hard readiness status where no active corpus is not green. Either return `Fail` from `classify_readiness` for `active.is_empty()` when used by `site readiness`, or split doctor-advisory classification from serving-gate classification so pre-catch-up doctor can warn while `site readiness` and install cannot pass.

### BLOCKER: duplicate configured trust anchors can silently overwrite each other during one bootstrap

`plan_anchor_installs` only compares each configured anchor against the already-installed DB anchors at `crates/jurisearch-deploy/src/ops/trust.rs:84`. It does not detect two entries in the same `site.toml` with the same `(key_id, key_epoch, purpose)` but different `public_key_hex`. With an empty DB, both are classified as `Install`; `bootstrap_trust` then calls syncd's upserting `install_trust_anchor` for each action at `crates/jurisearch-deploy/src/ops/trust.rs:177`, so the second configured row silently replaces the first. `validate_trust` checks key shape and purpose counts at `crates/jurisearch-deploy/src/validate.rs:223` but also does not reject duplicate identities.

This violates the "trust anchors are never silently replaced" gate despite the installed-anchor conflict check.

Actionable fix: reject duplicate configured anchor identities before any writes. Same identity plus different key should be a `Conflict`; same identity plus same key should either be rejected as duplicate config or collapsed idempotently. Add a unit test where two configured anchors conflict while no DB anchor is installed.

### BLOCKER: `site install` does not install the rendered systemd unit files where `systemctl` can find them

The dry-run text says units will be installed into `/etc/systemd/system` at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:327`, but the real install path writes the render output under `config.system.config_dir` at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:353`. The renderer places units in the relative `systemd/` subdirectory (`crates/jurisearch-deploy/src/render.rs:60`), so the default config writes units to `/etc/jurisearch/systemd/*.service`, then calls `systemctl enable jurisearch-*.service` by bare unit name at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:371`.

Unless the operator separately links or copies those files into a systemd unit search path, `daemon-reload`/`enable` will not manage the rendered units. This breaks install/lifecycle even though the render itself is deterministic and uses absolute paths internally.

Actionable fix: have install copy or symlink the rendered `systemd/*.service` files into `/etc/systemd/system` (or another explicit systemd unit directory) before `daemon-reload`, while keeping generated env files under `config.system.config_dir/generated`. Update dry-run and uninstall to match the actual installed locations.

### BLOCKER: `site doctor` omits the required trust/license and live embedder diagnostics

`classify_trust` exists and has tests, but `run_doctor` never loads installed trust anchors and never pushes its results. After DB reachability, `run_doctor` only calls `corpus_status`, `readiness_check`, and `active_corpus_note` at `crates/jurisearch-deploy/src/ops/doctor.rs:224`. Missing package/license anchors therefore are not reported by `site doctor`, despite the acceptance requirement for distinct trust/license diagnostics.

Similarly, `site doctor` only reuses `embed::structural_checks` at `crates/jurisearch-deploy/src/ops/doctor.rs:214`; it does not run the endpoint dimension probe or active-corpus fingerprint check from `embed_doctor` (`crates/jurisearch-deploy/src/ops/embed.rs:153`). A down bge-m3 endpoint or dimension mismatch is therefore absent from `site doctor` unless the operator runs `embed doctor` separately.

Actionable fix: in `run_doctor`, when DB is reachable, load package/license anchor counts via `jurisearch_storage::trust::load_trust_anchors` and push `classify_trust`. Also include the full `embed_doctor` report or the endpoint/fingerprint checks, while avoiding duplicate structural lines if desired. Add an integration-style unit seam so the `run_doctor` path, not just standalone classifiers, is tested.

### WARN: `embed render-service --out` writes all rendered site files, not just the bge-m3 service/env

`embed::render_service` correctly filters to the two bge-m3 files at `crates/jurisearch-deploy/src/ops/embed.rs:212`, but the CLI `--out` path ignores that filtered list for writes and calls `parsed.render().write_to(dir)` at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:295`. That writes site and syncd env/unit files too, while only reporting the bge-m3 files.

Actionable fix: write only the filtered `files` returned by `render_service`, preserving modes, or rename the command/output to make the full-render side effect explicit.

### WARN: `site uninstall` says it removes generated env files, but only removes systemd units

The command help and message say uninstall removes generated units/env files, but the implementation iterates only over `config.system.config_dir/systemd/<unit>` at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:424`. The generated env files under `generated/` are left behind.

Actionable fix: either remove the generated env files that belong to the managed units or adjust the command text to say env files are retained.

## Verification

I reviewed `git diff main` and the actual source under `crates/jurisearch-deploy/src/ops/`, `jurisearchctl.rs`, the M1-A validation/render code, and the wrapped `jurisearch-syncd` trust/catch-up/readiness symbols. I also ran:

```text
cargo test -p jurisearch-deploy
```

Result: 89 tests passed. The failures above are acceptance/logic holes not covered by the current pure tests.

VERDICT: FIXES_REQUIRED
