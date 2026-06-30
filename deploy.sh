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
#   ./deploy.sh --dashboard-only  Install/upgrade ONLY the read-only dashboard binary+service+config.
#                               Leaves the producer binary, secrets, provision-db, and (critically) the
#                               producer TIMERS completely untouched — never enable/start/stop them. Use
#                               this when producer ingestion is intentionally disabled (re-arming the
#                               timers would re-trigger the heavy update). Composes with --dry-run.
#   ./deploy.sh --render-only DIR  Render the dashboard [dashboard] config + systemd unit into DIR and
#                               exit (NO network, NO bundle needed). Used to inspect/validate templates.
#
# Environment overrides (all optional):
#   DEPLOY_HOST            target SSH host         (default 100.71.35.39 = jurisearch-update on tailnet)
#   DEPLOY_SSH_USER       target SSH user          (default root)
#   DEPLOY_SSH_PASS       target SSH password      (default 20Sense20)
#   DEPLOY_BUNDLE_DIR     bundle to deploy         (default <repo>/dist/update-server)
#   DEPLOY_DASHBOARD_BIND  tailnet addr the dashboard binds (default = DEPLOY_HOST; NEVER 0.0.0.0/wildcard)
#   DEPLOY_DASHBOARD_PORT  TCP port the dashboard listens on (default 8787)
#   DEPLOY_PG_ADMIN_PASSWORD   external PG superuser password used by provision-db (default 20Sense20)
#   DEPLOY_PG_WRITER_PASSWORD  password set for the jurisearch_write role          (default 20Sense20)
#   DEPLOY_PG_READ_PASSWORD    password set for the jurisearch_read role           (default 20Sense20)
#   OPENROUTER_API_KEY    embedding provider key written into producer.env (optional; updates need it)
#   PISTE_API_KEY              Judilibre KeyId         written into producer.env (optional; enables enrichment)
#   PISTE_OAUTH_CLIENT_ID      Legifrance client_id    written into producer.env (optional; enables enrichment)
#   PISTE_OAUTH_CLIENT_SECRET  Legifrance client_secret written into producer.env (optional; enables enrichment)
#                         (the three PISTE creds power Judilibre/Legifrance enrichment on the production PISTE env)

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
PISTE_API_KEY="${PISTE_API_KEY:-}"
PISTE_OAUTH_CLIENT_ID="${PISTE_OAUTH_CLIENT_ID:-}"
PISTE_OAUTH_CLIENT_SECRET="${PISTE_OAUTH_CLIENT_SECRET:-}"

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

# Dashboard (read-only update-server UI) remote layout. Same /usr/local/bin + /etc/jurisearch as the
# producer; a long-running systemd SERVICE (not a timer). The bind MUST be an explicit tailnet address
# (Spike C): the binary's own guard fails closed on a wildcard, and the unit passes the addr explicitly.
R_DASH_BIN="/usr/local/bin/jurisearch-dashboard"
R_DASH_CONF="$R_ETC/dashboard.toml"
R_DASH_SVC="jurisearch-dashboard.service"
R_DASH_UNIT="/etc/systemd/system/$R_DASH_SVC"
DASHBOARD_BIND="${DEPLOY_DASHBOARD_BIND:-$DEPLOY_HOST}"   # default: the tailnet addr we deploy to
DASHBOARD_PORT="${DEPLOY_DASHBOARD_PORT-8787}"            # default 8787 (>1024, Spike C); note the bare
                                                          # `-`: an explicitly-EMPTY override stays "" and
                                                          # is REJECTED by is_valid_port (not silently 8787)

DO_PROVISION=1
DO_SMOKE=0
DRY_RUN=0
FORCE_CONFIG=0
FORCE_PASSWORDS=0
RENDER_ONLY_DIR=""
DASHBOARD_ONLY="${DASHBOARD_ONLY:-0}"

while [ $# -gt 0 ]; do
  case "$1" in
    --no-provision)    DO_PROVISION=0;;
    --smoke)           DO_SMOKE=1;;
    --dry-run)         DRY_RUN=1;;
    --dashboard-only)  DASHBOARD_ONLY=1;;
    --force-config)    FORCE_CONFIG=1;;
    --force-passwords) FORCE_PASSWORDS=1;;
    --host)            DEPLOY_HOST="${2:?--host needs a value}"; DASHBOARD_BIND="${DEPLOY_DASHBOARD_BIND:-$DEPLOY_HOST}"; shift;;
    --bundle-dir)      BUNDLE_DIR="${2:?--bundle-dir needs a value}"; shift;;
    --render-only)     RENDER_ONLY_DIR="${2:?--render-only needs a DIR}"; shift;;
    -h|--help)         sed -n '1,53p' "$0"; exit 0;;
    *) echo "deploy.sh: unknown argument '$1' (try --help)" >&2; exit 2;;
  esac
  shift
done

