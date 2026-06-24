#!/usr/bin/env bash
# Launch a command in a NEW visible WezTerm GUI window, tee'd to a timestamped log under ops/logs/ so
# BOTH the user (the window) and Claude (the log file) can watch a long enrichment run live.
# Usage: run-in-wezterm.sh <label> <command...>
set -euo pipefail
LABEL="${1:?usage: run-in-wezterm.sh <label> <command...>}"; shift
OPS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOG_DIR="$OPS_DIR/logs"
mkdir -p "$LOG_DIR"
# Timestamp is injected by the caller's env (scripts can't call date() under some sandboxes); fall back.
STAMP="${RUN_STAMP:-$(date -u +%Y%m%d-%H%M%S 2>/dev/null || echo run)}"
LOG="$LOG_DIR/${LABEL}-${STAMP}.log"
CMD="$*"
echo "Launching WezTerm window '$LABEL' -> $LOG"
# The pane runs the command, tees to the log, and stays open afterwards so the user can read the result.
wezterm start --always-new-process -- bash -lc \
  "echo '=== $LABEL : $CMD ==='; { $CMD; } 2>&1 | tee '$LOG'; echo '=== finished (exit \${PIPESTATUS[0]}) ==='; exec bash" \
  >/dev/null 2>&1 &
disown || true
echo "$LOG"
