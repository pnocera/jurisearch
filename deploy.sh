#!/usr/bin/env bash
# deploy.sh — JuriSearch update-server (producer) deployment to CT 111 (jurisearch-update).
#
# The reproducible DEPLOY half of the build/deploy pipeline (dist.sh is the BUILD half): it takes the
# repository-local `dist/update-server/` bundle that dist.sh produced and installs it onto the target
# host, then provisions + arms the producer so the box is "up and running":
#
#   bundle (dist/update-server) ──scp──▶ target ──install──▶ /usr/local/bin/jurisearch-producer
#                                                          ├─ /etc/jurisearch/producer.toml + 0600 secrets
#                                                          ├─ provision-db  → external PostgreSQL (CT 110)
#                                                          └─ install+enable systemd timers (legi/jurispr.)
#
# It is IDEMPOTENT and supports UPGRADE-IN-PLACE: a re-run against an existing deployment stops the
# timers, swaps the binary, re-converges the (idempotent) provision/units, and re-arms the timers —
# while PRESERVING the signing seed and any operator-edited config (regenerating the seed would
# invalidate every already-published signature, so the seed is install-once and never rotated here).
#
# Transport: the target is reached directly over Tailscale by SSH as root. No bear/pct tunnel — CT 111
# joined the tailnet. Per the operator: this is a private tailnet, so the well-known password 20Sense20
# is acceptable as the default for SSH and DB roles (override via the env vars below for other sites).
#
# What this deploy DOES (the definition of "up and running" here):
#   * binary installed, identity verified by SHA256 AND by `--version` (commit + target stamp);
#   * producer.toml present and `validate`-clean; all secrets present at mode 0600;
#   * `provision-db` converged against the external PostgreSQL;
#   * systemd timers installed (via `jurisearch-producer install`), enabled and active/waiting;
#   * `status` reported.
# What it does NOT do: run a full `update --group` (a ~1 GB DILA baseline pull + embedding). That first
# live update is the operator's call; the script prints the exact command and (with --smoke) proves
# DILA reachability via a no-download `fetch --dry-run`.
#
# Usage:
#   ./deploy.sh                 Install or upgrade the update-server on the default target.
#   ./deploy.sh --smoke         ... then run `fetch --source legi --dry-run` to prove DILA egress.
#   ./deploy.sh --no-provision  Skip provision-db (binary/config/units only).
#   ./deploy.sh --force-config   Overwrite an existing producer.toml from the bundle example.
#   ./deploy.sh --force-passwords  Overwrite existing DB password files (NEVER the signing seed).
#   ./deploy.sh --dry-run       Print the plan and run only read-only remote checks; mutate nothing.
#
# Environment overrides (all optional):
#   DEPLOY_HOST            target SSH host         (default 100.71.35.39 = jurisearch-update on tailnet)
#   DEPLOY_SSH_USER       target SSH user          (default root)
#   DEPLOY_SSH_PASS       target SSH password      (default 20Sense20)
#   DEPLOY_BUNDLE_DIR     bundle to deploy         (default <repo>/dist/update-server)
#   DEPLOY_PG_ADMIN_PASSWORD   external PG superuser password used by provision-db (default 20Sense20)
#   DEPLOY_PG_WRITER_PASSWORD  password set for the jurisearch_write role          (default 20Sense20)
#   DEPLOY_PG_READ_PASSWORD    password set for the jurisearch_read role           (default 20Sense20)
#   OPENROUTER_API_KEY    embedding provider key written into producer.env (optional; updates need it)

set -euo pipefail

# ---------------------------------------------------------------------------------------------------
# Paths & configuration
# ---------------------------------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
REPO_ROOT="$SCRIPT_DIR"

DEPLOY_HOST="${DEPLOY_HOST:-100.71.35.39}"
DEPLOY_SSH_USER="${DEPLOY_SSH_USER:-root}"
DEPLOY_SSH_PASS="${DEPLOY_SSH_PASS:-20Sense20}"
BUNDLE_DIR="${DEPLOY_BUNDLE_DIR:-$REPO_ROOT/dist/update-server}"

