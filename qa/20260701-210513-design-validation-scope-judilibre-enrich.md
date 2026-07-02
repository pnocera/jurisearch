Verdict: **GO-with-adjustments.** The right fix is a decision-date cutoff in the enrichment candidate selector, threaded through `EnrichRequest` and configured by the producer with a default cutoff of `2016-01-01`. This prevents quota-burning attempts before any HTTP work is scheduled and does not affect zone-unit derivation except by reducing negative `decision_zones` churn.

## Required Adjustments

1. **Put the cutoff in the storage candidate selector.**

   The load-bearing waste is in `enrich_zone_candidates_json_with_client`: it pages all resolver-reachable cass/inca decisions with missing/expired/incomplete `decision_zones` (`crates/jurisearch-storage/src/zone_units.rs:63-126`). That is the correct layer to filter. Filtering later in the producer or worker would still page useless candidates and risks surprising counts; filtering after HTTP is obviously too late.

   Change the selector signature to something like:

   ```rust
   pub fn enrich_zone_candidates_json_with_client(
       client: &mut postgres::Client,
       source: &str,
       after_cursor: Option<&str>,
       since: Option<&str>,
       min_decision_date: Option<&str>,
       limit: u32,
       order: EnrichZoneOrder,
   ) -> Result<String, StorageError>
   ```

   Add the predicate only when `min_decision_date` is `Some`:

   ```sql
   AND d.valid_from IS NOT NULL
   AND d.valid_from >= '<cutoff>'::date
   ```

   Use the existing `sql_string_literal` helper, as the selector already does for `source`, cursor, and `since`.

2. **Use `valid_from`; exclude NULL when a cutoff is active.**

   `valid_from` is the right date column. Jurisprudence projection explicitly maps `valid_from = decision_date` for decisions (`crates/jurisearch-storage/src/projection/decisions.rs:17-20`, `:78-107`). The ingest canonical type requires `decision_date` and validates it as ISO `YYYY-MM-DD` (`crates/jurisearch-ingest/src/juri/types.rs:101-113`, `:185-188`), and the parser requires `DATE_DEC` (`crates/jurisearch-ingest/src/juri/parser.rs:288-305`).

   The table column is nullable because `documents` is shared with legislation and historical rows (`crates/jurisearch-storage/src/migrations.rs:47-58`), but a cass/inca decision with NULL `valid_from` is not a normal, enrichable projected decision. The Judilibre resolution metadata also sends `valid_from::text` as `decision_date` (`crates/jurisearch-storage/src/decision_zones.rs:52-55`, `:94-109`). So when a cutoff is configured, NULL should be excluded, not included. Including NULL would preserve the quota problem and likely produce lookup failures with no date.

   If you are worried about hand-loaded anomalies, add a preflight/diagnostic query, not enrichment attempts:

   ```sql
   SELECT source, count(*)
   FROM documents
   WHERE kind = 'decision'
     AND source IN ('cass','inca')
     AND valid_from IS NULL
   GROUP BY source;
   ```

3. **Thread through `EnrichRequest`; keep CLI backward-compatible.**

   Add `pub min_decision_date: Option<&'a str>` to `crates/jurisearch-pipeline/src/enrich.rs::EnrichRequest`. Destructure it in `enrich_zones_inner`, include it in the skipped/no-credentials and normal JSON body, and pass it to `enrich_zone_candidates_json_with_client`.

   Update the CLI `ingest enrich-zones` path, but make its default `None` to preserve current local/dev behavior:

   - `crates/jurisearch-cli/src/args.rs::IngestSubcommand::EnrichZones`: add `--min-decision-date <YYYY-MM-DD>`.
   - `crates/jurisearch-cli/src/ingest.rs`: pass it through.
   - `crates/jurisearch-cli/src/ingest/pipeline.rs::enrich_zones_payload`: populate `EnrichRequest { min_decision_date, ... }`.

   This keeps the CLI working and lets local tests/backfills still ask for historical behavior explicitly by omitting the flag.

4. **Add a producer config key with a default cutoff.**

   Producer config is strict (`#[serde(deny_unknown_fields)]`) and `[enrichment]` currently only has `mode` (`crates/jurisearch-producer/src/config.rs:255-260`). Add a serde-defaulted field so existing configs continue to parse:

   ```rust
   pub struct EnrichmentConfig {
       pub mode: EnrichmentModeConfig,
       #[serde(default = "default_judilibre_min_decision_date")]
       pub min_decision_date: Option<String>,
   }

   fn default_judilibre_min_decision_date() -> Option<String> {
       Some("2016-01-01".to_owned())
   }
   ```

   Validate the configured value in `ProducerConfig::validate`: if present, it must be exactly `YYYY-MM-DD` and parseable enough for PostgreSQL `::date`. There is a private ISO-date validator in `jurisearch-ingest::juri`, so the clean minimal option is a small local helper in producer config validation, or a tiny shared utility if you already have one in scope. Do not rely on a runtime PostgreSQL cast error for operator config validation.

   In `crates/jurisearch-producer/src/update.rs::enrich_group`, pass:

   ```rust
   min_decision_date: config.enrichment.min_decision_date.as_deref()
   ```

   A default cutoff is appropriate for the producer. The empirical CT110 data shows pre-2016 attempts are nearly pure quota waste, and steady-state timers should never grind that backlog before reaching new decisions. Operators who need a historical experiment can use the CLI/local path or a deliberate config override later; production default should be protective.

