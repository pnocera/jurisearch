# Phase 1.3 — French Legal BM25 Analyzer — Claude Review

Verdict: GO

Reviewed: uncommitted working-tree changes on `main` (not yet committed) in `/home/pierre/Work/jurisearch`.
Date: 2026-06-21. Reviewer: Claude (Opus 4.8). Scope: read-only review; no code modified.

Plan context: `IMPLEMENTATION_PLAN.md §1.3 Search Pipeline Hardening`, task *"Tune French legal analyzer for elision, accents, statutory references, and legal stopwords/boosters."*

## What changed

| File | Change |
|---|---|
| `crates/jurisearch-storage/src/migrations.rs` | `CURRENT_SCHEMA_VERSION` 8 → 9; new migration `9: chunk_french_legal_bm25_analyzer` drops/rebuilds `chunks_bm25_idx` with a `text_fields` tokenizer (`type: default`, `ascii_folding: true`, `stemmer: French`, `stopwords_language: French`) and records manifest schema version 9. |
| `crates/jurisearch-storage/tests/retrieval_smoke.rs` | Fixture bodies re-accented (`responsabilité`/`réparations`); recipe decoy now carries `article 1241`; two new assertions: accent-insensitive + stemmed query (`responsabilite faute reparation`) and statutory query (`article 1240`). |
| `crates/jurisearch-storage/tests/schema_migrations.rs` | Asserts migration `9:chunk_french_legal_bm25_analyzer` present and `indexdef` now contains `contextualized_body` AND `ascii_folding` AND `French`. |
| `work/03-implementation/IMPLEMENTATION_PLAN.md` | 1.2 hardening item marked Done; 1.3 "Current status (2026-06-21)" block added. |

## Verdict rationale

The change is small, correct, transactional, version-gated, and — critically — **verified on the real `pg_search` backend** (both touched test files pass against `~/.pgrx/18.4` with `JURISEARCH_REQUIRE_PG_EXTENSIONS=1`). The tokenizer JSON uses keys that genuinely exist in the installed ParadeDB 0.24.1 fork, the migration applies idempotently, and the accent/stem/statutory behaviors it claims are demonstrated by passing discriminating assertions. No blocking defects. The findings below are tuning, test-strength, and plan-accuracy improvements that do not gate the merge.

## Blocking findings

None.

## Non-blocking suggestions

**S1 — "elision" and "boosters" are silently dropped from the 1.3 status (plan accuracy).** The 1.3 task line is *"...elision, accents, statutory references, and legal stopwords/boosters."* The new status block (`IMPLEMENTATION_PLAN.md:607-610`) claims accent folding + stemming + stopword removal as Done and lists the *Remaining* items, but **neither claims elision/boosters done nor lists them as remaining** — they simply vanish from the ledger. Reality (verified, see V3): elision is in fact handled *incidentally* — the `default` tokenizer (Tantivy `SimpleTokenizer`) splits on the apostrophe so `l'homme → l + homme`, and the French stopword list removes the elision particles `l, d, j, c, n, s, t, m, qu, à`. There is **no dedicated elision filter**, and the effect is untested (S2). Recommend: state explicitly in the status that elision is covered incidentally via tokenizer split + French stopwords (no dedicated filter), and move "boosters" (legal term/field boosting) to the *Remaining* list so the ledger stays complete.

**S2 — elision is completely untested despite being a named task item.** No fixture in the new or existing tests contains an apostrophe, so the `l'`/`d'`/`qu'` path that S1 relies on is unexercised. Add one chunk/query pair with elision (e.g. index `l'auteur du dommage`, query `auteur dommage`) to lock the behavior — otherwise a future tokenizer/stopword change could regress elision handling without any test failing.

**S3 — the statutory test passes partly for the wrong reason (tiebreaker masks number precision).** `retrieval_smoke.rs:85-92` queries `article 1240`, but *both* candidates match the shared term `article` (the decoy now contains `Article 1241`). The discriminator is meant to be the number `1240`. However, the ordering is `paradedb.score(...) DESC, chunk_id` and the target id `chunk:1240:0` sorts lexicographically before `chunk:recipe:0` (`'1' < 'r'`), so the target is returned even on a score tie — meaning the test would still pass if `1240` were not indexed/matched at all. To make number precision a true guard, either (a) add the reciprocal assertion that `1241` returns the recipe chunk, or (b) give the decoy an alphabetically-earlier `chunk_id` so only a real score difference can select the target.

**S4 — accent-insensitivity is order-fragile and narrowly covered (robustness).** In ParadeDB 0.24.1 the filter chain (`tokenizers/src/manager.rs` `add_filters!`) is `lowercase → stemmer → stopwords → ascii_folding → … → stopwords_language`, i.e. the **French Snowball stemmer runs on the still-accented token and accent folding happens *after* it.** Accent-insensitive matching is therefore not guaranteed by construction; it works only when the stemmer produces stems that differ between the accented and unaccented input by accents alone. It is verified for exactly one word family (`responsabilité`/`réparations` ↔ `responsabilite`/`reparation`). This is a ParadeDB-imposed ordering, not something the migration can change, so it is purely a coverage concern: add a couple more accented/unaccented legal pairs (e.g. `arrêté`/`arrete`, `créancier`/`creancier`, `procédure`/`procedure`) to bound the risk.

**S5 — no operator note for migration 9's index drop/rebuild (consistency with migration 8).** Migration 8's status carries an explicit operator note: *"migration 8 drops and rebuilds the pg_search BM25 index; on a corpus-scale populated index this can take time and temporarily removes the lexical index while the migration runs"* (`IMPLEMENTATION_PLAN.md:580`). Migration 9 does the **same** `DROP INDEX … ; CREATE INDEX …` and has the same lock/rebuild cost at corpus scale, but the 1.3 status adds no equivalent operator note. Add one for parity so operators aren't surprised by a second full BM25 rebuild on upgrade.

