## Findings

None.

## Notes

The r4 canonicality gaps are closed. `is_tailnet_bind` now rejects any IPv4 octet with a leading zero unless the octet is exactly `0`, before the forced base-10 arithmetic and `100.64.0.0/10` range check. That keeps bare zero octets valid while refusing ambiguous literals such as `100.064.0.1`, `100.64.000.001`, `100.08.0.1`, and overlong octets before anything is rendered into `dashboard.toml` or `ExecStart`. `is_valid_port` now rejects leading-zero port strings after the digits-only check and before range evaluation, so `08787` and `01024` no longer render while `8787` still passes.

`--dashboard-only` is producer-safe in the reviewed diff. Local preparation skips producer secrets, env, and `producer.toml`; Phase 4 stages only the dashboard binary, `SHA256SUMS`, dashboard config, and dashboard unit; and the remote install guards all producer binary swaps, secret/env/config writes, `validate`, `provision-db`, producer unit install, timer stop, timer enable, timer start, and timer stamp mutation behind `DASHBOARD_ONLY != 1`. The only service stopped, swapped, enabled, and started in that mode is `jurisearch-dashboard.service`. Phase 6 reports producer timer state informationally in dashboard-only mode and does not set `fail=1` for disabled or inactive producer timers.

The `set -u` paths are covered: `EXPECT_SHA` and `EXPECT_VERSION` are assigned empty values in dashboard-only mode before the remote heredocs are expanded, and producer-only uses of the real values are gated off. The two remote heredocs remain syntactically valid in both modes under `bash -n`, with only the same pre-existing here-doc parser warnings noted below.

The default full path still performs the producer work when `--dashboard-only` is absent: producer bundle verification, staging, timer/service stop, guarded producer swap, secrets/env/config convergence, `validate`, optional `provision-db`, producer unit install, timer stamp seeding, timer enable/start, timer active/enabled verification, inactive-service verification, `status`, and optional smoke all remain on the full-deploy path. The new dashboard work runs in addition to that path but does not bypass or weaken the producer deploy and arming sequence.

The dashboard retains the expected safety treatment in dashboard-only mode. The locally rendered config and unit are validated before rendering; the unit passes explicit `--bind` and `--port` flags; the installed binary is SHA/version checked locally, in staging, at swap, and after install; Phase 6 requires the dashboard service to be active and enabled, cross-checks the configured bind against `tailscale ip`, rejects wildcard or extra listeners on the dashboard port, and verifies journald access plus the running MainPID's `systemd-journal` group.

Validation performed locally: `bash -n deploy.sh` returned 0 with the three existing here-doc warnings at lines 433, 497, and 748; `shellcheck deploy.sh` returned 0; `git diff --check -- deploy.sh` returned 0. No remote host contact was performed.

VERDICT: GO
