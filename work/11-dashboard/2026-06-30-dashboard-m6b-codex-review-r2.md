## Findings

### BLOCKER: IPv6 `DEPLOY_DASHBOARD_BIND` is only prefix-checked, so it can inject extra `ExecStart` flags before Phase 6 catches it

`deploy.sh:140-143` treats any lower-cased string matching `fd7a:115c:a1e0:*` as an allowed tailnet IPv6 bind. That is not a structural IPv6 literal check; it also accepts values like `fd7a:115c:a1e0:zzzz`, `fd7a:115c:a1e0:*`, and, more importantly, whitespace-bearing strings such as `fd7a:115c:a1e0::1 --bind 192.168.0.111`. The value is then rendered unquoted into the systemd command at `deploy.sh:200`.

Because the dashboard flag parser applies later duplicate string flags by assignment (`apps/dashboard/server/src/config/flags.ts:78-90`), an injected second `--bind` can override the intended tailnet bind. The dashboard runtime bind guard only rejects wildcard/unspecified addresses, not LAN/public explicit addresses (`apps/dashboard/server/src/http/bind.ts:155-165`), so this can start the no-auth dashboard on a non-tailnet interface during Phase 5. Phase 6 would likely fail afterward because `exp_addr` still contains the original bad string and the listener-only/tailscale checks reject it, but the safety requirement here is that the render/real Phase 2 guard prevents a non-tailnet bind before any remote mutation/start.

Concrete fix: make `is_tailnet_bind` validate IPv6 as an actual literal in `fd7a:115c:a1e0::/48`, not a shell prefix string. At minimum reject any character outside IPv6 literal syntax and whitespace; preferably expand/parse the hextets and check the first three groups exactly. Keep the remote `tailscale ip` cross-check as the host-specific belt-and-suspenders check.

### BLOCKER: `DEPLOY_DASHBOARD_PORT` is unvalidated and can also inject extra dashboard flags into `ExecStart`

`DASHBOARD_PORT` is assigned directly from `DEPLOY_DASHBOARD_PORT` at `deploy.sh:98` and is rendered directly into both TOML (`deploy.sh:169`) and the systemd `ExecStart` (`deploy.sh:200`) with no numeric/range validation. A value such as `8787 # --bind 192.168.0.111` leaves the TOML `port = 8787 # ...` valid, while systemd still tokenizes the `ExecStart` line into additional arguments; the later injected `--bind` then wins in `parseCliFlags` as above.

This is another path to start the dashboard on a non-tailnet explicit interface before Phase 6 fails the deployment. It is a new shell/rendering bug in the no-auth safety boundary.

Concrete fix: validate `DASHBOARD_PORT` before any render path as an integer TCP port, e.g. decimal digits only and `1..65535` (or `1024..65535` if the deployment intentionally forbids privileged ports). Render only the sanitized integer. Do not allow comments, whitespace, signs, shell/systemd metacharacters, or empty values.

## Notes

The five previous issues are otherwise materially improved:

- The IPv4 side of `is_tailnet_bind` is a positive allow-list for `100.64.0.0/10`, forces decimal arithmetic with `10#`, caps octet length, rejects LAN/public/loopback/out-of-range/wildcard spellings, and has no octal leading-zero bypass.
- The Phase 6 listener loop (`deploy.sh:727-740`) maps every `ss` listener on the port, normalizes bracketed IPv6 in `norm_hostport`, fails any unexpected listener, requires the expected listener, and `exit $fail` is wired to the outer `die` path (`deploy.sh:769-773`), so it cannot false-green an extra LAN/wildcard listener once verification runs.
- The `tailscale ip -4/-6` cross-check (`deploy.sh:707-714`) is fail-closed and proves the expected address belongs to the host, assuming the service has not already been started with injected flags as described above.
- The runtime journald checks are fail-closed: the `runuser` probe is retained, the unit property is asserted, and `/proc/$MainPID/status` must contain the `systemd-journal` gid (`deploy.sh:742-767`).
- I did not find a regression in the producer upgrade-safety or the two-binary SHA/version chain. Both binaries are checked locally against `SHA256SUMS`, checked again in remote staging, checked during same-filesystem swap, and checked post-install against the shipped version/SHA.

Validation performed locally: `bash -n deploy.sh` reports only the three pre-existing here-doc warnings described in the brief, and `git diff --check -- deploy.sh` is clean.

VERDICT: FIXES_REQUIRED
