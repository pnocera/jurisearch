#!/usr/bin/env bash
# Wipe the throwaway clone's zone + API-archive scratch so the enrichment can re-run FROM SCRATCH with
# the v16 archive wired in. Operates DIRECTLY on the clone's Postgres via pg_ctl/psql (the jurisearch CLI
# has no wipe command). ONLY ever run this against the zone-rollout CLONE, never prod.
set -euo pipefail
CLONE=${CLONE:-/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624}
PGCTL=/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_ctl
PSQL=/home/pierre/.pgrx/18.4/pgrx-install/bin/psql

case "$CLONE" in
  *zone-rollout*) : ;;
  *) echo "REFUSING: CLONE path does not look like a zone-rollout clone: $CLONE" >&2; exit 1 ;;
esac

started=0
if ! ls "$CLONE/pg/data/postmaster.pid" >/dev/null 2>&1; then
  "$PGCTL" -D "$CLONE/pg/data" -w start -o "-k $CLONE/pg/sock" >/tmp/wipe-clone-pg.log 2>&1
  started=1
  sleep 1
fi
PORT=$(sed -n '4p' "$CLONE/pg/data/postmaster.pid")
echo "Wiping zone + archive scratch on $CLONE (port $PORT) ..."
"$PSQL" -h "$CLONE/pg/sock" -p "$PORT" -d jurisearch -c \
  "TRUNCATE zone_unit_embeddings, zone_units, decision_zones, official_api_responses RESTART IDENTITY CASCADE;"
echo "Counts after wipe:"
"$PSQL" -h "$CLONE/pg/sock" -p "$PORT" -d jurisearch -At -c \
  "SELECT 'decision_zones='||count(*) FROM decision_zones UNION ALL
   SELECT 'zone_units='||count(*) FROM zone_units UNION ALL
   SELECT 'official_api_responses='||count(*) FROM official_api_responses;"
if [ "$started" = "1" ]; then
  "$PGCTL" -D "$CLONE/pg/data" -m fast stop >/dev/null 2>&1
  echo "(stopped the PG this script started)"
fi
echo "Wipe done."