PG_ADMIN_PASSWORD="${DEPLOY_PG_ADMIN_PASSWORD:-20Sense20}"
PG_WRITER_PASSWORD="${DEPLOY_PG_WRITER_PASSWORD:-20Sense20}"
PG_READ_PASSWORD="${DEPLOY_PG_READ_PASSWORD:-20Sense20}"
OPENROUTER_API_KEY="${OPENROUTER_API_KEY:-}"

# Remote layout — MUST match the bundled producer.toml.example ([install] + [database] + [package]).
R_BIN="/usr/local/bin/jurisearch-producer"
R_ETC="/etc/jurisearch"
R_CONF="$R_ETC/producer.toml"
R_ENV="$R_ETC/producer.env"
R_SECRETS="$R_ETC/secrets"
R_SVC_USER="jurisearch"
R_STATE="/var/lib/jurisearch-producer"
R_STOREBOX="/srv/jurisearch/storebox"
TIMERS=(jurisearch-producer-legislation.timer jurisearch-producer-jurisprudence.timer)

DO_PROVISION=1
DO_SMOKE=0
DRY_RUN=0
FORCE_CONFIG=0
FORCE_PASSWORDS=0

while [ $# -gt 0 ]; do
  case "$1" in
    --no-provision)    DO_PROVISION=0;;
    --smoke)           DO_SMOKE=1;;
    --dry-run)         DRY_RUN=1;;
    --force-config)    FORCE_CONFIG=1;;
    --force-passwords) FORCE_PASSWORDS=1;;
    --host)            DEPLOY_HOST="${2:?--host needs a value}"; shift;;
    --bundle-dir)      BUNDLE_DIR="${2:?--bundle-dir needs a value}"; shift;;
    -h|--help)         sed -n '1,46p' "$0"; exit 0;;
    *) echo "deploy.sh: unknown argument '$1' (try --help)" >&2; exit 2;;
  esac
  shift
done

