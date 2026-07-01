# Review — Judilibre Enrichment Decision-Date Cutoff r2

## Findings

### WARN — The documented `null` escape hatch cannot be expressed in TOML

`crates/jurisearch-producer/src/config.rs:262` and `crates/jurisearch-producer/src/config.rs:719`

The r2 docs/comments now correctly distinguish omission from disablement, but the disable path they document is not actually usable in `producer.toml`: TOML has no `null` literal. With `#[serde(default = "default_judilibre_min_decision_date")] pub min_decision_date: Option<String>`, an omitted key becomes `Some("2016-01-01")`, while `min_decision_date = null` is a parse error before serde can produce `None`. That means operators cannot configure the promised "attempt all / historical behavior" path through the producer config.

Concrete fix: add a config test that replaces the example line with the documented disabled form and proves it loads to `None`, then implement a TOML-representable disable mechanism. For example, use a separate boolean such as `disable_min_decision_date = true`, or a custom string deserializer accepting a documented sentinel like `"none"`/`"disabled"` while still defaulting omission to `2016-01-01`.

## Verified

- The prior misleading-comment finding is only partially addressed: omission is now documented as defaulting to `2016-01-01`, but the documented `null` disable form is not parseable TOML.
- The optional cutoff predicate is spliced as `){decision_date_predicate}`, and the predicate owns its leading newline/indentation, so the `None` path restores the pre-cutoff SQL shape while the `Some` path adds a valid `AND d.valid_from IS NOT NULL AND d.valid_from >= ...::date` line.
- The cutoff predicate is placed after the status/expiry/refresh `OR` group, so it ANDs with the whole candidate eligibility group rather than only the final `OR` branch.
- The new date-validation tests cover the requested calendar-invalid cases (`2016-02-30`, `2019-02-29`, `0000-00-00`, `2016-04-31`) and a valid leap day (`2020-02-29`).
- The producer default/backward-compat path for omitted `min_decision_date` still loads as `Some("2016-01-01")`, and the producer passes that value through to `EnrichRequest`.
- The diff does not alter zone-unit derivation, zone-unit embedding/finalize logic, or the existing enrichment walk order beyond threading the new cutoff argument.

I did not rerun the test suite for this review.

VERDICT: FIXES_REQUIRED
