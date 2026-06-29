# Codex Re-Review: M4 Site Deploy r2

## Findings

No BLOCKER/WARN/NIT findings.

## Verification

I reviewed `git diff main` and the source paths touched by the r2 fixes: `crates/jurisearch-deploy/src/ops/readiness.rs`, `crates/jurisearch-deploy/src/bin/jurisearchctl.rs`, `crates/jurisearch-deploy/src/ops/trust.rs`, `crates/jurisearch-deploy/src/validate.rs`, `crates/jurisearch-deploy/src/render.rs`, `crates/jurisearch-deploy/src/ops/doctor.rs`, `crates/jurisearch-deploy/src/ops/embed.rs`, and the relevant tests.

The previous blocker fixes hold against the source:

- `site readiness` and the `site install` start gate use the serving readiness path, where no active corpus, stale/missing readiness stamp, and fingerprint mismatch are hard failures; `site doctor` still uses the advisory no-active-corpus warning.
- Duplicate configured trust anchors with the same identity and different key material are refused before writes, including on an empty DB; `bootstrap_trust` refuses any conflict before installing anchors.
- Install now writes env files under `config_dir/generated` and bare unit names under `system.systemd_unit_dir`, with dry-run and uninstall messages matching those locations.
- `site doctor` now includes live embedder endpoint diagnostics, trust/license installed-anchor diagnostics when the DB is reachable, advisory no-active-corpus readiness, and active-corpus fingerprint checks through the `assemble_doctor_report` seam.
- `embed render-service --out` writes only the bge-m3 service/env subset.
- `site uninstall` removes the managed unit files and generated env files from the configured locations after disabling the units.

I also checked that the loopback embedder validation, entitlement/signature catch-up path, and argv/render safety guards were not weakened by these changes.

Commands run:

```text
cargo test -p jurisearch-deploy
cargo fmt --check
cargo clippy -p jurisearch-deploy --all-targets -- -D warnings
```

All passed.

VERDICT: GO
