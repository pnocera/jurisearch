## Findings

### WARN: preserved `producer.env` can keep drifted secret-file metadata

Refs: [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:339), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:341), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:342), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:344), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:409), [dist/update-server/config/producer.toml.example](/home/pierre/Work/jurisearch/dist/update-server/config/producer.toml.example:85)

The managed producer-config secrets now converge correctly, but the adjacent runtime EnvironmentFile does not on the preserve path. When `OPENROUTER_API_KEY` is not provided and `$R_ENV` already exists, the script keeps the existing file contents and metadata. That preserves operator edits, which is correct, but it can also preserve a drifted mode/owner such as `0644 root:root` or `0600 jurisearch:jurisearch` while the deploy still verifies green. This file is explicitly the systemd `EnvironmentFile` and can contain operator credentials; the script comment says it is intentionally `root:root 0600`, but Phase 6 excludes it from any hard assertion.

Concrete fix: after the `$R_ENV` write/keep branch, always repair metadata without changing contents:

```bash
chown root:root "$R_ENV"
chmod 0600 "$R_ENV"
```

Then add a Phase 6 assertion for `$R_ENV` separately from the managed producer-config secrets, expecting exactly `600 root root $R_ENV`. Keep it out of the `jurisearch:jurisearch` secret loop because systemd reads the EnvironmentFile as root.

## Prior Finding Re-verified

The r2 managed-secret warning is resolved. `install_secret` now runs `chown "$R_SVC_USER:$R_SVC_USER" "$dst"` and `chmod 0600 "$dst"` after both the fresh-write branch and the kept-on-upgrade branch, so existing seed/password bytes are preserved while owner/group/mode converge. Phase 6 now hard-fails each managed secret unless `stat -c '%a %U %G %n'` is exactly `600 jurisearch jurisearch <path>`.

The signing seed remains install-once: `--force-passwords` is only passed to the three password files, while `producer-signing.seed` is called with force `0`. The fresh-write path still installs it from the staged seed, and the keep path repairs only metadata.

## Regression Checks

No additional regression found in the reviewed areas: `set -euo pipefail` is present locally and in the remote install block; the dry-run exits after local prep and read-only remote preflight, before staging or remote mutation; the heredoc command substitutions still parse under `bash -n` with the existing warning noise but no syntax failure; `producer.toml` still preserves operator edits unless `--force-config`; and the Phase 6 timer/service/status checks fail closed instead of printing a false-green deploy.

VERDICT: FIXES_REQUIRED
