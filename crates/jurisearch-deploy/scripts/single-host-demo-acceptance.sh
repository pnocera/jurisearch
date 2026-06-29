#!/usr/bin/env bash
# M5-B — operated SINGLE-HOST acceptance for the jurisearchctl demo/smoke/watchdog surface.
#
# Two tiers, so this is safe to run in CI yet complete enough to operate a real host:
#
#   TIER 1 (ALWAYS, no infra) — the smoke/watchdog/demo/fixture DECISION logic, unit-tested with synthetic
#   responses. This proves every acceptance gate (leg classification, no-silent-skip, the
#   stalled-cursor-vs-no-new-packages discrimination, the negative checks, and the
#   hybrid-skip-with-recorded-reason decision) WITHOUT a live DB, embedder, systemd, or network.
#
#   TIER 2 (AUTHORIZED-ONLY, skipped by default) — the live end-to-end legs that need real infra:
#   provisioning an external PostgreSQL, applying the signed FIXTURE corpus, starting the real
#   `jurisearch serve-site` + bge-m3, and running `jurisearchctl demo smoke` / `site smoke` /
#   `site watchdog` against it. These are SKIPPED with a recorded reason unless you opt in with
#   JURISEARCH_ACCEPTANCE_AUTHORIZED=1 AND pass --config <demo-site.toml>. The OPERATED-only legs that hit
#   a real DILA id / live PG / paid embeddings are never run by default and never fabricated.
#
# Usage:
#   crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh [--config <demo-site.toml>] \
#       [--fetch-id <id>]
#
#   JURISEARCH_ACCEPTANCE_AUTHORIZED=1   opt in to the live Tier-2 legs (requires --config)
set -uo pipefail

CONFIG=""
FETCH_ID=""   # defaults to the documented stable fixture id when empty
while [ $# -gt 0 ]; do
  case "$1" in
    --config) CONFIG="$2"; shift 2;;
    --fetch-id) FETCH_ID="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 64;;
  esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT" || { echo "cannot cd to repo root $ROOT" >&2; exit 1; }

CTL="cargo run --quiet -p jurisearch-deploy --bin jurisearchctl --"
RC=0

echo "=== TIER 1 — decision logic (always; no live infra) ==="
# The pinned acceptance gates live as unit tests over the pure classifiers. `cargo test` accepts only ONE
# positional filter, so each module is a SEPARATE invocation and any failure aggregates into TIER1_RC.
TIER1_RC=0
for module in ops::smoke:: ops::watchdog:: ops::demo:: ops::fixture::; do
  if ! cargo test -p jurisearch-deploy --quiet "$module"; then
    echo "[FAIL] decision-logic unit tests ($module)"; TIER1_RC=1
  fi
done
if [ "$TIER1_RC" -eq 0 ]; then
  echo "[PASS] smoke/watchdog/demo/fixture decision logic (leg classification, no-silent-skip,"
  echo "       stalled-vs-no-new-packages, negative checks, hybrid-skip-with-reason)"
else
  RC=1
fi

echo
echo "=== TIER 2 — live end-to-end (authorized-only) ==="
if [ "${JURISEARCH_ACCEPTANCE_AUTHORIZED:-0}" != "1" ]; then
  echo "[SKIP] live demo up/smoke/watchdog/down — reason: JURISEARCH_ACCEPTANCE_AUTHORIZED != 1"
  echo "       (these legs provision a real PostgreSQL, apply the signed fixture corpus, and start the"
  echo "        real serve-site + bge-m3; opt in with JURISEARCH_ACCEPTANCE_AUTHORIZED=1 --config <toml>)"
  echo
  echo "RESULT: Tier-1 RC=$RC; Tier-2 skipped-by-default (recorded reason above)."
  exit "$RC"
fi
if [ -z "$CONFIG" ]; then
  echo "[SKIP] live legs — reason: authorized but no --config <demo-site.toml> provided"
  echo "RESULT: Tier-1 RC=$RC; Tier-2 skipped (no config)."
  exit "$RC"
fi

# Authorized + config present: drive the REAL binaries. Each leg is required to succeed (no silent green).
#
# FIXTURE PREFLIGHT: the live fixture-backed demo (demo up/smoke) consumes the committed signed fixture
# artifact under crates/jurisearch-deploy/fixtures/published/. Generating those bytes is an authorized-only
# step that needs a populated producer DB (see the fixtures README), so the artifact may legitimately be
# absent. When it is, the fixture demo legs are SKIPPED WITH AN EXPLICIT RECORDED REASON (consistent with
# the no-silent-skip discipline) rather than fabricating a pass or hitting an obscure catch-up failure.
FIXTURE_MANIFEST="$ROOT/crates/jurisearch-deploy/fixtures/published/core/manifest.json"
if [ ! -f "$FIXTURE_MANIFEST" ]; then
  echo "[SKIP] demo up / demo smoke — reason: committed fixture artifact absent"
  echo "       (missing $FIXTURE_MANIFEST; the live fixture demo is UNAVAILABLE until the artifact is"
  echo "        generated via ops::fixture::generate_fixture against a populated producer DB — see"
  echo "        crates/jurisearch-deploy/fixtures/README.md — and committed)"
else
  echo "+ demo up (provision DB + trust + catch-up the fixture corpus + gated start)"
  $CTL demo up --config "$CONFIG" || { echo "[FAIL] demo up"; RC=1; }

  echo "+ demo url"
  URL="$($CTL demo url --config "$CONFIG")" && echo "  demo URL: $URL" || { echo "[FAIL] demo url"; RC=1; }

  echo "+ demo smoke (real status/fetch/search; hybrid skipped-with-reason if embedder assets absent)"
  $CTL demo smoke --config "$CONFIG" || { echo "[FAIL] demo smoke"; RC=1; }
fi

if [ -n "$FETCH_ID" ]; then
  echo "+ site smoke --fetch-id $FETCH_ID  (OPERATED: a real published/DILA id)"
  $CTL site smoke --config "$CONFIG" --fetch-id "$FETCH_ID" || { echo "[FAIL] site smoke"; RC=1; }
else
  echo "[SKIP] site smoke with a REAL DILA id — reason: no --fetch-id (the default fixture id is exercised"
  echo "       by demo smoke; pass --fetch-id <real-published-id> after the producer publishes real packages)"
fi

echo "+ site watchdog (READ-ONLY: stalled-cursor vs no-new-packages)"
$CTL site watchdog --config "$CONFIG" || { echo "[FAIL] site watchdog (signalled an alert exit; see the line above)"; RC=1; }

echo "+ demo down"
$CTL demo down --config "$CONFIG" || { echo "[FAIL] demo down"; RC=1; }

echo
echo "RESULT: Tier-1 + Tier-2 complete; RC=$RC"
exit "$RC"