log()  { printf '\n\033[1;34m==>\033[0m %s\n' "$*"; }
info() { printf '    %s\n' "$*"; }
die()  { printf '\033[1;31mdeploy.sh: %s\033[0m\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------------------------------
# Dashboard template rendering (Spike C requirements baked in). Pure/local — NO network. Both the
# real deploy (Phase 2 stages these) and `--render-only DIR` use the SAME functions, so what an
# operator inspects is byte-for-byte what gets installed.
# ---------------------------------------------------------------------------------------------------
# Fail closed at AUTHOR time on ANYTHING that is not a Tailscale tailnet address. A POSITIVE allow-list
# (not a wildcard blacklist) is what enforces the load-bearing tailnet-only/no-auth guarantee: a
# non-tailnet but non-wildcard interface (LAN 192.168.x, a public IP, loopback) would otherwise render
# into dashboard.toml + ExecStart and serve the no-auth UI off the tailnet. The allowed set is exactly:
#   * IPv4 in Tailscale's CGNAT range 100.64.0.0/10  (first octet 100, second octet 64-127), and
#   * IPv6 in Tailscale's ULA prefix  fd7a:115c:a1e0::/48.
# Requiring a positive match ALSO rejects every all-interface spelling the dashboard's own runtime guard
# rejects (0, 0.0, 000.000.000.000, 0x0, ::, 0:0:0:0:0:0:0:0, ::ffff:0.0.0.0, *) for free.
# bind + port are rendered into the systemd ExecStart line, so they MUST be validated as STRICT literals
# BEFORE any render/remote mutation — a value bearing whitespace or a `-`-prefixed token (e.g. a smuggled
# `--bind 192.168.0.111`) would inject a SECOND --bind that the dashboard's last-flag-wins parser honors,
# starting the no-auth UI off the tailnet during Phase 5 before Phase 6 could fail it. The validators
# below accept ONLY exact CGNAT-v4 / ULA-v6 / numeric-port literals — no space, no `-`, no metacharacter.

# Structurally-valid IPv6 literal (lowercased) confined to fd7a:115c:a1e0::/48 — NOT a prefix string
# match: rejects fd7a:115c:a1e0:zzzz, :*, embedded IPv4 (dots), and any whitespace/metacharacter.
is_tailnet_ipv6() { # <lowercased-addr>
  local a="$1" grp dcolon=0 explicit=0
  case "$a" in *[!0-9a-f:]*) return 1;; esac        # IPv6 literal charset ONLY (hex + colon)
  case "$a" in fd7a:115c:a1e0:*) : ;; *) return 1;; esac   # the /48 prefix, three explicit hextets
  case "$a" in *:::*) return 1;; esac                      # no ':::' (overlapping pairs the next glob misses)
  case "$a" in *::*::*) return 1;; *::*) dcolon=1;; esac    # at most one '::'
  case "$a" in :*) [ "${a:0:2}" = "::" ] || return 1;; esac # no lone leading ':'
  case "$a" in *:) [ "${a: -2}" = "::" ] || return 1;; esac # no lone trailing ':'
  local -a parts; IFS=: read -ra parts <<<"$a"      # non-WS IFS keeps empty fields from '::'
  for grp in "${parts[@]}"; do
    [ -n "$grp" ] || continue                       # empty field = the '::' compression slot
    [ "${#grp}" -le 4 ] || return 1                 # ≤4 hex digits per hextet (charset already hex-only)
    explicit=$((explicit + 1))
  done
  if [ "$dcolon" = 1 ]; then [ "$explicit" -le 7 ]; else [ "$explicit" -eq 8 ]; fi
}
is_tailnet_bind() { # <addr> → rc 0 iff a Tailscale tailnet literal (CGNAT v4 or ULA v6)
  local a="$1" lower="${1,,}" o1 o2 o3 o4 v
  case "$lower" in *:*) is_tailnet_ipv6 "$lower"; return $?;; esac   # any colon ⇒ IPv6 candidate
  [[ "$a" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || return 1   # else must be a dotted IPv4 quad
  IFS=. read -r o1 o2 o3 o4 <<<"$a"
  for v in "$o1" "$o2" "$o3" "$o4"; do
    [ "${#v}" -le 3 ] || return 1
    case "$v" in 0) ;; 0*) return 1;; esac         # CANONICAL decimal only: bare 0 ok, 00/08/064 rejected
                                                   # (libc would read 064 as octal 52 — a DIFFERENT, non-tailnet addr)
    [ "$((10#$v))" -le 255 ] || return 1           # base-10 forced: no octal pitfall on leading zeros
  done
  [ "$((10#$o1))" -eq 100 ] && [ "$((10#$o2))" -ge 64 ] && [ "$((10#$o2))" -le 127 ]  # 100.64.0.0/10
}
assert_tailnet_bind() {
  is_tailnet_bind "$DASHBOARD_BIND" || die "dashboard bind must be a Tailscale tailnet address \
(IPv4 100.64.0.0/10 or IPv6 fd7a:115c:a1e0::/48), got '$DASHBOARD_BIND'; set DEPLOY_DASHBOARD_BIND"
}

# Strict TCP port: decimal digits ONLY, range 1024..65535 (Spike C uses >1024 unprivileged; reject
# privileged/0/empty). Rejects whitespace, comments (`#`), signs, and any systemd-tokenizable junk so the
# unquoted ExecStart cannot gain an extra argument from the port field.
is_valid_port() { # <port>
  local p="$1"
  case "$p" in ''|*[!0-9]*) return 1;; esac
  case "$p" in 0*) return 1;; esac                 # CANONICAL: no leading zero (a valid port is >=1024)
  [ "${#p}" -le 5 ] || return 1
  [ "$((10#$p))" -ge 1024 ] && [ "$((10#$p))" -le 65535 ]
}
assert_dashboard_port() {
  is_valid_port "$DASHBOARD_PORT" || die "dashboard port must be a TCP port 1024..65535 (decimal digits \
only), got '$DASHBOARD_PORT'; set DEPLOY_DASHBOARD_PORT"
}

# Render the deployed [dashboard] config. Read-only, NO secrets (Phase 1 dashboard needs none). The
# explicit bind/port are ALSO passed as ExecStart flags (flags > toml), so the unit can never bind a
# wildcard even if the on-disk config were later hand-edited to one.
render_dashboard_config() { # <dest>
  assert_tailnet_bind
  assert_dashboard_port
  cat > "$1" <<EOF
# JuriSearch update-server dashboard config ([dashboard]) — rendered by deploy.sh.
# Read-only UI; no secrets. Resolution precedence in the binary is flags > env > this toml > defaults,
# and the systemd unit ALSO passes --bind/--port explicitly (so a wildcard can never be served).
[dashboard]
bind = "$DASHBOARD_BIND"
port = $DASHBOARD_PORT
producer_bin = "$R_BIN"
producer_config = "$R_CONF"
state_dir = "$R_STATE"
corpora_dir = "$R_STOREBOX/packages"
groups = ["legislation", "jurisprudence"]
EOF
}

