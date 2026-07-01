# Review — Judilibre Enrichment Decision-Date Cutoff

## Findings

### WARN — Example config says omission disables the cutoff, but omission actually enables the default cutoff

`crates/jurisearch-producer/src/config.rs:716`

The shipped `PRODUCER_CONFIG_EXAMPLE` comment says `Omit/`null` to attempt all (historical)`, but the implemented serde default on `EnrichmentConfig.min_decision_date` means an omitted field becomes `Some("2016-01-01")`. The code and backward-compat test intentionally make old `[enrichment] mode = "auto"` configs use the 2016 cutoff, so the example comment is misleading operator guidance.

Concrete fix: change the example comment to say that omission uses the default 2016 cutoff, and only `null` disables the cutoff, e.g. `Omit to use the default 2016-01-01 cutoff; set to null to attempt all (historical behavior).`

### NIT — The no-cutoff SQL text is not byte-identical to the pre-change selector

`crates/jurisearch-storage/src/zone_units.rs:134`

When `min_decision_date` is `None`, the new `{decision_date_predicate}` placeholder expands to an empty string on its own line. That leaves an extra whitespace-only SQL line compared with the previous selector. This has no semantic effect and the returned JSON remains unchanged, but it does not satisfy the stated byte-identical SQL requirement for the `None` path.

Concrete fix: include the leading newline/indentation inside the optional predicate and splice it adjacent to `{cursor_predicate}`, so the formatted SQL is exactly the old text when `None` and gains the cutoff line only when `Some`.

### NIT — Config validation tests miss the day-range cases called out by the validator requirement

`crates/jurisearch-producer/tests/config_and_fingerprint.rs:120`

`is_iso_calendar_date` correctly rejects impossible calendar dates in source review, including `2016-02-30`, `0000-00-00`, and non-leap `YYYY-02-29`. The malformed-date test currently covers malformed shape and invalid month, but not month-specific day ranges or year zero. A future regression that only checks `1..=31` days could still pass this test.

Concrete fix: add rejected cases such as `2016-02-30`, `2019-02-29`, `0000-00-00`, and `2016-04-31`, plus at least one accepted leap-day case such as `2020-02-29`.

## Verified

- The active cutoff predicate is injection-safe through `sql_string_literal`, excludes `NULL valid_from`, and is placed after the `z.status IS NULL OR ...` group, so it ANDs with the whole candidate status/expiry condition.
- The producer thread passes `config.enrichment.min_decision_date.as_deref()` into `EnrichRequest`, while the CLI defaults to `None` and forwards an explicit `--min-decision-date` when provided.
- `load_derivable_decision_zones_json_with_client`, dense finalize, and producer enrich order remain unchanged by this diff.
- The new storage cutoff test proves pre-cutoff exclusion, exact-cutoff inclusion, NULL-date exclusion under cutoff, and no-cutoff inclusion.

VERDICT: FIXES_REQUIRED
