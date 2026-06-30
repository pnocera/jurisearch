## Findings

### BLOCKER: `DEPLOY_DASHBOARD_BIND` is not actually constrained to the tailnet

`deploy.sh:134-137` only rejects empty and a few wildcard spellings, and `deploy.sh:680-689` then treats whatever string the operator supplied as the expected-good listener. An accidental `DEPLOY_DASHBOARD_BIND=192.168.0.111` or other non-tailnet explicit interface would render into both `dashboard.toml` and `ExecStart`, the dashboard would start there, and Phase 6 would pass because the listener exactly matches the non-tailnet value. That violates the load-bearing "tailnet-only/no-auth" guarantee.

Concrete fix: make the deploy-time guard validate the bind as a tailnet address, not just "not wildcard". For CT 111 this can be a strict literal IP allow-list/prefix check (`100.64.0.0/10` and the expected Tailscale IPv6 prefix, if IPv6 is supported), or a remote check against `tailscale ip -4/-6` before starting the service. Phase 6 should also verify the listener address is in that same allowed tailnet set.

### BLOCKER: Phase 6 does not enforce "ONLY on `${BIND}:${PORT}`"

`deploy.sh:681-689` fails wildcard listeners and requires one expected listener, but it does not fail additional non-wildcard listeners on the same port. A local address list containing both `100.71.35.39:8787` and `192.168.0.111:8787` would pass today. That is a false green for the stated invariant at `deploy.sh:680`: "must be `${DASHBOARD_BIND}:${DASHBOARD_PORT} ONLY`".

Concrete fix: after collecting `listeners`, normalize the expected address and fail if any non-empty listener is not exactly the expected local address. For example, iterate each `$addr` and set `fail=1` unless it equals the normalized expected address; keep the wildcard-specific message as an earlier clearer diagnostic.

### WARN: IPv6 tailnet binds will false-red in listener verification

If `DEPLOY_DASHBOARD_BIND` is an explicit Tailscale IPv6 address, `ss` reports local addresses in bracketed form such as `[fd7a:...]:8787`, while `deploy.sh:686` compares against the unbracketed `${DASHBOARD_BIND}:${DASHBOARD_PORT}`. The service can be correctly bound but still fail Phase 6.

Concrete fix: either render/require bracketed IPv6 consistently for verification, or normalize both sides by parsing the local address/port from `ss` and stripping one layer of brackets before comparison.

### WARN: the render-time wildcard guard is weaker than the dashboard's runtime guard

`deploy.sh:134-137` rejects `0.0.0.0`, `::`, `[::]`, `*`, and empty, but the dashboard bind guard explicitly rejects other all-interface spellings such as `0`, `0.0`, `000.000.000.000`, `0x0`, `0:0:0:0:0:0:0:0`, and IPv4-mapped unspecified forms. Those inputs are accepted into the rendered unit/config, then fail only after Phase 5 has installed and attempted to start the service.

Concrete fix: share the same normalization policy in shell, or simplify by accepting only explicit tailnet literal forms as above. That makes malformed/wildcard overrides fail before any remote mutation.

### WARN: journald verification proves group readability, not the running unit's actual groups

`deploy.sh:695` uses `runuser -u jurisearch -g systemd-journal`, which starts a probe process with `systemd-journal` as its primary group. The deployed unit uses `Group=jurisearch` plus `SupplementaryGroups=systemd-journal` (`deploy.sh:178-182`). The probe is useful, but it does not directly verify that systemd applied the supplementary group to the running dashboard process.

Concrete fix: keep the `journalctl` probe, but also check the running unit's `MainPID` group set after `enable --now` by comparing `/proc/$pid/status` `Groups:` against `getent group systemd-journal`, or assert `systemctl show jurisearch-dashboard.service -p SupplementaryGroups` and the live process group list.

## Notes

I did not find a producer upgrade-safety regression in the `swap_binary` refactor: the producer stop/active guard, install-next-to-destination, SHA recheck, and final `mv -f` behavior are preserved, and the secret/config/timer paths remain additive aside from the new dashboard work. Both bundle binaries are SHA-checked locally, SHA-checked after staging, SHA-checked during swap, and identity-checked again in Phase 6.

`bash -n deploy.sh` still reports only the three pre-existing here-doc warnings described in the review instructions, and `git diff --check -- deploy.sh` is clean.

VERDICT: FIXES_REQUIRED
