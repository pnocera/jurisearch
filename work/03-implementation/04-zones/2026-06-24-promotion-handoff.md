# HANDOFF — zone-precise retrieval promotion + Legifrance recall tuning (2026-06-24)

Read this top-to-bottom to resume. Context was cleared after writing it; nothing below is in working
memory anymore.

## 0. TL;DR — what to do next (decided with the user)

We are mid-**promotion** of the Option-B zone-precise retrieval subsystem onto a clone of production,
and adding a durable official-API archive + a Legifrance legislation-citation enrichment. All CODE is
implemented and codex-GO'd EXCEPT the Legifrance `/search` work, which has open fixes.

**The user chose "option 2": tune the Legifrance query for higher recall FIRST, then finish the
promotion.** Concretely, the next session should, in order:

1. **Fix + tune the Legifrance enrichment** (one codex-reviewed change):
   - (recall) Lift the ~35% resolution rate — current query is `TOUS_LES_MOTS_DANS_UN_CHAMP` over
     `typeChamp=ALL` with the whole `"<article> <code>"` string; engineer a more precise query, e.g.
     separate champs (article number in an article/number field + code name in the title field), or a
     better `typeRecherche`/`fond`. Validate live against the API (see §6 for the working body + token).
   - (codex WARN #1) `legifrance_search_exchange` computes `request_fingerprint` from a now-absent
     top-level `body["query"]`, so every row archives the empty fingerprint `legifrance-search:`. Make it
     read `recherche.champs[*].criteres[*].valeur` (or hash the body). + regression test.
     (`crates/jurisearch-official-api/src/lib.rs:~397`.)
   - (codex WARN #2) `cite --online` (`apply_online_citation_confirmation`, `main.rs:~11683`) STILL sends
     the bad `{query,pageSize}` body → would 500. Factor a shared body builder so both the enrichment and
     `cite --online` use the real contract. + test.
   - Review file to address: `work/03-implementation/04-zones/reviews/2026-06-24-legifrance-bodyfix-codex-review-r1.md`
     (VERDICT: FIXES_REQUIRED — these 2 WARNs).
   - Get a codex review (use the `codex-review` skill), apply, GO, **rebuild the release binary**.
2. **Run the full Legifrance pass** on the clone (resumable): `ops/03-legislation-enrich-loop.sh` in a
   WezTerm window (see §5). Re-run with `--retry-errors` so the ~1,300 rows attempted under the old broken
   body (now `upstream_error`) get redone. ~17,220 unique citations, ~0.3–0.9s each ⇒ ~1.5–2.5h.
3. **Build + embed + eval the zone retrieval** (`ops/02-build-embed-eval.sh`, WezTerm): build-zone-units →
   OpenRouter embed → `eval france-juris-zones --floor 0.8`. (CANNOT overlap step 2 — both need the clone
   Postgres; do them sequentially.)
4. **Go/no-go + promote**: coverage already clears the bar (72,911 zoned decisions ≫ 25k / 5%); require
   `eval france-juris-zones` `all_meet_proposed_floor=true`; then **directory-swap** the clone into prod,
   **preserving the cache** (see memory `preserve-decision-zones-cache-on-promotion`).

There is also a SEPARATE, DONE-BUT-NOT-IMPLEMENTED deliverable: the authority-aware ranking
**analysis + design** (codex-GO'd) under `work/03-implementation/05-ranking/`. It is design-only; do NOT
implement it as part of this promotion. (§8)

## 1. What this effort is

Build order in `work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md`
(Z1–Z5). Z5 (status block + measured-only zone eval) is done. Then the user added two requirements:
**keep ALL official-API call results** (a durable archive) and **also enrich via Legifrance** (persist
legislation cited in decisions' `visa`). Those became "slice 1" (v16 archive) and "slice 2" (v17
Legifrance), both implemented + codex-GO'd. The Legifrance API call itself was then found broken (wrong
body) and is the remaining work (above).

## 2. Code state — commits (all on `main`, all codex-reviewed unless noted)

```
7bb75bd Legifrance: fix /search request body (HTTP 500 -> 200)   <-- review FIXES_REQUIRED (2 WARNs, §0.1)
188c5ee Ranking: authority-aware ranking design (codex GO, r4)   <-- design only, deferred (§8)
d4e0d9a Ranking: authority-aware ranking analysis (codex GO, r2)
e81f236 Zone rollout slice 2 r2 review (codex GO)
8f4be82 Zone rollout slice 2 r1 fix (codex): archive missing-credential Legifrance attempts
9ca529b Zone rollout slice 2: Legifrance legislation enrichment (v17)
ad79bea Zone rollout: ops scripts (wezterm-logged) + slice-1 archive review (codex GO)
d7040d1 Zone rollout slice 1: durable official-API response archive (v16)
ce4704d Zone rollout: enrich --order review (codex GO)
c3a78cf Zone rollout: add enrich-zones --order recent|oldest
c01f322 Zone retrieval Z5 r2 review (codex GO)
9b66d30 Zone retrieval Z5 r1 fixes (codex)
b87ac8c Zone retrieval Z5: status.zone_retrieval + zone eval benchmark
```
Schema head is **v17** (`crates/jurisearch-storage/src/migrations.rs`): v16 = `official_api_responses`
(durable archive); v17 = `decision_legislation_citations` + `legislation_citation_resolutions`.
Tests pass (`cargo test -p jurisearch-storage -p jurisearch-cli`); the only open code items are the 2
Legifrance WARNs in §0.1.

## 3. Data state — the working clone (NEVER touch prod until promotion)

- **Prod (untouched):** `/mnt/models/jurisearch-index/phase2-full-juridic` (157G). Its Postgres was a
  leftover idle instance; it was cleanly stopped early on and left stopped (no consumer).
- **Immutable backup:** `/mnt/models/jurisearch-index/phase2-full-juridic.backup-20260624` (157G).
- **Working clone (all rollout work happens here):**
  `/mnt/models/jurisearch-index/phase2-full-juridic.zone-rollout-20260624`. Snapshot of state (queried
  just before this handoff, clone PG then stopped):
  - schema **17**
  - `decision_zones` status=ok: **72,911** (the zoned Cassation decisions: cass 11,606 + inca 61,305)
  - `official_api_responses`: **206,894** total; judilibre ok: **203,857** (full /search + /decision raw
    archived for every fetched decision, incl. the ~32k that turned out to have no zones)
  - `decision_legislation_citations` (occurrences): **63,738**
  - `legislation_citation_resolutions` (unique): **17,220** — MOSTLY `pending`; ~1,300 are `upstream_error`
    from the broken-body run (will be redone via `--retry-errors`); a handful resolved during smokes.
  - `zone_units` = **0**, `zone_unit_embeddings` = **0** — build/embed NOT run yet (step 3).
- Coverage was harvested **recent-first** (`enrich-zones --order recent`) with a yield-collapse stop:
  Judilibre only zone-annotates RECENT decisions, so old decisions have no zones (correct, expected).

## 4. Remaining promotion steps (detail)

### Step 1 — Legifrance fix + recall tuning (option 2) — see §0.1.
### Step 2 — full Legifrance pass: `ops/03-legislation-enrich-loop.sh` (WezTerm, §5), `--retry-errors`.
### Step 3 — build/embed/eval: `ops/02-build-embed-eval.sh` (WezTerm). Uses the OpenRouter pool (§6) for
   embedding (NOT the local server — user instruction); fingerprint stays `bge-m3:1024:normalize:true`.
   `eval france-juris-zones --mode hybrid --floor 0.8` writes
   `work/03-implementation/02-evidence/2026-06-24-phase2-zone-benchmark-clone.json`.
### Step 4 — go/no-go + promote:
   - go/no-go (codex's thresholds): ≥25k ok decisions OR ≥5% of 494,701 reachable (BOTH already met:
     72,911); `eval france-juris-zones` `all_meet_proposed_floor=true`; `status.zone_retrieval`
     embeddings complete (`units_pending=0`, total==zone_units.total, fingerprint matches).
   - **Promote by directory swap** (NOT rsync-over): stop any clone/prod PG, `mv prod prod.pre-zone-<ts>`,
     `mv clone prod`; verify `status` + a `search --zone`. KEEP the cache (decision_zones +
     official_api_responses) — the swap preserves it by construction (clone is a superset). See memory
     `preserve-decision-zones-cache-on-promotion`. Update memory `phase2-jurisprudence-progress`.

## 5. Ops scripts + WezTerm logging (the user wants live observability)

`work/03-implementation/04-zones/ops/` (committed):
- `run-in-wezterm.sh <label> <cmd…>` — launches `<cmd>` in a NEW visible WezTerm GUI window, tee'd to
  `ops/logs/<label>-<RUN_STAMP>.log`. Pass `RUN_STAMP=$(date -u +%Y%m%d-%H%M%S)` and export `BIN`/`CLONE`.
  It `disown`s the window (no harness completion signal) — so ALSO launch a background `until grep … done`
  watcher on the log to get a completion notification, and read the log file for progress (the WezTerm
  window uses `--always-new-process`, so `wezterm cli list` on the default socket won't show it).
- `00-wipe-clone-zone-data.sh` — TRUNCATEs zone/archive scratch (only if a from-scratch redo is needed;
  NOT needed now — the archive + citations are populated).
- `01-recent-enrich-loop.sh` — recent-first zone enrich (DONE; here for re-runs).
- `02-build-embed-eval.sh` — step 3.
- `03-legislation-enrich-loop.sh` — step 2 (batched, resumable, `--retry-errors` supported via the
  underlying command; the loop script currently calls plain `--limit`; add `--retry-errors` for the redo).
- `README.md` — the sequence.

There are likely 2 idle leftover WezTerm windows (the finished enrich + the killed legislation loop) — they
are harmless (`exec bash` prompts); close them or ignore.

## 6. Key operational facts (verified)

- **Release binary:** `target/release/jurisearch`. **REBUILD after any code change**
  (`cargo build --release -p jurisearch-cli`) — long jobs spawn fresh processes that pick up the new binary.
- **Clone Postgres management:** pg_ctl at `/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_ctl`, psql at
  `.../bin/psql`. `jurisearch` commands open the clone via a MANAGED durable Postgres (advisory lock) — so
  **two `jurisearch` commands cannot run against the clone at once** (build/embed and Legifrance must be
  sequential). To inspect while no command runs: `pg_ctl -D "$CLONE/pg/data" -w start -o "-k $CLONE/pg/sock"`,
  read port from `$CLONE/pg/data/postmaster.pid` line 4, `psql -h "$CLONE/pg/sock" -p <port> -d jurisearch`,
  then `pg_ctl … -m fast stop`. A killed command can leave the managed PG running — stop it before re-running.
- **PISTE / Judilibre / Legifrance (env from ~/.zshrc; production by default):**
  - Judilibre = KeyId header (`PISTE_API_KEY`); Legifrance = OAuth2 (`PISTE_OAUTH_CLIENT_ID` /
    `PISTE_OAUTH_CLIENT_SECRET`). **OAuth2 works** (token endpoint
    `https://oauth.piste.gouv.fr/api/oauth/token`, grant `client_credentials`, scope `openid`).
  - **Legifrance `/search` WORKING body** (validated live; HTTP 200): POST
    `https://api.piste.gouv.fr/dila/legifrance/lf-engine-app/search` with
    `{"fond":"CODE_DATE","recherche":{"operateur":"ET","sort":"PERTINENCE","typePagination":"DEFAUT",
    "pageNumber":1,"pageSize":N,"champs":[{"typeChamp":"ALL","operateur":"ET","criteres":[
    {"typeRecherche":"TOUS_LES_MOTS_DANS_UN_CHAMP","valeur":"<query>","operateur":"ET"}]}]}}`.
    `UN_DES_MOTS` = ~everything + ~12s (bad). `TOUS_LES_MOTS` is INVALID (→null). `TOUS_LES_MOTS_DANS_UN_CHAMP`
    ~0.3–0.9s. Resolution of `"<article> <code>"` is ~35% — the recall to improve (step 1).
- **OpenRouter embedding (for step 3) — memory `embedding-via-openrouter`:**
  `JURISEARCH_EMBED_POOL="https://openrouter.ai/api/v1|baai/bge-m3|OPENROUTER_API_KEY"`,
  `--batch-size 32 --pool-concurrency 8`, auto-restart loop for 429s. Fingerprint stays
  `bge-m3:1024:normalize:true` (`baai/bge-m3` is only the request alias). `ops/02-…` already sets this.
- **Codex reviews:** use the `codex-review` skill (writes a review file, replies DONE). Give minimal scope
  (the commit/diff). All review artifacts live under `…/04-zones/reviews/` and `…/05-ranking/reviews/`.

## 7. Gotchas

- Build/embed and Legifrance can't run concurrently (clone PG lock) — sequence them.
- A `--limit`-bounded foreground command can exceed the 2-min Bash timeout — run long jobs in WezTerm or
  background.
- `embed-zone-units` finalize-gap (memory `embed-chunks-finalize-gap`): never leave the ANN index unbuilt
  after an aborted embed; `ops/02-…` wraps it in an auto-restart `until` loop.
- Promotion must PRESERVE `decision_zones` + `official_api_responses` (memory
  `preserve-decision-zones-cache-on-promotion`) — directory swap is safe; never rebuild/drop them.

## 8. Deferred (NOT part of this promotion): authority-aware ranking

`work/03-implementation/05-ranking/` — `…analysis.md` (codex GO r2) + `…design.md` (codex GO r4), both
committed. Design only (default-OFF post-SQL re-rank, per-order authority scales, first-page-only
pagination, `--authority-weight` no-env-fallback/decision-kind-gated/0.0-inert, measured-only pairwise
authority-lift metric, phased R1–R5 plan). Implement later as its own effort; it must not touch the
default ranking path. Do NOT conflate with the promotion.

## 9. Pointers

- Plan: `work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md`
- Reviews: `work/03-implementation/04-zones/reviews/` (incl. the open Legifrance body-fix r1) and
  `work/03-implementation/05-ranking/reviews/`
- Memories (auto-loaded): `autonomous-execution`, `review-before-execute`,
  `ask-codex-before-important-decisions`, `codex-review-no-explicit-instructions`,
  `phase2-jurisprudence-progress`, `embedding-via-openrouter`, `embed-chunks-finalize-gap`,
  `preserve-decision-zones-cache-on-promotion`.
- Working directives this session: don't stop except for true blockers; ask codex for decisions; codex-
  review + apply fixes at each step; use WezTerm with logging so the user can observe long runs.
