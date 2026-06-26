Recommend **(b): remove the reserved `fetch --as-of` / `--part` flags for Phase 0** and keep `fetch` as exact-ID retrieval.

The low-risk reason is that there is no cheap existing “version-group or undated ID -> document valid at date” primitive to wire in. `fetch_payload` currently calls `fetch_documents_json`, whose SQL joins `documents` by exact `document_id` only. `context_payload` passes `as_of` to `context_documents_json`, but that SQL still starts from an exact `document_id`; `as_of` only filters whether that exact target is visible and filters siblings, it does not resolve to another version in the same group. `cite_payload` uses `citation_lookup_json` and then classifies/annotates candidates with `candidate_valid_on`; that is a candidate lookup plus validity annotation, not a single pinned fetch resolver. The structured citation resolver, `resolve_legi_citation_json`, does apply validity predicates:

```sql
AND (d.valid_from IS NULL OR d.valid_from <= {as_of}::date)
AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)
```

but it is article-title/code-hint search SQL and returns ranked candidates, not an exact identifier/version-group fetch contract.

So wiring (a) would define new semantics, not just expose existing behavior: what inputs count as “undated ID”, whether `source_uid` or `version_group` is authoritative, how ambiguity is reported, how multiple IDs preserve ordering, how missing/stale versions behave, and how `--part` slices document/chunk output. That is beyond a Phase 0 “honesty / no new capability / low risk” task. Removing the flags is cleaner because it makes the CLI contract match the implementation: `fetch` retrieves full documents for exact, version-pinned IDs like `legi:LEGIARTI000006850948@1994-08-21`; date-resolved or sliced fetch can be designed later alongside `versions` / `diff`, with a dedicated storage resolver and tests.
