# Zone rollout ops scripts

Operational scripts for the Option B zone-precise retrieval data rollout, run on the throwaway CLONE
`/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624` (never prod until promotion).

Override `BIN` / `CLONE` env vars to retarget. Logs land under `ops/logs/`.

Sequence:
1. `00-wipe-clone-zone-data.sh` — TRUNCATE the clone's decision_zones/zone_units/zone_unit_embeddings/
   official_api_responses so enrichment re-runs from scratch (with the v16 archive wired in).
2. `01-recent-enrich-loop.sh` — recent-first `enrich-zones` (cass then inca), archiving EVERY official-API
   exchange into `official_api_responses`; yield-collapse stop rule; resumable.
3. (slice 2) `collect-legislation-citations` + `enrich-legislation-citations` — extract visa citations from
   the archived /decision responses, dedup, resolve via Legifrance, persist (v17).
4. `02-build-embed-eval.sh` — derive zone_units, embed via OpenRouter, verify dense index, measured eval.

Run any long step in a visible WezTerm window via:
  `RUN_STAMP=<ts> ./run-in-wezterm.sh enrich ./01-recent-enrich-loop.sh`
which tees output to `ops/logs/<label>-<ts>.log` (watchable by both the user and Claude).
