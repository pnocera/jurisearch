# A3 Config Validation Re-Review R3

## Findings

No BLOCKER/WARN/NIT findings.

The r2 gap is closed: `search_accepts_positive_authority_weight_on_decision_kind_without_cursor`
locks the allowed one-shot CLI path (`--kind decision --authority-weight 0.5`, no cursor) to reach
`index_unavailable` instead of A3 `bad_input`, and the same test covers the JSONL session request path.

I rechecked the current A3 wiring against the acceptance matrix:

- `SearchArgs` exposes `--authority-weight` and maps it into `SearchRequest`.
- `SearchRequest` deserializes `authority_weight` for session JSON and carries it into `RetrievalOptions`.
- `validate_retrieval_options` rejects non-finite or out-of-range values and leaves `0.0` valid but inert.
- The main search path rejects positive effective authority weight for `kind=all`/`kind=code`, and rejects
  positive authority weight with an inbound cursor before index access.
- The zone path allows the decision-implied zone route but still rejects positive authority weight with an
  inbound cursor.
- The schema/golden schema exposes `authority_weight` as a bounded numeric session field.
- Field-by-field `SearchRequest` constructors in eval/scoring now initialize `authority_weight: None`.

## Verification

- `cargo test -p jurisearch-cli authority_weight -- --nocapture` passed.
- `cargo test -p jurisearch-cli help_schema_json_is_valid_and_lists_commands -- --exact` passed.
- `cargo check -p jurisearch-cli` passed.
- Manual CLI/session spot checks confirmed the accepted positive decision case reaches `index_unavailable`,
  while positive `kind=all` rejects with A3 `bad_input`.

VERDICT: GO
