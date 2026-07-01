# Review - Judilibre Enrichment Decision-Date Cutoff r3

## Findings

No findings.

## R2 Deviation Review

The docs-only resolution is acceptable. The r2 warning was about a documented `null` escape hatch that TOML cannot express; the current diff removes that promise from both producer-facing documentation points:

- `crates/jurisearch-producer/src/config.rs:260` now documents `min_decision_date` as a `YYYY-MM-DD` cutoff, says omission defaults to `2016-01-01`, explicitly notes TOML has no `null`, and directs operators to set an earlier date such as `1900-01-01` when they want to widen coverage.
- `crates/jurisearch-producer/src/config.rs:714` makes the same contract visible in `PRODUCER_CONFIG_EXAMPLE`.

Given the stated production policy - pre-cutoff decisions have no useful Judilibre zone coverage and enriching them wastes quota - I do not see a concrete production need for a TOML-representable disable switch. The required operator escape path is "move the cutoff earlier," not "remove the predicate entirely," and the new test proves that path parses, validates, and loads as `Some("1900-01-01")`.

The pipeline and CLI still preserve the lower-level `None` behavior for explicit/manual callers (`None` omits the SQL predicate and restores historical candidate selection), so this docs-only producer decision does not remove the generic API's ability to attempt all candidates where it is still intentionally exposed.

## Verified

- `cargo test -p jurisearch-producer --test config_and_fingerprint min_decision_date` passed: the example default, earlier-date widening path, and malformed-date rejection tests all pass.
- `cargo check -p jurisearch-cli -p jurisearch-producer -p jurisearch-pipeline -p jurisearch-storage` passed.
- The cutoff validation rejects malformed/nonexistent dates before PostgreSQL casts are involved, including invalid leap days and impossible month/day values.
- The SQL predicate is still appended outside the status/expiry/refresh `OR` group, so the cutoff applies to the whole candidate eligibility group.
- The `None` SQL path remains predicate-free, while the producer config load path defaults omission to `Some("2016-01-01")`; that matches the revised producer contract.

VERDICT: GO