5. **Do not change zone-unit derivation or finalize coverage.**

   `load_derivable_decision_zones_json_with_client` consumes only `decision_zones` rows with `z.status = 'ok'`, non-null `text_hash`, non-expired TTL, cass/inca source, parser-valid pourvoi, and absent/stale `zone_units` (`crates/jurisearch-storage/src/zone_units.rs:276-333`). The cutoff belongs to enrichment, not derivation. Once a recent decision gets `status='ok'`, derivation and zone-unit embedding behave exactly as they do now.

   Finalize coverage is also unaffected: zone-unit dense finalize checks the current `zone_units` table against `zone_unit_embeddings`, not the population of never-attempted historical decisions.

6. **Keep `order=Oldest`; the cutoff fixes the bad queue.**

   Switching producer order to `Recent` would only mask the issue: old pre-cutoff candidates would still remain and eventually burn quota. With the cutoff in SQL, `Oldest` becomes acceptable again because the oldest candidate is now the oldest enrichable candidate, not 377k pre-2016 dead work.

## Expected Behavior

With producer default `min_decision_date = "2016-01-01"`, the next `update --group jurisprudence` enrichment pass should select:

- `status IS NULL` cass/inca decisions with `valid_from >= 2016-01-01`,
- plus any >=cutoff expired rows,
- plus any >=cutoff fresh `ok`/`invalid_offsets` rows with `text_hash IS NULL`.

Given the stated live facts and a near-term rerun, that should be approximately the **3,612** valuable recent un-enriched decisions, not the 377,399 pre-2016 backlog. The 3,592 newly ingested catch-up decisions with `valid_from` 2022-11..2026-06 are included. The ~41k already-written pre-2016 negative rows stay in `decision_zones` but are no longer expanded by future producer runs; they will be replicated only if they are in the next package window from prior writes.

Those recent `ok` rows then flow into the already-committed Phase 4.5:

1. `enrich_group` writes `decision_zones`.
2. `derive_zone_units_if_applicable` derives units from `status='ok'` rows.
3. `embed_pending(EmbedTarget::ZoneUnits)` embeds and finalizes zone units.
4. Phase 6 incremental publish captures the `decision_zones`, `zone_units`, and `zone_unit_embeddings` outbox rows in the same package window.

So the target outcome, normally `core-3-4` after the existing `core-2-3`, is correct.

## Minimal Correct Slice

1. **Storage selector**

   Files/symbols:

   - `crates/jurisearch-storage/src/zone_units.rs`
   - `enrich_zone_candidates_json`
   - `enrich_zone_candidates_json_with_client`

   Add `min_decision_date: Option<&str>` and the optional `valid_from` predicate. Update storage tests in `crates/jurisearch-storage/tests/zone_units.rs` to cover:

   - pre-cutoff status-null cass decision excluded,
   - cutoff-or-newer status-null cass decision included,
   - NULL `valid_from` excluded when cutoff is set,
   - existing no-cutoff behavior unchanged.

2. **Pipeline request**

   Files/symbols:

   - `crates/jurisearch-pipeline/src/enrich.rs`
   - `EnrichRequest`
   - `enrich_zones_inner`

   Add `min_decision_date`, pass it to storage, and include it in the JSON body for observability.

3. **Producer config and call site**

   Files/symbols:

   - `crates/jurisearch-producer/src/config.rs`
   - `EnrichmentConfig`
   - `ProducerConfig::validate`
   - `crates/jurisearch-producer/src/update.rs::enrich_group`

   Add `min_decision_date` with default `Some("2016-01-01")`, validate the format, document it in the sample/config rendering area around `[enrichment]` (`crates/jurisearch-producer/src/config.rs:620-675`), and pass it into `EnrichRequest`.

4. **CLI compatibility**

   Files/symbols:

   - `crates/jurisearch-cli/src/args.rs::IngestSubcommand::EnrichZones`
   - `crates/jurisearch-cli/src/ingest.rs`
   - `crates/jurisearch-cli/src/ingest/pipeline.rs::enrich_zones_payload`

   Add optional `--min-decision-date`; default remains `None`.

5. **Tests**

   Add at least:

   - storage selector tests for cutoff/no-cutoff/NULL behavior,
   - producer config parse test proving old `[enrichment] mode = "auto"` still parses and defaults to `2016-01-01`,
   - producer config validation rejects malformed dates,
   - pipeline/CLI request construction test if existing CLI contract tests make this cheap.

## Non-blockers / Deferred

- You do not need to remove existing pre-2016 negative `decision_zones` rows to fix the run strategy. They are already written; the cutoff prevents more.
- You do not need to change `load_derivable_decision_zones_json_with_client`, `build_zone_units`, or dense finalize.
- A future richer policy could use per-source or per-provider cutoff dates, but a single Judilibre `min_decision_date` under `[enrichment]` is sufficient for this production problem.

Final verdict: **GO-with-adjustments.** Implement the cutoff in the candidate SQL, default the producer to `2016-01-01`, keep CLI default behavior unchanged, exclude NULL dates under the cutoff, and leave derivation/finalize untouched.
