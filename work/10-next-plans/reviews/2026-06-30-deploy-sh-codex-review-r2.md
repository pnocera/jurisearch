## Findings

### WARN: preserved secrets can keep the wrong owner and still pass deployment

Refs: [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:319), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:321), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:328), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:398), [config.rs](/home/pierre/Work/jurisearch/crates/jurisearch-producer/src/config.rs:583)

The fresh-install path writes password files and the signing seed as `0600 jurisearch:jurisearch`, but the upgrade/preservation path returns early for every existing secret when it is not being force-overwritten. That preserves the seed contents, which is correct, but it also preserves a bad owner/group/mode if the host has drifted or was bootstrapped by hand. The producer-side permission check only rejects group/world-accessible mode bits; a `0600 root:root` seed or password passes `validate` and `status` when run as root during deploy, then the systemd `User=jurisearch` update service cannot read it. Phase 6 only prints `ls -l` output, so this can still end in a green deploy with timers armed but first live update failing.

Concrete fix: after each `install_secret` keep/write decision, converge the destination metadata without changing contents: `chown "$R_SVC_USER:$R_SVC_USER" "$dst"` and `chmod 0600 "$dst"`. Keep the signing seed install-once by not replacing bytes, but still repair metadata. Add a hard Phase 6 assertion such as `stat -c '%a %U %G %n' "$R_SECRETS"/...` and fail unless each deployed secret is exactly `600 jurisearch jurisearch`.

## Prior Findings Re-verified

The previous blocker is resolved: the script now seeds `/var/lib/systemd/timers/stamp-$timer` before starting persistent timers, enables/starts the timers separately, and asserts the legislation/jurisprudence services are inactive after arming.

The previous fail-open verification warning is resolved for timer/status/binary identity: Phase 6 captures the remote block's exit code, checks each timer is enabled and active, checks the producer services are inactive, treats non-zero `status` as failure, and still cross-checks version and SHA locally.

The previous non-atomic binary replacement warning is resolved: the script installs the new binary to a temp path beside `$R_BIN`, verifies that temp file's SHA, and `mv -f`s it into place after checking the expected systemd services are inactive.

The previous `/etc/jurisearch` owner typo is resolved: the directory install now uses `-o root -g root -m 0755`.

## Notes

I did not find a regression in the dry-run path: it exits after local prep and read-only remote preflight, before staging or remote mutation. Secret transport is via local/staged files and `sshpass -e`, not password arguments. The preserved `producer.toml` behavior remains upgrade-friendly, with `--force-config` available for an explicit overwrite.

`bash -n deploy.sh` emits parse warnings for the heredocs embedded in command substitutions, but execution still parses successfully and `shellcheck` produced no findings in this environment. Moving the closing `)"` after each heredoc terminator would remove that noise, but I am not treating it as deploy-blocking.

VERDICT: FIXES_REQUIRED