log()  { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
die()  { printf '\033[1;31mdeploy.sh: %s\033[0m\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------------------------------
# Local scratch (0700) for the generated secret candidates + remote provisioning script. Shredded on
# EXIT so password/seed material never lingers on the operator host.
# ---------------------------------------------------------------------------------------------------
LOCAL_TMP="$(mktemp -d "${TMPDIR:-/tmp}/jurisearch-deploy.XXXXXX")"
SSH_CTL="$LOCAL_TMP/ssh-ctl"
cleanup() {
  # Best-effort: close the multiplexed SSH master, then shred + remove the scratch dir.
  if [ -S "$SSH_CTL" ]; then
    SSHPASS="$DEPLOY_SSH_PASS" sshpass -e ssh -o ControlPath="$SSH_CTL" -O exit "$DEPLOY_HOST" 2>/dev/null || true
  fi
  if [ -d "$LOCAL_TMP" ]; then
    find "$LOCAL_TMP" -type f -exec shred -u {} + 2>/dev/null || true
    rm -rf "$LOCAL_TMP"
  fi
}
trap cleanup EXIT
chmod 700 "$LOCAL_TMP"

# ---------------------------------------------------------------------------------------------------
# SSH/SCP helpers — one multiplexed connection (ControlMaster), so sshpass authenticates ONCE and the
# password never reaches any argv (passed via the SSHPASS env to `sshpass -e`).
# ---------------------------------------------------------------------------------------------------
SSH_OPTS=(
  -o ControlMaster=auto -o ControlPath="$SSH_CTL" -o ControlPersist=120
  -o StrictHostKeyChecking=accept-new -o ConnectTimeout=12
  -o ServerAliveInterval=15 -o ServerAliveCountMax=4
)
rsh()  { SSHPASS="$DEPLOY_SSH_PASS" sshpass -e ssh "${SSH_OPTS[@]}" "$DEPLOY_SSH_USER@$DEPLOY_HOST" "$@"; }
rput() { SSHPASS="$DEPLOY_SSH_PASS" sshpass -e scp "${SSH_OPTS[@]}" "$@"; }

# ---------------------------------------------------------------------------------------------------
# Phase 1 — LOCAL preflight: tools, bundle integrity, and a hard check that the bundle binary is a
# VERSIONED build (refuse to deploy a pre-versioning artifact; re-run ./dist.sh to refresh it).
# ---------------------------------------------------------------------------------------------------
log "Phase 1/6 — local preflight"
for t in sshpass ssh scp sha256sum shred; do
  command -v "$t" >/dev/null 2>&1 || die "missing required local tool: $t"
done
[ -d "$BUNDLE_DIR" ] || die "bundle dir not found: $BUNDLE_DIR (run ./dist.sh first)"
BUNDLE_BIN="$BUNDLE_DIR/bin/jurisearch-producer"
BUNDLE_SUMS="$BUNDLE_DIR/SHA256SUMS"
BUNDLE_CONF="$BUNDLE_DIR/config/producer.toml.example"
for f in "$BUNDLE_BIN" "$BUNDLE_SUMS" "$BUNDLE_CONF"; do
  [ -f "$f" ] || die "bundle is incomplete, missing: $f"
done

# Verify the binary against the bundle's own SHA256SUMS (the line for ./bin/jurisearch-producer).
EXPECT_SHA="$(awk '$2=="./bin/jurisearch-producer"{print $1}' "$BUNDLE_SUMS")"
[ -n "$EXPECT_SHA" ] || die "SHA256SUMS has no entry for ./bin/jurisearch-producer"
ACTUAL_SHA="$(sha256sum "$BUNDLE_BIN" | awk '{print $1}')"
[ "$EXPECT_SHA" = "$ACTUAL_SHA" ] || die "bundle binary SHA mismatch (expected $EXPECT_SHA, got $ACTUAL_SHA)"
info "binary SHA256 verified against bundle SHA256SUMS: $ACTUAL_SHA"

# The bundle binary is x86_64-linux and runs on this x86_64-linux host; clap prints --version and exits
# before any app logic. A bundle built BEFORE the versioning change has no `--version` → fail loudly.
EXPECT_VERSION="$("$BUNDLE_BIN" --version 2>/dev/null || true)"
case "$EXPECT_VERSION" in
  jurisearch-producer\ *\(*,\ *\)) info "bundle --version: $EXPECT_VERSION";;
  *) die "bundle binary does not emit a versioned --version ('$EXPECT_VERSION'); rebuild with ./dist.sh";;
esac

# ---------------------------------------------------------------------------------------------------
# Phase 2 — generate secret candidates (local, 0600). DB passwords are deterministic (operator choice);
# the signing seed is a fresh 32-byte ed25519 seed as 64 hex chars. Candidates are STAGED but installed
# REMOTELY only where the destination is absent (install-once), so the seed is preserved across upgrades.
# ---------------------------------------------------------------------------------------------------
log "Phase 2/6 — prepare config + secrets"
umask 077
gen_seed() {
  if command -v openssl >/dev/null 2>&1; then openssl rand -hex 32
  else head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; fi
}
printf '%s' "$PG_ADMIN_PASSWORD"  > "$LOCAL_TMP/postgres-admin-password"
printf '%s' "$PG_WRITER_PASSWORD" > "$LOCAL_TMP/jurisearch-write-password"
printf '%s' "$PG_READ_PASSWORD"   > "$LOCAL_TMP/jurisearch-read-password"
gen_seed                          > "$LOCAL_TMP/producer-signing.seed"
# Sanity: the producer expects exactly 64 hex chars (a 32-byte seed).
[ "$(wc -c < "$LOCAL_TMP/producer-signing.seed" | tr -d ' ')" = "64" ] || die "generated signing seed is not 64 hex chars"

# producer.env (systemd EnvironmentFile, read by PID1 as root). Must EXIST or the units fail to start.
{
  echo "# JuriSearch producer runtime env (systemd EnvironmentFile, mode 0600). Operator secrets only."
  echo "# OPENROUTER_API_KEY powers producer-side document embedding; updates fail (config error) until set."
  if [ -n "$OPENROUTER_API_KEY" ]; then
    echo "OPENROUTER_API_KEY=$OPENROUTER_API_KEY"
  else
    echo "OPENROUTER_API_KEY="
  fi
} > "$LOCAL_TMP/producer.env"
[ -n "$OPENROUTER_API_KEY" ] || info "WARNING: no OPENROUTER_API_KEY provided; nightly updates will fail until \
it is set in $R_ENV (deploy/provision/timers still come up)."

