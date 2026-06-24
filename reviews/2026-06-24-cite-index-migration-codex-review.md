# Code Review: decision-citation index migration v11 + pourvoi query rewrite

## Findings

No BLOCKER/WARN/NIT findings in the scoped working-tree diff.

## Review Notes

- `CURRENT_SCHEMA_VERSION` is bumped to 11 and the new migration is contiguous with the existing migration list.
- The ECLI index matches the existing decision-only predicate shape: `d.kind = 'decision' AND upper(d.canonical_json->>'ecli') = ...`.
- The pourvoi rewrite preserves the previous lookup semantics for canonical decision data: the input and stored `case_numbers` values are both normalized by removing dots and spaces, and `@> ARRAY[...]::text[]` preserves the previous "any case number equals the normalized query" behavior.
- `document_lookup_sql(&predicate, "TRUE")` preserves the previous `exact_identifier_match` behavior for pourvoi hits.
- I did not run migrations or mutate any index, per the static-review constraint. Static checks used: `git diff`, focused source inspection, and `git diff --check` for the two scoped files.

VERDICT: GO
