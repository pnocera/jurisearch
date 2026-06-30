## Findings

### BLOCKER: IPv4 validator still accepts ambiguous leading-zero dotted quads before remote mutation

`deploy.sh:169-175` accepts any all-digit dotted quad and then forces each octet through Bash base-10 arithmetic. That rejects range errors, but it does not require the rendered literal to be canonical decimal IPv4. As a result, malformed/ambiguous values such as `100.064.0.1` and `100.64.000.001` pass `is_tailnet_bind`, render into both `dashboard.toml` and the dashboard unit `ExecStart`, and reach the remote staging/install path before Phase 6 can fail them.

This violates the same author-time contract as the r3 IPv6 issue: the guard is supposed to reject anything that is not a strict tailnet literal before rendering or remote mutation. It is also not just cosmetic. On this host, libc resolves `100.064.0.1` as `100.52.0.1`, while strict JavaScript IP parsing rejects it as non-IP. The deploy script should not render a no-auth bind value whose downstream interpretation can differ from the allow-list check.

Concrete repro:

```sh
DEPLOY_DASHBOARD_BIND=100.064.0.1 DEPLOY_DASHBOARD_PORT=8787 \
  bash deploy.sh --render-only "$tmpdir"
```

This succeeds and renders:

```text
bind = "100.064.0.1"
ExecStart=/usr/local/bin/jurisearch-dashboard --config /etc/jurisearch/dashboard.toml --bind 100.064.0.1 --port 8787
```

Require canonical dotted decimal octets before doing the `100.64.0.0/10` arithmetic, for example by rejecting any octet with a leading zero unless the octet is exactly `0`.

### WARN: Port validator also renders non-canonical decimal strings

`deploy.sh:185-189` accepts `DEPLOY_DASHBOARD_PORT=08787` and `01024`, then renders those exact strings into both `dashboard.toml` and `ExecStart`. This is not a flag-injection bypass because the value is still digits only and the dashboard CLI parses decimal with `Number(...)`, but it is the same canonicality gap in the render contract: the authored file/unit contain a non-canonical port literal after a validator that claims to enforce strict numeric-port literals. Rejecting leading zeros, except for values where zero would already be out of range, would close this at the same boundary.

## Notes

The r3 blocker is resolved. The new `case "$a" in *:::*) return 1;; esac` check rejects the exact regression cases `fd7a:115c:a1e0:::1`, `fd7a:115c:a1e0:::`, and `fd7a:115c:a1e0::::`, while valid `fd7a:115c:a1e0::1` and `fd7a:115c:a1e0:0:0:0:0:1` still render successfully.

The other previously GO'd safeguards still appear intact: whitespace/flag injection is rejected before rendering, IPv4 is positively limited to the 100.64.0.0/10 numeric range for canonical inputs, IPv6 is limited to `fd7a:115c:a1e0::/48` with the triple-colon fix, Phase 6 cross-checks the expected bind against `tailscale ip` and fails extra listeners on the dashboard port, IPv6 listener comparison is bracket-normalized, journald access is asserted both through `runuser` and the running process gid, producer/dashboard binary swaps are stopped/inactive-guarded, and both binaries still go through SHA/version checks locally, in staging, at swap, and after install.

Validation performed locally: `bash -n deploy.sh` returned 0 with the same three here-doc warnings, `shellcheck deploy.sh` returned 0, and `git diff --check -- deploy.sh` returned 0. I also ran `--render-only` probes for the requested triple-colon rejects, the two requested valid IPv6 accepts, tailnet/out-of-range IPv4 cases, wildcard and embedded-IPv4 rejects, injection rejects, and port range/injection rejects.

VERDICT: FIXES_REQUIRED