# Render the dashboard systemd SERVICE unit. LOCKED requirements (Spike C): User/Group=jurisearch,
# SupplementaryGroups=systemd-journal (the log tail reads journald; jurisearch is NOT in that group by
# default and the unit-level grant is sufficient — proven by Spike C's `runuser -g systemd-journal`),
# Restart=always, and an ExecStart bound to the explicit tailnet addr (NEVER a wildcard). Deliberately
# UNHARDENED beyond least-privilege identity: the dashboard shells out to journalctl/systemctl (D-Bus
# to the systemd manager + /var/log/journal), paths a ProtectSystem=strict/PrivateTmp sandbox could
# break and that Spike C did NOT prove safe — so we match the proven identity exactly, nothing more.
render_dashboard_unit() { # <dest>
  assert_tailnet_bind
  assert_dashboard_port
  cat > "$1" <<EOF
[Unit]
Description=JuriSearch update-server dashboard (read-only, tailnet-only)
Documentation=https://github.com/juridia/jurisearch
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$R_SVC_USER
Group=$R_SVC_USER
# Spike C: REQUIRED for journalctl access (jurisearch is not in systemd-journal by default).
SupplementaryGroups=systemd-journal
ExecStart=$R_DASH_BIN --config $R_DASH_CONF --bind $DASHBOARD_BIND --port $DASHBOARD_PORT
Restart=always
RestartSec=2
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
EOF
}

# --render-only DIR: render BOTH templates and exit. No bundle, no SSH, no scratch dir — a pure local
# inspection/validation path (the M6b authoring DoD's network-free render check).
if [ -n "$RENDER_ONLY_DIR" ]; then
  mkdir -p "$RENDER_ONLY_DIR" || die "cannot create render dir: $RENDER_ONLY_DIR"
  render_dashboard_config "$RENDER_ONLY_DIR/dashboard.toml"
  render_dashboard_unit   "$RENDER_ONLY_DIR/$R_DASH_SVC"
  log "rendered dashboard templates into $RENDER_ONLY_DIR (no network performed)"
  info "$RENDER_ONLY_DIR/dashboard.toml"
  info "$RENDER_ONLY_DIR/$R_DASH_SVC"
  exit 0
fi

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
BUNDLE_DASH_BIN="$BUNDLE_DIR/bin/jurisearch-dashboard"
BUNDLE_SUMS="$BUNDLE_DIR/SHA256SUMS"
BUNDLE_CONF="$BUNDLE_DIR/config/producer.toml.example"
# --dashboard-only needs ONLY the dashboard binary + the sums; the full deploy also needs the producer
# binary + its config example (for producer.toml rendering).
REQUIRED_BUNDLE_FILES=("$BUNDLE_DASH_BIN" "$BUNDLE_SUMS")
[ "$DASHBOARD_ONLY" = 1 ] || REQUIRED_BUNDLE_FILES+=("$BUNDLE_BIN" "$BUNDLE_CONF")
for f in "${REQUIRED_BUNDLE_FILES[@]}"; do
  [ -f "$f" ] || die "bundle is incomplete, missing: $f"
done

# Verify each bundle binary against the bundle's own SHA256SUMS AND that it emits a versioned
# `--version` (refuse a pre-versioning artifact). DRY: one routine over BOTH binaries — the dashboard
# is held to the SAME identity discipline as the producer. The bundle binaries are x86_64-linux and run
# on this x86_64-linux host; clap/the binary prints --version and exits before any app logic. Results
# are stashed per-binary so Phases 4-6 can stage/verify each by name.
declare -A BIN_EXPECT_SHA BIN_EXPECT_VERSION
verify_bundle_binary() { # <basename>
  local name="$1" path expect actual ver
  path="$BUNDLE_DIR/bin/$name"
  expect="$(awk -v p="./bin/$name" '$2==p{print $1}' "$BUNDLE_SUMS")"
  [ -n "$expect" ] || die "SHA256SUMS has no entry for ./bin/$name"
  actual="$(sha256sum "$path" | awk '{print $1}')"
  [ "$expect" = "$actual" ] || die "bundle binary SHA mismatch for $name (expected $expect, got $actual)"
  ver="$("$path" --version 2>/dev/null || true)"
  case "$ver" in
    "$name"\ *\(*,\ *\)) : ;;
    *) die "bundle binary $name does not emit a versioned --version ('$ver'); rebuild with ./dist.sh";;
  esac
  BIN_EXPECT_SHA["$name"]="$actual"
  BIN_EXPECT_VERSION["$name"]="$ver"
  info "$name: SHA256 verified ($actual); --version: $ver"
}
# --dashboard-only verifies ONLY the dashboard binary (the producer install is left untouched).
[ "$DASHBOARD_ONLY" = 1 ] || verify_bundle_binary jurisearch-producer
verify_bundle_binary jurisearch-dashboard
# Keep the producer's existing variable names for the downstream remote heredocs; add dashboard peers.
# EXPECT_SHA/EXPECT_VERSION are ALWAYS assigned (empty in --dashboard-only, where the producer binary is
# never touched) so the heredoc headers can expand them under the outer `set -u` without an unbound error.
EXPECT_DASH_SHA="${BIN_EXPECT_SHA[jurisearch-dashboard]}"; EXPECT_DASH_VERSION="${BIN_EXPECT_VERSION[jurisearch-dashboard]}"
EXPECT_SHA=""; EXPECT_VERSION=""
if [ "$DASHBOARD_ONLY" != 1 ]; then
  EXPECT_SHA="${BIN_EXPECT_SHA[jurisearch-producer]}";     EXPECT_VERSION="${BIN_EXPECT_VERSION[jurisearch-producer]}"
fi