# producer.toml from the bundle example, with read_password_file UNCOMMENTED (we set a read password and
# CT 110 uses password auth). Everything else already matches the CT topology.
sed -E 's|^# (read_password_file = "/etc/jurisearch/secrets/jurisearch-read-password")|\1|' \
  "$BUNDLE_CONF" > "$LOCAL_TMP/producer.toml"
grep -q '^read_password_file = ' "$LOCAL_TMP/producer.toml" || die "failed to enable read_password_file in producer.toml"

# ---------------------------------------------------------------------------------------------------
# Phase 3 — REMOTE preflight (read-only): reachability, systemd, arch, psql, external PG reachability,
# and detect FRESH vs UPGRADE. Establishes the multiplexed master connection.
# ---------------------------------------------------------------------------------------------------
log "Phase 3/6 — remote preflight ($DEPLOY_SSH_USER@$DEPLOY_HOST)"
PRE="$(rsh 'bash -s' <<REMOTE_PRE 2>&1)" || die "remote preflight failed (cannot SSH or shell error):\n$PRE"
set -e
echo "HOST=\$(hostname)"
echo "ARCH=\$(uname -m)"
echo "SYSTEMD=\$(pidof systemd >/dev/null 2>&1 && echo yes || echo no)"
echo "PSQL=\$(command -v psql >/dev/null 2>&1 && echo yes || echo no)"
if timeout 5 bash -c 'cat < /dev/null > /dev/tcp/192.168.0.110/5432' 2>/dev/null; then echo "PG=open"; else echo "PG=unreachable"; fi
echo "HAS_BIN=\$([ -x "$R_BIN" ] && echo yes || echo no)"
echo "HAS_SEED=\$([ -f "$R_SECRETS/producer-signing.seed" ] && echo yes || echo no)"
echo "OLD_VERSION=\$([ -x "$R_BIN" ] && "$R_BIN" --version 2>/dev/null || echo none)"
REMOTE_PRE
# shellcheck disable=SC2001  # prefix every captured line with 4 spaces — a line-anchor sub no ${//} can do
echo "$PRE" | sed 's/^/    /'
get() { echo "$PRE" | sed -n "s/^$1=//p" | head -1; }
[ "$(get ARCH)" = "x86_64" ]   || die "target arch is not x86_64: $(get ARCH)"
[ "$(get SYSTEMD)" = "yes" ]   || die "target is not running systemd as PID1"
[ "$(get PSQL)" = "yes" ]      || die "target has no psql client"
[ "$(get PG)" = "open" ]       || die "external PostgreSQL 192.168.0.110:5432 not reachable from target"
HAS_BIN="$(get HAS_BIN)"
MODE="install"; [ "$HAS_BIN" = "yes" ] && MODE="upgrade"
log "deployment mode: $MODE  (existing version: $(get OLD_VERSION))"

if [ "$DRY_RUN" = "1" ]; then
  log "DRY RUN — plan only; no remote mutation performed."
  info "would install binary $EXPECT_VERSION to $R_BIN"
  info "would write $R_CONF + 0600 secrets (seed install-once; HAS_SEED=$(get HAS_SEED))"
  info "would $( [ "$DO_PROVISION" = 1 ] && echo run || echo SKIP ) provision-db, install + enable: ${TIMERS[*]}"
  exit 0
fi

# ---------------------------------------------------------------------------------------------------
# Phase 4 — stage the bundle + secrets into a root-only remote temp dir.
# ---------------------------------------------------------------------------------------------------
log "Phase 4/6 — stage bundle + secrets"
STAGING="$(rsh 'umask 077; mktemp -d /tmp/jurisearch-deploy.XXXXXX')" || die "could not create remote staging dir"
[ -n "$STAGING" ] || die "empty remote staging dir path"
info "remote staging: $STAGING"
rput "$BUNDLE_BIN" "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/jurisearch-producer"
rput "$BUNDLE_SUMS" "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/SHA256SUMS"
rput "$LOCAL_TMP/producer.toml" "$LOCAL_TMP/producer.env" \
     "$LOCAL_TMP/postgres-admin-password" "$LOCAL_TMP/jurisearch-write-password" \
     "$LOCAL_TMP/jurisearch-read-password" "$LOCAL_TMP/producer-signing.seed" \
     "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/"
