## Findings

### BLOCKER: `systemctl enable --now` can immediately run the heavy update path

Refs: [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:344), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:347), [render.rs](/home/pierre/Work/jurisearch/crates/jurisearch-producer/src/render.rs:95)

`deploy.sh` renders timers whose unit files contain `Persistent=true`, then starts them with `systemctl enable --now`. For calendar timers, `Persistent=true` is specifically the catch-up behavior: when the timer is activated after a missed scheduled time, systemd may immediately trigger the service. Those services run `jurisearch-producer update --config ... --group ...`, which is the heavy DILA pull/publish path the review instructions say this deploy must not run. This can happen on a fresh install or after an upgrade if the daily `OnCalendar` window has already passed.

Concrete fix: do not start persistent timers in a way that performs catch-up during deploy. Either set the systemd timer stamp to "now" before starting each timer, or add a deploy-safe install/arm path that starts timers with catch-up disabled for initial activation. After arming, explicitly assert that `jurisearch-producer-legislation.service` and `jurisearch-producer-jurisprudence.service` are not active/running before declaring success.

### WARN: verification can false-green when status or timer health is broken

Refs: [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:357), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:368), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:372)

Phase 6 captures the remote verification block with `|| true`, and the `status` command inside that block is also forced successful with `|| true`. The only hard checks after verification are binary version and SHA. That means the script can print `DONE` even if `jurisearch-producer status --config ...` fails, timer units are disabled/inactive, or a timer immediately started an update service because of the `Persistent=true` issue above.

Concrete fix: make Phase 6 fail closed. Run the remote verification block under `set -euo pipefail`, remove both `|| true` suppressions, assert every expected timer is `enabled` and `active`, assert the producer services are not active immediately after deploy, and treat a non-zero `status` exit as deployment failure.

### WARN: the binary replacement is not atomic despite the script's safety claim

Refs: [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:294), [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:299)

The script says `install(1)` writes to a temp file and renames into place, but `install "$STAGING/jurisearch-producer" "$R_BIN"` is not a guaranteed atomic same-directory replacement. On a live host, that leaves an avoidable window for a truncated/partial target binary if the copy is interrupted, and it is less safe if a manual producer process is still running.

Concrete fix: install to a temporary path on the same filesystem, validate that file's hash/version, then `mv -f` it onto `$R_BIN`. Also verify no `jurisearch-producer-*.service` unit remains active before the swap.

### NIT: `/etc/jurisearch` directory install has a duplicated owner flag

Ref: [deploy.sh](/home/pierre/Work/jurisearch/deploy.sh:287)

`install -d -o root -o root -m 0755 "$R_ETC"` repeats `-o` and never specifies the group. It is probably harmless when run as root, but it is not the stated root:root convergence.

Concrete fix: change it to `install -d -o root -g root -m 0755 "$R_ETC"`.

## Notes

The signing seed is generated as 64 hex chars on fresh install and installed with owner `jurisearch:jurisearch`, mode `0600`; existing seeds are preserved on upgrade. The config/secret paths used by the script match the bundled `producer.toml.example` and the producer schema, including the uncommented `read_password_file`.

VERDICT: FIXES_REQUIRED