# ---------------------------------------------------------------------------------------------------
# Phase 2 — generate secret candidates (local, 0600). DB passwords are deterministic (operator choice);
# the signing seed is a fresh 32-byte ed25519 seed as 64 hex chars. Candidates are STAGED but installed
# REMOTELY only where the destination is absent (install-once), so the seed is preserved across upgrades.
# ---------------------------------------------------------------------------------------------------
log "Phase 2/6 — prepare config + secrets"
umask 077
gen_seed() {
  local s
  if command -v openssl >/dev/null 2>&1; then s="$(openssl rand -hex 32)"
  else s="$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"; fi
  printf '%s' "$s"
}
# Producer secrets/env/config — SKIPPED entirely in --dashboard-only (the producer install is left
# untouched; only the read-only dashboard is (re)deployed).
if [ "$DASHBOARD_ONLY" != 1 ]; then
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
  echo "# PISTE_API_KEY / PISTE_OAUTH_CLIENT_ID / PISTE_OAUTH_CLIENT_SECRET power Judilibre/Legifrance"
  echo "# enrichment (mode=auto); absence is honest + non-fatal (SkippedNoCredentials), not a config error."
  # Emit a managed-cred line ONLY when non-empty. The remote merge UPSERTS exactly these provided
  # keys and never blanks an omitted one, so an empty assignment must NOT be staged. The header lines
  # above always write, keeping the file non-empty/existent even when no creds are provided.
  [ -n "$OPENROUTER_API_KEY" ]        && echo "OPENROUTER_API_KEY=$OPENROUTER_API_KEY"
  [ -n "$PISTE_API_KEY" ]             && echo "PISTE_API_KEY=$PISTE_API_KEY"
  [ -n "$PISTE_OAUTH_CLIENT_ID" ]     && echo "PISTE_OAUTH_CLIENT_ID=$PISTE_OAUTH_CLIENT_ID"
  [ -n "$PISTE_OAUTH_CLIENT_SECRET" ] && echo "PISTE_OAUTH_CLIENT_SECRET=$PISTE_OAUTH_CLIENT_SECRET"
  :
} > "$LOCAL_TMP/producer.env"
[ -n "$OPENROUTER_API_KEY" ] || info "WARNING: no OPENROUTER_API_KEY provided; nightly updates will fail until \
it is set in $R_ENV (deploy/provision/timers still come up)."
{ [ -n "$PISTE_API_KEY" ] && [ -n "$PISTE_OAUTH_CLIENT_ID" ] && [ -n "$PISTE_OAUTH_CLIENT_SECRET" ]; } || \
  info "info: incomplete PISTE creds; Judilibre/Legifrance enrichment is SkippedNoCredentials until set in $R_ENV (deploy/provision/timers still come up)."

# producer.toml from the bundle example, with read_password_file UNCOMMENTED (we set a read password and
# CT 110 uses password auth). Everything else already matches the CT topology.
sed -E 's|^# (read_password_file = "/etc/jurisearch/secrets/jurisearch-read-password")|\1|' \
  "$BUNDLE_CONF" > "$LOCAL_TMP/producer.toml"
grep -q '^read_password_file = ' "$LOCAL_TMP/producer.toml" || die "failed to enable read_password_file in producer.toml"
else
  info "--dashboard-only: skipping producer secrets/env/config (producer install left untouched)"
fi

# Dashboard config + systemd unit — rendered from the SAME functions `--render-only` uses (Spike C
# requirements baked in). No secrets. Staged in Phase 4, installed in Phase 5.
render_dashboard_config "$LOCAL_TMP/dashboard.toml"
render_dashboard_unit   "$LOCAL_TMP/$R_DASH_SVC"
info "rendered dashboard config + unit (bind $DASHBOARD_BIND:$DASHBOARD_PORT, SupplementaryGroups=systemd-journal)"

# --dashboard-only --dry-run: print the dashboard-only plan with NO network and exit (mutate nothing).
# (The full-deploy --dry-run still runs its read-only remote preflight below — unchanged.)
if [ "$DASHBOARD_ONLY" = 1 ] && [ "$DRY_RUN" = 1 ]; then
  log "DRY RUN (--dashboard-only) — plan only; NO network, NO remote mutation."
  info "would verify + install ONLY $EXPECT_DASH_VERSION → $R_DASH_BIN (SHA $EXPECT_DASH_SHA)"
  info "would write $R_DASH_CONF + $R_DASH_UNIT (bind $DASHBOARD_BIND:$DASHBOARD_PORT, SupplementaryGroups=systemd-journal)"
  info "would: ensure user $R_SVC_USER + $R_ETC, ensure systemd-journal group, stop+swap+enable --now $R_DASH_SVC"
  info "would NOT touch: producer binary $R_BIN, secrets, $R_ENV, provision-db, OR producer timers (${TIMERS[*]} left exactly as-is)"
  info "Phase 6 would verify the DASHBOARD only (active+enabled, tailnet-only listener, journald); producer timers reported informationally, NEVER required"
  exit 0
fi

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
# psql + external-PG reachability are PRODUCER concerns (provision-db / updates). --dashboard-only never
# touches the DB, so these are not required in that mode (the DB may even be intentionally offline).
if [ "$DASHBOARD_ONLY" != 1 ]; then
  [ "$(get PSQL)" = "yes" ]      || die "target has no psql client"
  [ "$(get PG)" = "open" ]       || die "external PostgreSQL 192.168.0.110:5432 not reachable from target"
fi
HAS_BIN="$(get HAS_BIN)"
MODE="install"; [ "$HAS_BIN" = "yes" ] && MODE="upgrade"
log "deployment mode: $MODE  (existing version: $(get OLD_VERSION))"

if [ "$DRY_RUN" = "1" ]; then
  log "DRY RUN — plan only; no remote mutation performed."
  info "would install binary $EXPECT_VERSION to $R_BIN"
  info "would install binary $EXPECT_DASH_VERSION to $R_DASH_BIN"
  info "would write $R_CONF + 0600 secrets (seed install-once; HAS_SEED=$(get HAS_SEED))"
  info "would write $R_DASH_CONF + $R_DASH_UNIT (bind $DASHBOARD_BIND:$DASHBOARD_PORT, SupplementaryGroups=systemd-journal) and enable --now $R_DASH_SVC"
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
# Always stage the dashboard binary + SHA256SUMS + dashboard config/unit.
rput "$BUNDLE_DASH_BIN" "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/jurisearch-dashboard"
rput "$BUNDLE_SUMS" "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/SHA256SUMS"
rput "$LOCAL_TMP/dashboard.toml" "$LOCAL_TMP/$R_DASH_SVC" \
     "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/"