rsh "chmod 600 '$STAGING'/* && chmod 700 '$STAGING'"

# ---------------------------------------------------------------------------------------------------
# Phase 5 — remote install/upgrade. One self-contained script: user/dirs → stop timers → verify+swap
# binary → install-if-absent secrets → config → validate → provision-db → install+enable units.
# Flags travel as a non-secret header; secrets are the already-staged 0600 files. Staging is shredded.
# ---------------------------------------------------------------------------------------------------
log "Phase 5/6 — remote install ($MODE)"
INSTALL_OUT="$(rsh 'bash -s' <<REMOTE_INSTALL 2>&1)" && INSTALL_RC=0 || INSTALL_RC=$?
set -euo pipefail
STAGING="$STAGING"
EXPECT_SHA="$EXPECT_SHA"
DO_PROVISION="$DO_PROVISION"
FORCE_CONFIG="$FORCE_CONFIG"
FORCE_PASSWORDS="$FORCE_PASSWORDS"
R_BIN="$R_BIN"; R_ETC="$R_ETC"; R_CONF="$R_CONF"; R_ENV="$R_ENV"; R_SECRETS="$R_SECRETS"
R_SVC_USER="$R_SVC_USER"; R_STATE="$R_STATE"; R_STOREBOX="$R_STOREBOX"
TIMERS="${TIMERS[*]}"

cleanup_remote() { find "\$STAGING" -type f -exec shred -u {} + 2>/dev/null || true; rm -rf "\$STAGING"; }
trap cleanup_remote EXIT
say() { printf '    %s\n' "\$*"; }

# Verify the staged binary against the staged SHA256SUMS BEFORE installing anything.
STAGED_SHA="\$(sha256sum "\$STAGING/jurisearch-producer" | awk '{print \$1}')"
[ "\$STAGED_SHA" = "\$EXPECT_SHA" ] || { echo "staged binary SHA mismatch (\$STAGED_SHA != \$EXPECT_SHA)"; exit 1; }

# Service user (system, no login, home = state dir). Idempotent.
if ! id "\$R_SVC_USER" >/dev/null 2>&1; then
  useradd --system --shell /usr/sbin/nologin --home-dir "\$R_STATE" --create-home "\$R_SVC_USER"
  say "created system user \$R_SVC_USER"
else
  say "system user \$R_SVC_USER present"
fi

# Directories + ownership. The service (User=jurisearch) writes packages/archives/state; ProtectSystem=
# strict makes everything else read-only at runtime, so ownership here is what matters.
install -d -o root -g root -m 0755 "\$R_ETC"
install -d -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0700 "\$R_SECRETS"
install -d -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0750 "\$R_STATE"
for d in "\$R_STOREBOX" "\$R_STOREBOX/packages" "\$R_STOREBOX/archives" "\$R_STOREBOX/manifests" "\$R_STOREBOX/tmp"; do
  install -d -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0750 "\$d"
done

# Stop timers/service before swapping the binary (UPGRADE path; no-op on a fresh box).
for t in \$TIMERS; do systemctl stop "\$t" 2>/dev/null || true; done
systemctl stop 'jurisearch-producer-*.service' 2>/dev/null || true

# Guard: refuse to swap the executable out from under a STILL-RUNNING producer (a manual \`update\` or a
# unit that did not stop above). Replacing a live binary's file is exactly the unsafe window we avoid.
for s in jurisearch-producer-legislation.service jurisearch-producer-jurisprudence.service; do
  if systemctl is-active --quiet "\$s"; then echo "refusing binary swap: \$s is still active"; exit 1; fi
done

