## Findings

### BLOCKER: `is_tailnet_ipv6` still accepts malformed triple-colon IPv6 literals before remote mutation

`deploy.sh:154` tries to reject multiple `::` with the glob `*::*::*`, but that does not catch overlapping pairs inside `:::`. As a result, malformed values such as `fd7a:115c:a1e0:::1` and `fd7a:115c:a1e0:::` pass `is_tailnet_ipv6`, render into both `dashboard.toml` and `ExecStart`, and only fail later when the dashboard cannot bind or Phase 6 cannot match the address.

This is not a remaining second-flag or non-tailnet exposure bypass: the accepted strings still contain only hex/colon characters, the rendered `ExecStart` remains exactly seven tokens, and the Phase 6 `tailscale ip`/listener checks would fail closed. It is still a blocker for this gate because the stated safety contract is an author-time structural IPv6 validator that rejects malformed values before any remote mutation/start; these malformed bind values currently get past that boundary.

Concrete fix: make the `::` check reject overlapping compression markers as well. For example, explicitly reject `*:::*` before the existing group-count logic, or replace the glob check with a split/count of exact `::` occurrences plus an explicit ban on any `:::` substring. Keep the existing prefix, charset, hextet-length, and group-count checks.

## Notes

The original r2 flag-injection blockers are otherwise closed. `DEPLOY_DASHBOARD_BIND` values containing whitespace, `-`, `%`, `$`, backticks, Unicode, embedded IPv4, public/LAN IPv4, and outside-prefix IPv6 are refused before rendering. `DEPLOY_DASHBOARD_PORT` refuses empty, whitespace/comment injection, signs, non-digits, privileged ports, and out-of-range ports; accepted bind/port pairs render an `ExecStart` with exactly seven tokens.

Valid IPv6 forms such as `fd7a:115c:a1e0::1` and `fd7a:115c:a1e0:0:0:0:0:1` are accepted, including uppercase input after lowercasing for validation. Leading-zero decimal ports are treated as base-10 by `10#$p`; Bun's TOML parser and the dashboard CLI also parse `08787` as 8787, so I did not find an octal bypass there.

The previously good deployment guards still appear intact: the IPv4 allow-list is positive `100.64.0.0/10` with decimal octet arithmetic; Phase 6 cross-checks the expected bind against `tailscale ip`, normalizes bracketed IPv6 listeners, and fails any extra listener on the dashboard port; the journald checks assert both `runuser` access and the live process's `systemd-journal` gid; producer upgrade-safety still stops/verifies inactive units before swapping; and both producer/dashboard binaries go through local SHA/version, remote staged SHA, swap-time SHA, and post-install SHA/version checks.

Validation performed locally: `bash -n deploy.sh` returned 0 with only the three pre-existing here-doc warnings, `shellcheck deploy.sh` returned 0, and `git diff --check -- deploy.sh` returned 0. I also ran a local `--render-only` matrix covering clean tailnet IPv4/IPv6, valid ports, injection attempts, malformed/out-of-range binds and ports, and token counts; the only unexpected accepts were the triple-colon IPv6 forms above.

VERDICT: FIXES_REQUIRED
