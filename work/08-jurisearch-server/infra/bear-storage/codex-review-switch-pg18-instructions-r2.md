# Codex re-review (r2) — switch-to-pg18.sh

## Scope
`/home/pierre/bear-storage/switch-to-pg18.sh` (in CT 110, Debian 13: replace PG17 with PG18 + pgvector
matching the source corpus). Ground truth unchanged from r1 instructions
(`codex-review-switch-pg18-instructions.md`); key fact you established in r1: PGDG trixie ships
postgresql-18-pgvector **0.8.3** (no 0.8.0), and the source corpus uses pgvector **0.8.0**.

This is **r2**. Your r1 review returned FIXES_REQUIRED with 1 BLOCKER + 1 WARN. The script was rewritten.

## Fixes applied — verify each
1. **BLOCKER (apt pgvector is 0.8.3, not 0.8.0).** The script no longer apt-installs pgvector. It now
   **builds pgvector from source at tag `v0.8.0`** against PG18:
   `git clone --depth 1 --branch v0.8.0 … ; make -C … PG_CONFIG=/usr/lib/postgresql/18/bin/pg_config ;
    make … install`, then verifies the installed `vector.control` `default_version` equals `0.8.0`
   (dies otherwise). Confirm this exactly matches the source corpus's pgvector 0.8.0 (so the physical
   copy's catalog version, control file, and shared library all agree at 0.8.0), and that the build
   deps (build-essential/make/gcc + postgresql-server-dev-18) are sufficient for pgvector (it has no
   other deps).
2. **WARN (PG17 dropped before confirming PG18 installable).** Reordered: PGDG repo setup + apt update
   + a non-destructive `apt-get install --simulate postgresql-18 postgresql-server-dev-18` preflight
   now run FIRST; only then is the empty PG17 cluster dropped, immediately before the real install.
   Confirm the preflight (`--simulate`) actually aborts on an unresolvable package set, and that
   dropping 17/main before installing PG18 is what lets 18/main take port 5432.

## Also confirm
- The PGDG repo line / signed-by key path are still correct (you approved these in r1).
- The pgvector control-version parse `sed -nE "s/^default_version = '([^']+)'.*/\1/p"` yields `0.8.0`
  for pgvector 0.8.0's vector.control.
- `set -Eeuo pipefail` + ERR trap + PG18-SWITCH-FAILED sentinel still make every failure fail closed,
  and no destructive step runs before the preflight passes.
- Build tools are checked up front (`for t in curl git make gcc`).

## Output
For the 2 findings: RESOLVED / PARTIALLY / NOT RESOLVED + one-line justification. List any new issues
(severity + concrete fix). End with exactly `VERDICT: GO` or `VERDICT: FIXES_REQUIRED`.