# Atomic, GUARDED binary swap: stage the new binary NEXT TO \$R_BIN (same dir = same filesystem, so the
# final mv is a rename, not a copy), set root:root 0755, re-verify its SHA against the expected bundle sum,
# then atomically rename into place. A torn/partial target is impossible: \$R_BIN is only ever the old
# file or the fully-written new one (\$\$ = this remote shell's PID, so the temp name can't collide).
NEW="\$R_BIN.new.\$\$"
install -o root -g root -m 0755 "\$STAGING/jurisearch-producer" "\$NEW"
NEW_SHA="\$(sha256sum "\$NEW" | awk '{print \$1}')"
if [ "\$NEW_SHA" != "\$EXPECT_SHA" ]; then rm -f "\$NEW"; echo "staged binary SHA mismatch at swap (\$NEW_SHA != \$EXPECT_SHA)"; exit 1; fi
mv -f "\$NEW" "\$R_BIN"
INSTALLED_VERSION="\$("\$R_BIN" --version 2>/dev/null || echo FAILED)"
say "installed binary: \$INSTALLED_VERSION"

# Secrets: install-if-absent (preserve the signing seed and, by default, existing passwords across
# upgrades). --force-passwords overwrites password files; the seed is NEVER force-rewritten here.
install_secret() { # <staged-name> <force?>
  local name="\$1" force="\$2" dst="\$R_SECRETS/\$1"
  if [ -f "\$dst" ] && [ "\$force" != "1" ]; then
    say "kept existing secret \$name"
  else
    install -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0600 "\$STAGING/\$name" "\$dst"
    say "wrote secret \$name (0600 \$R_SVC_USER)"
  fi
  # Converge metadata on EVERY path (fresh-write AND kept-on-upgrade): the producer config check only
  # rejects group/world MODE bits, so a drifted 0600 root:root seed/password passes validate/status as
  # root yet is UNREADABLE by the User=jurisearch update service → green deploy, failing first update.
  # Repair owner/group/mode WITHOUT touching contents — the seed stays install-once (bytes preserved).
  chown "\$R_SVC_USER:\$R_SVC_USER" "\$dst"
  chmod 0600 "\$dst"
}
install_secret postgres-admin-password  "\$FORCE_PASSWORDS"
install_secret jurisearch-write-password "\$FORCE_PASSWORDS"
install_secret jurisearch-read-password  "\$FORCE_PASSWORDS"
install_secret producer-signing.seed     0   # install-once; never rotate via deploy

# producer.env (systemd EnvironmentFile): refresh when a key was provided (non-empty line staged) or
# when absent; otherwise keep the existing operator file.
if [ ! -f "\$R_ENV" ] || grep -q '^OPENROUTER_API_KEY=.\+' "\$STAGING/producer.env"; then
  install -o root -g root -m 0600 "\$STAGING/producer.env" "\$R_ENV"; say "wrote \$R_ENV"
else
  say "kept existing \$R_ENV"
fi
# Converge metadata on EVERY path (fresh-write AND kept-on-upgrade): producer.env is the systemd
# EnvironmentFile read by PID1 as root, so it is intentionally root:root 0600. A kept-existing file can
# carry drifted owner/mode (e.g. 0644 root:root) that validate/status never catches; repair it WITHOUT
# touching contents so the kept operator key survives. (Mirrors the install_secret convergence above.)
chown root:root "\$R_ENV"
chmod 0600 "\$R_ENV"

# producer.toml: install-if-absent unless --force-config (preserve operator edits on upgrade).
if [ ! -f "\$R_CONF" ] || [ "\$FORCE_CONFIG" = "1" ]; then
  install -o root -g root -m 0644 "\$STAGING/producer.toml" "\$R_CONF"; say "wrote \$R_CONF"
else
  say "kept existing \$R_CONF"
fi

# Validate strictly before doing anything that connects.
"\$R_BIN" validate --config "\$R_CONF"
say "validate: OK"

# Provision (converge) the external PostgreSQL: roles, db, extensions, schema. Idempotent.
if [ "\$DO_PROVISION" = "1" ]; then
  "\$R_BIN" provision-db --config "\$R_CONF"
  say "provision-db: OK"
else
  say "provision-db: SKIPPED (--no-provision)"
fi