# --dashboard-only stages NOTHING producer-related (no producer binary, secrets, env, or config).
if [ "$DASHBOARD_ONLY" != 1 ]; then
  rput "$BUNDLE_BIN" "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/jurisearch-producer"
  rput "$LOCAL_TMP/producer.toml" "$LOCAL_TMP/producer.env" \
       "$LOCAL_TMP/postgres-admin-password" "$LOCAL_TMP/jurisearch-write-password" \
       "$LOCAL_TMP/jurisearch-read-password" "$LOCAL_TMP/producer-signing.seed" \
       "$DEPLOY_SSH_USER@$DEPLOY_HOST:$STAGING/"
fi
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
DASHBOARD_ONLY="$DASHBOARD_ONLY"
EXPECT_SHA="$EXPECT_SHA"
EXPECT_DASH_SHA="$EXPECT_DASH_SHA"
DO_PROVISION="$DO_PROVISION"
FORCE_CONFIG="$FORCE_CONFIG"
FORCE_PASSWORDS="$FORCE_PASSWORDS"
R_BIN="$R_BIN"; R_ETC="$R_ETC"; R_CONF="$R_CONF"; R_ENV="$R_ENV"; R_SECRETS="$R_SECRETS"
R_SVC_USER="$R_SVC_USER"; R_STATE="$R_STATE"; R_STOREBOX="$R_STOREBOX"
R_DASH_BIN="$R_DASH_BIN"; R_DASH_CONF="$R_DASH_CONF"; R_DASH_SVC="$R_DASH_SVC"; R_DASH_UNIT="$R_DASH_UNIT"
TIMERS="${TIMERS[*]}"

cleanup_remote() { find "\$STAGING" -type f -exec shred -u {} + 2>/dev/null || true; rm -rf "\$STAGING"; }
trap cleanup_remote EXIT
say() { printf '    %s\n' "\$*"; }

# Verify EACH staged binary against its expected bundle SHA BEFORE installing anything (DRY: same gate
# for producer + dashboard).
verify_staged() { # <staged-file> <expected-sha>
  local got; got="\$(sha256sum "\$1" | awk '{print \$1}')"
  [ "\$got" = "\$2" ] || { echo "staged binary SHA mismatch for \$1 (\$got != \$2)"; exit 1; }
}
[ "\$DASHBOARD_ONLY" = 1 ] || verify_staged "\$STAGING/jurisearch-producer"  "\$EXPECT_SHA"
verify_staged "\$STAGING/jurisearch-dashboard" "\$EXPECT_DASH_SHA"

# Atomic, GUARDED binary swap (DRY: producer + dashboard). Stage the new binary NEXT TO its dest (same
# dir = same filesystem, so the final mv is a rename, not a copy), set root:root 0755, re-verify its SHA
# against the expected bundle sum, then atomically rename into place. A torn/partial target is impossible:
# the dest is only ever the old file or the fully-written new one (\$\$ = this remote shell's PID, so the
# temp name can't collide). Callers MUST have stopped + verified-inactive any unit using the dest first.
swap_binary() { # <staged-file> <dest> <expected-sha>
  local staged="\$1" dest="\$2" esha="\$3" new sha
  new="\$dest.new.\$\$"
  install -o root -g root -m 0755 "\$staged" "\$new"
  sha="\$(sha256sum "\$new" | awk '{print \$1}')"
  if [ "\$sha" != "\$esha" ]; then rm -f "\$new"; echo "staged binary SHA mismatch at swap for \$dest (\$sha != \$esha)"; exit 1; fi
  mv -f "\$new" "\$dest"
}

# Service user (system, no login, home = state dir). Idempotent.
if ! id "\$R_SVC_USER" >/dev/null 2>&1; then
  useradd --system --shell /usr/sbin/nologin --home-dir "\$R_STATE" --create-home "\$R_SVC_USER"
  say "created system user \$R_SVC_USER"
else
  say "system user \$R_SVC_USER present"
fi

# systemd-journal group: the dashboard unit's SupplementaryGroups=systemd-journal (Spike C) requires the
# group to EXIST or the unit fails to start. It exists on CT 111; create it (system group) if somehow
# absent. NOTE: we deliberately do NOT \`usermod -aG\` jurisearch into it — the unit-level grant is scoped
# to the dashboard process only (proven sufficient by Spike C's \`runuser -g systemd-journal\`), so the
# producer's identity is left untouched (narrower blast radius than a global membership).
if ! getent group systemd-journal >/dev/null 2>&1; then
  groupadd --system systemd-journal && say "created group systemd-journal"
else
  say "group systemd-journal present"
fi

# Directories + ownership. \$R_ETC always (holds the dashboard config); the producer state/secrets/storebox
# dirs only in a full deploy (--dashboard-only must not re-chmod/chown the producer's existing dirs). The
# service (User=jurisearch) writes packages/archives/state; ProtectSystem=strict makes everything else
# read-only at runtime, so ownership here is what matters.
install -d -o root -g root -m 0755 "\$R_ETC"
if [ "\$DASHBOARD_ONLY" != 1 ]; then
  install -d -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0700 "\$R_SECRETS"
  install -d -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0750 "\$R_STATE"
  for d in "\$R_STOREBOX" "\$R_STOREBOX/packages" "\$R_STOREBOX/archives" "\$R_STOREBOX/manifests" "\$R_STOREBOX/tmp"; do
    install -d -o "\$R_SVC_USER" -g "\$R_SVC_USER" -m 0750 "\$d"
  done
fi

# Stop the dashboard service before swapping its binary (it holds \$R_DASH_BIN open). In a FULL deploy we
# also stop the producer timers + services before the producer swap; --dashboard-only NEVER stops the
# producer timers/services (they stay exactly as the operator left them — e.g. intentionally disabled).
if [ "\$DASHBOARD_ONLY" != 1 ]; then
  for t in \$TIMERS; do systemctl stop "\$t" 2>/dev/null || true; done
  systemctl stop 'jurisearch-producer-*.service' 2>/dev/null || true
fi
systemctl stop "\$R_DASH_SVC" 2>/dev/null || true

# Guard: refuse to swap an executable out from under a STILL-RUNNING unit (a manual \`update\`, or a unit
# that did not stop above). Replacing a live binary's file is exactly the unsafe window we avoid. In
# --dashboard-only we only guard (and swap) the dashboard; the producer binary is not touched.
if [ "\$DASHBOARD_ONLY" = 1 ]; then GUARD_SVCS="\$R_DASH_SVC"
else GUARD_SVCS="jurisearch-producer-legislation.service jurisearch-producer-jurisprudence.service \$R_DASH_SVC"; fi
for s in \$GUARD_SVCS; do
  if systemctl is-active --quiet "\$s"; then echo "refusing binary swap: \$s is still active"; exit 1; fi