**S6 — the French analyzer lands before the harness that would measure its ranking impact.** Migration 8 left a follow-up: *"run a before/after BM25 ranking check on the target-spike corpus … before treating the longer header-prefixed lexical field as quality-neutral at scale"* (`IMPLEMENTATION_PLAN.md:581`). Migration 9 introduces a far larger ranking change (stemming + stopword removal + accent folding) than migration 8 did, yet the 1.3 acceptance gate that would quantify it — *"BM25-only, dense-only, hybrid, hybrid+authority ablations are measurable"* — is correctly still listed as *Remaining*. So the analyzer's corpus-scale quality is currently unmeasured; only the 2-document smoke proves directional correctness. This is acceptable sequencing (the plan is honest that ablations are pending), but flag that the migration-8 ranking-check follow-up should explicitly be re-scoped to cover the analyzer change once the ablation harness exists, and consider whether stopword removal could drop a legally-meaningful token (the list excludes `article`/`code`/`fait`/`cause`/`droit`, which is good, but the broader 154-word list deserves a legal-vocabulary pass — this is the "legal stopwords" half of the task).

## Verification notes

**Environment.** `~/.pgrx/18.4/pgrx-install/{bin/pg_config, lib/postgresql/pg_search.so, lib/postgresql/vector.so}` all present, so the *real* backend path runs (not the silent skip path in `tests/common/mod.rs`, which returns `Ok(None)` and reports a green pass when assets are absent — keep `JURISEARCH_REQUIRE_PG_EXTENSIONS=1` on the gating runner).

**V1 — tests pass on the real backend (forced).**
```
JURISEARCH_REQUIRE_PG_EXTENSIONS=1 cargo test -p jurisearch-storage \
  --test retrieval_smoke --test schema_migrations -- --nocapture --test-threads=1
```
→ `retrieval_smoke`: 2 passed (2.64s); `schema_migrations`: 2 passed (2.95s). This exercises: migration 9 applying cleanly and idempotently across two `start_durable` sessions; `indexdef LIKE '%ascii_folding%' AND '%French%'`; accent+stem query resolving to the legal chunk; statutory query resolving to the legal chunk; hybrid candidate JSON unchanged.

**V2 — tokenizer config keys are valid in the installed extension.** Read the ParadeDB fork at `/home/pierre/Work/paradedb` (HEAD `d087339c`, version `0.24.1`, `pgrx =0.18.1`). `tokenizers/src/manager.rs`: `SearchTokenizerFilters` defines `stemmer: Option<Language>`, `stopwords_language: Option<Vec<Language>>` (accepts a single string *or* an array), and `ascii_folding: Option<bool>`; `from_json_value` maps `"type": "default"` → `SearchTokenizer::Simple(filters)`. So all four keys in the migration are real and correctly typed; a typo would have failed `from_json_value` at `CREATE INDEX`, and V1 confirms it did not.

**V3 — elision / stopword behavior inspected at source.** `default` → Tantivy `SimpleTokenizer` (splits on non-alphanumerics, including `'`). `stopwords_languages()` uses `StopWordFilter::new(Language::French)`. The forked Tantivy French list (`…/tantivy@dcbfce2/src/tokenizer/stop_word_filter/stopwords.rs`, 154 entries) **includes** the elision particles `l, d, j, qu, c, n, s, t, m, à` and **excludes** the legal tokens `article, code, fait, cause, droit`. Hence elision is handled incidentally (S1/S2) and the tested legal terms survive stopword removal. Note one cosmetic edge: `stopwords_language` runs *after* `ascii_folding`, and the list stores `à` (accented) while folding has already produced `a`, so a folded `à→a` is not stopped — negligible.

**V4 — migration mechanics.** Migration 9 is version-gated (`run_migrations` skips applied versions), wrapped in `BEGIN … COMMIT` with the per-statement `INSERT INTO schema_migrations`, runs under the existing single-writer file + advisory locks (`runtime.rs`), and `validate_migration_list()` enforces contiguous versions and `latest == CURRENT_SCHEMA_VERSION` (= 9). `DROP INDEX IF EXISTS` + non-concurrent `CREATE INDEX` is transaction-safe and mirrors migration 8. The `SchemaVersionAhead` guard means an old (v8) binary will correctly refuse a v9 data dir. No down-migration — consistent with the project's forward-only design.

**V5 — no stale consumers.** No other test or source asserts schema version 8 or the pre-v9 `indexdef`. The CLI/core `SCHEMA_VERSION`/`CANONICAL_SCHEMA_VERSION` constants are the output-contract versions (`"1"`, `"canonical:v1"`), unrelated to the storage migration version; no `status`/CLI test pins the storage migration count. The query leg (`retrieval.rs:59`, `c.contextualized_body @@@ {query_text}` with `query_text = sql_string_literal(...)`) applies the field's analyzer symmetrically to query and index and is unchanged — the new tokenizer flows through it for free, and the temporal predicates remain applied independently to the lexical and dense pools before RRF (matches the status claim).

**Plan-accuracy summary.** The "Done" claims in the 1.3 status (accent folding, French stemming, French stopword removal, migration 9 drop/rebuild, manifest v9, the retrieval-smoke coverage description, independent temporal prefilter) are all accurate and verified. The gaps are omissions, not misstatements: elision/boosters dropped from the ledger (S1) and no migration-9 operator note (S5).