# Render + install the systemd units (authoritative; uses [install] from producer.toml).
"\$R_BIN" install --config "\$R_CONF"
systemctl daemon-reload

# DEPLOY-SAFE ARM. The producer timers are Persistent=true, so a plain \`enable --now\` can immediately
# fire a missed-window CATCH-UP — running the heavy \`update\` (~1 GB DILA pull) mid-deploy if today's
# OnCalendar window has already passed. Seed each timer's persistent stamp to NOW first: systemd reads the
# stamp file's mtime as the LastTrigger, so a current stamp means "already triggered this window" and no
# catch-up fires. The unit names already carry the .timer suffix, so the stamp file is, e.g.,
# stamp-jurisearch-producer-legislation.timer. THEN enable + start (armed for the next window, no run now).
mkdir -p /var/lib/systemd/timers
for t in \$TIMERS; do : > "/var/lib/systemd/timers/stamp-\$t"; done
# shellcheck disable=SC2086
systemctl enable \$TIMERS
for t in \$TIMERS; do systemctl start "\$t"; done
say "timers armed (no catch-up): \$TIMERS"

# Fail closed: arming must NOT have started a producer run. If either service is active as a result of
# enabling/starting the timers, the stamp seeding did not suppress catch-up — abort loudly.
for s in jurisearch-producer-legislation.service jurisearch-producer-jurisprudence.service; do
  if systemctl is-active --quiet "\$s"; then echo "arming triggered \$s (unexpected heavy run); aborting"; exit 1; fi
done
say "verified: arming started no producer service"
REMOTE_INSTALL

# shellcheck disable=SC2001  # prefix every captured line with 4 spaces — a line-anchor sub no ${//} can do
echo "$INSTALL_OUT" | sed 's/^/    /'
[ "${INSTALL_RC:-1}" = "0" ] || die "remote install failed (rc=$INSTALL_RC); see output above"

# ---------------------------------------------------------------------------------------------------
# Phase 6 — verify the box is up: installed identity matches the bundle, timers armed, status reported.
# ---------------------------------------------------------------------------------------------------
log "Phase 6/6 — verify"
# Fail CLOSED: the remote block sets fail=1 on any broken invariant and \`exit \$fail\`; we capture its rc
# below and \`die\` on non-zero. No \`|| true\` — a down/disabled timer, or a heavy run that arming started,
# must turn the deploy RED instead of printing a false DONE.
VERIFY="$(rsh 'bash -s' <<REMOTE_VERIFY 2>&1)" && vrc=0 || vrc=$?
fail=0
echo "INSTALLED_VERSION=\$("$R_BIN" --version 2>/dev/null || echo FAILED)"
echo "INSTALLED_SHA=\$(sha256sum "$R_BIN" | awk '{print \$1}')"
echo "--- secret perms (each managed secret must be EXACTLY 600 jurisearch jurisearch) ---"
ls -l "$R_SECRETS"/ 2>/dev/null | awk 'NR>1{print \$1, \$3, \$4, \$9}'
# HARD fail-closed: a producer-config secret read as User=jurisearch must be 600 jurisearch:jurisearch.
# (producer.env is intentionally root:root 0600 — an EnvironmentFile read by systemd as root, NOT a
# producer-config secret — so it is NOT asserted here.) A drift here turns the deploy RED via \$fail.
for sec in postgres-admin-password jurisearch-write-password jurisearch-read-password producer-signing.seed; do
  f="$R_SECRETS/\$sec"
  if [ ! -e "\$f" ]; then echo "FAIL: managed secret missing: \$f"; fail=1; continue; fi
  meta="\$(stat -c '%a %U %G %n' "\$f")"
  if [ "\$meta" != "600 jurisearch jurisearch \$f" ]; then
    echo "FAIL: secret perms drift: '\$meta' (want '600 jurisearch jurisearch \$f')"; fail=1
  fi