done

# Swap the dashboard binary always; the producer binary only in a full deploy. Each dest is verified
# inactive by the guard just above.
if [ "\$DASHBOARD_ONLY" != 1 ]; then
  swap_binary "\$STAGING/jurisearch-producer" "\$R_BIN" "\$EXPECT_SHA"
  INSTALLED_VERSION="\$("\$R_BIN" --version 2>/dev/null || echo FAILED)"
  say "installed binary: \$INSTALLED_VERSION"
fi
swap_binary "\$STAGING/jurisearch-dashboard" "\$R_DASH_BIN" "\$EXPECT_DASH_SHA"
INSTALLED_DASH_VERSION="\$("\$R_DASH_BIN" --version 2>/dev/null || echo FAILED)"
say "installed binary: \$INSTALLED_DASH_VERSION"

# ── Producer-only install steps (secrets, env, producer.toml, validate, provision, units, timers) ──
# ALL skipped in --dashboard-only: the producer install is left exactly as-is.
if [ "\$DASHBOARD_ONLY" != 1 ]; then
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

# producer.env (systemd EnvironmentFile): MERGE-UPSERT, never wholesale-blank. The staged file carries
# ONLY the managed creds provided THIS run (non-empty). On a fresh box install it as-is; on an existing
# box, upsert each provided key into the live file (replace in place if present, else append) and leave
# every omitted managed key AND every operator-added extra line untouched — so a partial re-export can
# never blank a cred it did not re-provide. Values are opaque literals (UUIDs/keys): the merge reads each
# value VERBATIM from the staged file's content (substr) and re-emits it byte-for-byte — a value NEVER
# passes through awk -v, sed replacement text, or any other escape-interpreting channel (so literal
# backslash sequences like \\n/\\t survive intact). Only the safe managed KEY NAMES travel via -v, and
# keys match by literal prefix (no regex metachar pitfalls).
MANAGED_ENV_KEYS="OPENROUTER_API_KEY PISTE_API_KEY PISTE_OAUTH_CLIENT_ID PISTE_OAUTH_CLIENT_SECRET"
if [ ! -f "\$R_ENV" ]; then
  install -o root -g root -m 0600 "\$STAGING/producer.env" "\$R_ENV"; say "wrote \$R_ENV"
else
  merged="\$(mktemp)"
  awk -v keys="\$MANAGED_ENV_KEYS" '
    BEGIN { n = split(keys, ka, " ") }
    function mkey(line,   i, k) { for (i = 1; i <= n; i++) { k = ka[i]; if (index(line, k "=") == 1) return k } return "" }
    NR == FNR {                              # staged file: capture non-empty managed values verbatim
      k = mkey(\$0)
      if (k != "") { v = substr(\$0, length(k) + 2); if (v != "") { val[k] = v; have[k] = 1 } }
      next
    }
    {                                        # existing base file: replace managed lines we have, else keep
      k = mkey(\$0)
      if (k != "" && have[k]) { if (!printed[k]) { print k "=" val[k]; printed[k] = 1 } }
      else { print }
    }
    END { for (i = 1; i <= n; i++) { k = ka[i]; if (have[k] && !printed[k]) print k "=" val[k] } }
  ' "\$STAGING/producer.env" "\$R_ENV" > "\$merged"
  if ! cmp -s "\$merged" "\$R_ENV"; then install -o root -g root -m 0600 "\$merged" "\$R_ENV"; say "merged creds into \$R_ENV"; else say "kept existing \$R_ENV"; fi
  rm -f "\$merged"
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
fi  # end producer-only secrets/env/producer.toml block

# dashboard.toml: install-if-absent unless --force-config (preserve operator edits on upgrade), mirroring
# producer.toml. No secrets (read-only Phase 1). Root-owned 0644 — the dashboard only READS it.
if [ ! -f "\$R_DASH_CONF" ] || [ "\$FORCE_CONFIG" = "1" ]; then
  install -o root -g root -m 0644 "\$STAGING/dashboard.toml" "\$R_DASH_CONF"; say "wrote \$R_DASH_CONF"
else
  say "kept existing \$R_DASH_CONF"
fi

if [ "\$DASHBOARD_ONLY" != 1 ]; then
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
fi  # end producer-only validate/provision/units block

# Install the dashboard unit (authoritative: deploy.sh OWNS it, so overwrite every run — like the
# producer units the line above re-renders). It was rendered locally with the Spike C requirements
# (SupplementaryGroups=systemd-journal, Restart=always, explicit tailnet bind) and staged 0600; install
# root:root 0644 as a normal unit file.
install -o root -g root -m 0644 "\$STAGING/\$R_DASH_SVC" "\$R_DASH_UNIT"
say "wrote \$R_DASH_UNIT"
systemctl daemon-reload

# Producer TIMER ARMING — SKIPPED ENTIRELY in --dashboard-only (the whole reason this mode exists: never
# re-enable/start/stop the producer timers, so a disabled producer stays disabled and no heavy update
# fires). DEPLOY-SAFE ARM (full deploy only): the producer timers are Persistent=true, so a plain
# \`enable --now\` can immediately fire a missed-window CATCH-UP — running the heavy \`update\` (~1 GB DILA
# pull) mid-deploy if today's OnCalendar window has already passed. Seed each timer's persistent stamp to
# NOW first: systemd reads the stamp file's mtime as the LastTrigger, so a current stamp means "already
# triggered this window" and no catch-up fires. The unit names already carry the .timer suffix, so the
# stamp file is, e.g., stamp-jurisearch-producer-legislation.timer. THEN enable + start (next window only).
if [ "\$DASHBOARD_ONLY" != 1 ]; then
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
else
  say "--dashboard-only: producer timers left untouched (not enabled/started/stopped): \$TIMERS"
fi