done
# HARD fail-closed for the runtime EnvironmentFile (SEPARATE from the jurisearch loop above): producer.env
# is read by systemd as root, so it must be EXACTLY 600 root:root — not jurisearch-owned. A drift here
# (e.g. 0644 root:root preserved on the keep path) turns the deploy RED via \$fail.
echo "--- env file perms (producer.env must be EXACTLY 600 root root) ---"
if [ ! -e "$R_ENV" ]; then echo "FAIL: env file missing: $R_ENV"; fail=1; else
  env_meta="\$(stat -c '%a %U %G %n' "$R_ENV")"
  echo "\$env_meta"
  if [ "\$env_meta" != "600 root root $R_ENV" ]; then
    echo "FAIL: env file perms drift: '\$env_meta' (want '600 root root $R_ENV')"; fail=1
  fi
fi
echo "--- timers (each must be enabled + active) ---"
for t in ${TIMERS[*]}; do
  en="\$(systemctl is-enabled "\$t" 2>/dev/null)"; ac="\$(systemctl is-active "\$t" 2>/dev/null)"
  printf '%s enabled=%s active=%s\n' "\$t" "\$en" "\$ac"
  [ "\$en" = "enabled" ] || { echo "FAIL: \$t not enabled (\$en)"; fail=1; }
  [ "\$ac" = "active" ]  || { echo "FAIL: \$t not active (\$ac)"; fail=1; }
done
echo "--- producer services (must be inactive after a no-update deploy) ---"
for s in jurisearch-producer-legislation.service jurisearch-producer-jurisprudence.service; do
  if systemctl is-active --quiet "\$s"; then echo "FAIL: \$s active (a heavy run started)"; fail=1; else echo "\$s inactive"; fi
done
echo "--- next runs ---"
systemctl list-timers '${TIMERS[0]}' '${TIMERS[1]}' --no-pager 2>/dev/null | sed -n '1,4p'
echo "--- status ---"
# \`status\` reads on-disk state only and exits 0 EVEN ON A FRESH BOX — the no-baseline ("broken") state is
# encoded in its JSON (overall/published_head_sequence), not the exit code. So a NON-ZERO exit means
# \`status\` itself errored (bad config / unreadable state) = a real deploy failure → fail closed. This is
# safe on a legitimate fresh deploy because this script never runs \`update\`, yet status still exits 0.
if "$R_BIN" status --config "$R_CONF" 2>&1; then :; else src=\$?; echo "FAIL: status exited non-zero (rc=\$src)"; fail=1; fi
exit \$fail
REMOTE_VERIFY
# shellcheck disable=SC2001  # prefix every captured line with 4 spaces — a line-anchor sub no ${//} can do
echo "$VERIFY" | sed 's/^/    /'
[ "${vrc:-1}" = "0" ] || die "remote verification failed (rc=$vrc); see output above"

# Cross-check the installed binary identity against the bundle we shipped.
INSTALLED_VERSION="$(echo "$VERIFY" | sed -n 's/^INSTALLED_VERSION=//p' | head -1)"
INSTALLED_SHA="$(echo "$VERIFY" | sed -n 's/^INSTALLED_SHA=//p' | head -1)"
[ "$INSTALLED_VERSION" = "$EXPECT_VERSION" ] || die "installed --version ('$INSTALLED_VERSION') != bundle ('$EXPECT_VERSION')"
[ "$INSTALLED_SHA" = "$EXPECT_SHA" ] || die "installed SHA ('$INSTALLED_SHA') != bundle ('$EXPECT_SHA')"
log "verified: installed binary matches the bundle ($INSTALLED_VERSION)"

# Optional: prove DILA egress without downloading anything (no DB/embedding involved).
if [ "$DO_SMOKE" = "1" ]; then
  log "smoke — fetch --source legi --dry-run (proves DILA reachability; no download)"
  rsh "'$R_BIN' fetch --source legi --dry-run --config '$R_CONF' 2>&1 | sed 's/^/    /'" || \
    info "WARNING: smoke fetch --dry-run returned non-zero (DILA listing/egress?)."
fi

log "DONE — update-server ${MODE}ed and armed on $DEPLOY_HOST."
info "First live publish (operator step; needs OPENROUTER_API_KEY in $R_ENV):"
info "  ssh $DEPLOY_SSH_USER@$DEPLOY_HOST '$R_BIN update --config $R_CONF --group legislation'"