# Dashboard: a long-running read-only SERVICE — UNLIKE the timers, we DO want it active now. enable --now
# starts it bound to the explicit tailnet addr. If it fails to bind (e.g. a wildcard the binary's guard
# rejects, or the tailnet addr not yet up) it stays inactive — caught by the Phase 6 fail-closed gate.
systemctl enable --now "\$R_DASH_SVC"
say "enabled + started \$R_DASH_SVC"
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
echo "INSTALLED_DASH_VERSION=\$("$R_DASH_BIN" --version 2>/dev/null || echo FAILED)"
echo "INSTALLED_DASH_SHA=\$(sha256sum "$R_DASH_BIN" | awk '{print \$1}')"

# Producer-side verification — only in a FULL deploy. In --dashboard-only the producer install was NOT
# touched, so we DON'T re-assert its identity/secrets/timers (and we NEVER require the timers enabled —
# they are intentionally disabled in this mode); we just report their state informationally (never fail=1).
if [ "$DASHBOARD_ONLY" != 1 ]; then
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
else
echo "--- producer timers (INFORMATIONAL — --dashboard-only left them untouched; NOT required) ---"
for t in ${TIMERS[*]}; do
  printf '%s enabled=%s active=%s\n' "\$t" "\$(systemctl is-enabled "\$t" 2>/dev/null)" "\$(systemctl is-active "\$t" 2>/dev/null)"
done
fi

# ── Dashboard fail-closed gate (M6b safety gate — RUNS during the real M7 deploy) ──────────────────
# Hard invariants, each turning the deploy RED via \$fail:
#   (1) the service is ACTIVE (enable --now brought it up and it stayed up);
#   (2) the configured bind is genuinely THIS host's tailnet addr (cross-checked vs \`tailscale ip\`);
#   (3) it is listening EXACTLY on \$exp_addr:\$exp_port and on NOTHING else on that port — no wildcard,
#       no second non-tailnet listener (IPv6 compared bracket-normalized);
#   (4) journalctl works UNDER THE DASHBOARD IDENTITY (systemd-journal group), AND the running MainPID
#       actually carries the systemd-journal gid (unit property + /proc/<pid>/status) — Spike C.
echo "--- dashboard service (must be active) ---"
dac="\$(systemctl is-active "$R_DASH_SVC" 2>/dev/null)"
den="\$(systemctl is-enabled "$R_DASH_SVC" 2>/dev/null)"
printf '%s enabled=%s active=%s\n' "$R_DASH_SVC" "\$den" "\$dac"
[ "\$dac" = "active" ]  || { echo "FAIL: $R_DASH_SVC not active (\$dac)"; fail=1; }
[ "\$den" = "enabled" ] || { echo "FAIL: $R_DASH_SVC not enabled (\$den)"; fail=1; }

# The expected address/port, address lowercased (IPv6 hex is case-insensitive; ss reports lowercase).
exp_addr="\$(printf '%s' "${DASHBOARD_BIND}" | tr '[:upper:]' '[:lower:]')"; exp_port="${DASHBOARD_PORT}"

# Belt-and-suspenders (BLOCKER #1): prove the expected bind is genuinely THIS host's tailnet addr — not
# just a 100.64/10-shaped literal — by cross-checking it against \`tailscale ip\`. Fail closed if tailscale
# can't be read (the box must be on the tailnet to serve the dashboard there).
echo "--- dashboard bind must be one of THIS host's tailscale ip addrs ---"
ts_ips="\$( { tailscale ip -4 2>/dev/null; tailscale ip -6 2>/dev/null; } | tr '[:upper:]' '[:lower:]')"
if [ -z "\$ts_ips" ]; then
  echo "FAIL: cannot read 'tailscale ip' (tailscaled down?); cannot prove \$exp_addr is this host's tailnet addr"; fail=1
elif printf '%s\n' "\$ts_ips" | grep -qxF "\$exp_addr"; then
  echo "ok: \$exp_addr is a tailscale address of this host"
else
  echo "FAIL: dashboard bind \$exp_addr is NOT among this host's tailscale ip addresses (\$(printf '%s' "\$ts_ips" | tr '\n' ' '))"; fail=1
fi

echo "--- dashboard listener (must be EXACTLY \$exp_addr:\$exp_port, and NOTHING else, on that port) ---"
# Parse "Local Address:Port" → "addr port" with IPv6 unbracketed + lowercased, so an \`ss\`-bracketed
# IPv6 tailnet addr (\`[fd7a:...]:PORT\`) compares equal to the unbracketed expected addr (WARN #3).
norm_hostport() { # <ss-local-addr:port> → "<addr> <port>"
  local hp="\$1" addr port
  if [ "\${hp#[}" != "\$hp" ]; then addr="\${hp#[}"; addr="\${addr%%]*}"; port="\${hp##*]:}"   # [v6]:port
  else addr="\${hp%:*}"; port="\${hp##*:}"; fi                                                  # v4:port / *:port
  printf '%s %s' "\$(printf '%s' "\$addr" | tr '[:upper:]' '[:lower:]')" "\$port"
}
# Enforce ONLY (BLOCKER #2): EVERY listener on the port must equal the expected addr:port; a second
# non-wildcard listener (e.g. a LAN IP on the same port) must turn the deploy RED, not be ignored.
mapfile -t LISTENERS < <(ss -ltnH "sport = :\$exp_port" 2>/dev/null | awk '{print \$4}')
printf '    listening: %s\n' "\${LISTENERS[@]:-<none>}"
found_expected=0
for l in "\${LISTENERS[@]}"; do
  [ -n "\$l" ] || continue
  read -r la lp < <(norm_hostport "\$l")
  case "\$la" in
    0.0.0.0|"::"|"*"|"") echo "FAIL: $R_DASH_SVC bound a WILDCARD \$l (no-auth exposure)"; fail=1; continue;;
  esac
  if [ "\$la" = "\$exp_addr" ] && [ "\$lp" = "\$exp_port" ]; then found_expected=1
  else echo "FAIL: unexpected listener \$l on port \$exp_port (only \$exp_addr:\$exp_port allowed)"; fail=1; fi
done
if [ "\$found_expected" = 1 ]; then echo "ok: listening only on \$exp_addr:\$exp_port"
else echo "FAIL: $R_DASH_SVC not listening on the expected tailnet addr \$exp_addr:\$exp_port"; fail=1; fi

echo "--- journald access under the dashboard identity (User=$R_SVC_USER + systemd-journal) ---"
# Spike C probe 2 replicated: the supplementary group must grant journal reads for the log tail. \`-q\`
# would hide errors; we want the JSON line on success and a non-zero exit on permission failure.
if runuser -u "$R_SVC_USER" -g systemd-journal -- journalctl -u jurisearch-producer-legislation.service -n 1 -o json >/dev/null 2>&1; then
  echo "ok: journalctl readable as $R_SVC_USER:systemd-journal"
else
  echo "FAIL: journalctl unreadable under the dashboard identity (systemd-journal group not effective)"; fail=1
fi

# WARN #5: the probe above proves the GROUP can read journald, not that systemd actually applied
# SupplementaryGroups to the RUNNING dashboard process. Assert it on the live MainPID two ways: the
# unit property AND the kernel's view of the process's supplementary gids in /proc/<pid>/status.
echo "--- running dashboard process actually has the systemd-journal group ---"
jgid="\$(getent group systemd-journal | awk -F: '{print \$3}')"
supp="\$(systemctl show "$R_DASH_SVC" -p SupplementaryGroups --value 2>/dev/null)"
mainpid="\$(systemctl show "$R_DASH_SVC" -p MainPID --value 2>/dev/null)"
echo "MainPID=\$mainpid SupplementaryGroups='\$supp' systemd-journal gid=\$jgid"
case " \$supp " in *" systemd-journal "*) echo "ok: unit declares SupplementaryGroups=systemd-journal";;
  *) echo "FAIL: unit SupplementaryGroups does not list systemd-journal ('\$supp')"; fail=1;; esac
if [ -n "\$jgid" ] && [ -n "\$mainpid" ] && [ "\$mainpid" != 0 ] && [ -r "/proc/\$mainpid/status" ]; then
  pgroups="\$(awk '/^Groups:/{\$1=""; print}' "/proc/\$mainpid/status")"
  case " \$pgroups " in *" \$jgid "*) echo "ok: pid \$mainpid carries gid \$jgid (systemd-journal)";;
    *) echo "FAIL: pid \$mainpid Groups (\$pgroups) lacks systemd-journal gid \$jgid"; fail=1;; esac
else
  echo "FAIL: cannot read /proc/\$mainpid/status for the dashboard MainPID (gid=\$jgid)"; fail=1
fi

exit \$fail
REMOTE_VERIFY
# shellcheck disable=SC2001  # prefix every captured line with 4 spaces — a line-anchor sub no ${//} can do
echo "$VERIFY" | sed 's/^/    /'
[ "${vrc:-1}" = "0" ] || die "remote verification failed (rc=$vrc); see output above"

# Cross-check the installed binary identity against the bundle we shipped. The producer cross-check runs
# only in a FULL deploy (--dashboard-only never touched the producer binary, so there is nothing to match).
if [ "$DASHBOARD_ONLY" != 1 ]; then
  INSTALLED_VERSION="$(echo "$VERIFY" | sed -n 's/^INSTALLED_VERSION=//p' | head -1)"
  INSTALLED_SHA="$(echo "$VERIFY" | sed -n 's/^INSTALLED_SHA=//p' | head -1)"
  [ "$INSTALLED_VERSION" = "$EXPECT_VERSION" ] || die "installed --version ('$INSTALLED_VERSION') != bundle ('$EXPECT_VERSION')"
  [ "$INSTALLED_SHA" = "$EXPECT_SHA" ] || die "installed SHA ('$INSTALLED_SHA') != bundle ('$EXPECT_SHA')"
  log "verified: installed producer matches the bundle ($INSTALLED_VERSION)"
fi

INSTALLED_DASH_VERSION="$(echo "$VERIFY" | sed -n 's/^INSTALLED_DASH_VERSION=//p' | head -1)"
INSTALLED_DASH_SHA="$(echo "$VERIFY" | sed -n 's/^INSTALLED_DASH_SHA=//p' | head -1)"
[ "$INSTALLED_DASH_VERSION" = "$EXPECT_DASH_VERSION" ] || die "installed dashboard --version ('$INSTALLED_DASH_VERSION') != bundle ('$EXPECT_DASH_VERSION')"
[ "$INSTALLED_DASH_SHA" = "$EXPECT_DASH_SHA" ] || die "installed dashboard SHA ('$INSTALLED_DASH_SHA') != bundle ('$EXPECT_DASH_SHA')"
log "verified: installed dashboard matches the bundle ($INSTALLED_DASH_VERSION)"

# Optional: prove DILA egress without downloading anything (no DB/embedding involved). Producer-only —
# never run against the producer in --dashboard-only (that mode must not touch it).
if [ "$DO_SMOKE" = "1" ] && [ "$DASHBOARD_ONLY" != 1 ]; then
  log "smoke — fetch --source legi --dry-run (proves DILA reachability; no download)"
  rsh "'$R_BIN' fetch --source legi --dry-run --config '$R_CONF' 2>&1 | sed 's/^/    /'" || \
    info "WARNING: smoke fetch --dry-run returned non-zero (DILA listing/egress?)."
fi

if [ "$DASHBOARD_ONLY" = 1 ]; then
  log "DONE — dashboard-only deploy on $DEPLOY_HOST (producer + its timers left untouched)."
  info "Dashboard (read-only, tailnet-only): http://$DASHBOARD_BIND:$DASHBOARD_PORT"
else
  log "DONE — update-server ${MODE}ed and armed on $DEPLOY_HOST."
  info "Dashboard (read-only, tailnet-only): http://$DASHBOARD_BIND:$DASHBOARD_PORT"
  info "First live publish (operator step; needs OPENROUTER_API_KEY in $R_ENV; PISTE_* creds enable Judilibre/Legifrance enrichment):"
  info "  ssh $DEPLOY_SSH_USER@$DEPLOY_HOST '$R_BIN update --config $R_CONF --group legislation'"
fi
