# P8 Writable-App Reference Model + Validator Design Validation

## Verdict

**GO with adjustments.** The proposed soft-reference model fits the current storage topology: `jurisearch_app` is preserved across generation swaps, there is exactly one active generation per corpus, and `documents` has the identity fields needed for both pinned and logical references. Keep the validator out of the apply transaction.

The main corrections:

- Use `valid_to` as **exclusive**, not inclusive. Current retrieval code uses `d.valid_to > as_of` and even emits `"to_exclusive": true`.
- Add at least `invalid`/`unvalidated` handling to the validation status model. `resolved`/`changed`/`missing` cannot distinguish malformed references from legitimate misses.
- If you ship `target_kind IN ('chunk','zone_unit')`, add an app-level anchor payload such as `anchor_json jsonb`; do not imply that document-level resolution validates a specific derived chunk/zone.
- Read the active generation directly, but read `active_generation` and `schema_version` from `corpus_state` in the same validation transaction and stamp those exact values.

## Source Constraints Verified

`jurisearch_app` is created empty in migration v20 and is not part of replicated generation schemas. P5/P7 generation apply/switch logic operates on per-corpus `jurisearch_server_<corpus>_gNNNN` schemas plus `jurisearch_control`; it does not write `jurisearch_app`.

`jurisearch_control.corpus_state` has the active cursor fields P8 needs: `active_generation`, `sequence`, `baseline_id`, `schema_version`, `embedding_fingerprint`, `builder_versions`, and package identity. `generation_registry` has a partial unique index enforcing at most one `active` generation per corpus.

`documents` has `document_id` as PK plus `source`, `source_uid`, `version_group`, `valid_from`, and `valid_to`. `documents.corpus` is a stored generated column added in the corpus attribution migration and is cloned into generation schemas because generation tables are created from `public` table definitions.

The current code's temporal convention is half-open validity: `valid_from <= as_of` and `as_of < valid_to` when `valid_to` is present. `retrieval/citation.rs` uses `d.valid_to > as_of` and marks the JSON validity interval as `to_exclusive`.

P7 added `jurisearch-syncd::planner::read_client_cursor`, but that is a syncd service helper, not a storage primitive. A storage-level reference validator should query `jurisearch_control.corpus_state` itself or use storage generation helpers, not depend on `jurisearch-syncd`.

## Q1: Concrete `app_reference` Table

**GO, but keep it explicitly a reference implementation.** Shipping `jurisearch_app.app_reference` is the right P8 vertical slice because it gives the validator something real to validate and proves the reload boundary. It should not be presented as the only app model.

Do not put this state in `jurisearch_control`. Control is sync/client-position state: cursors, generation registry, trust anchors, licenses. App references are writable app data and belong in `jurisearch_app`, which is exactly the schema preserved across server re-baselines.

I would define the table with a few more guardrails:

```sql
CREATE TABLE jurisearch_app.app_reference (
    reference_id bigserial PRIMARY KEY,
    target_kind text NOT NULL CHECK (
        target_kind IN ('document_version','logical_article','decision','chunk','zone_unit')
    ),
    corpus text NOT NULL,
    document_id text,
    source text,
    source_uid text,
    version_group text,
    as_of_date date,
    anchor_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    resolved_document_id text,
    resolved_generation text,
    resolved_schema_version integer,
    validated_at timestamptz,
    validation_status text NOT NULL DEFAULT 'unvalidated'
        CHECK (validation_status IN ('unvalidated','resolved','changed','missing','invalid'))
);
```

Indexes worth adding now:

- `(corpus, validation_status)`
- `(corpus, document_id)` for pinned references
- `(corpus, source, version_group, as_of_date)` and/or `(corpus, source, source_uid, as_of_date)` for logical references

`anchor_json` is important if you keep `chunk` and `zone_unit` in the target vocabulary. The design says those should be anchored at document/article level plus offsets or quote hashes, not by derived IDs. Without an anchor column, P8 can only validate “the parent document exists,” not any chunk/zone-level target.

## Q2: Active Generation vs Stable Views

**Read the active physical generation directly.** For P8, direct generation reads are better than `jurisearch_server.*` views:

- You need to stamp `resolved_generation`.
- A corpus has exactly one active generation, enforced by `generation_registry_one_active_per_corpus`.
- The stable views are `UNION ALL` across active corpora, which is useful for read transparency but less precise for per-corpus validation.
- Direct schema reads avoid accidental cross-corpus matches and make it obvious which generation was validated.

Implementation shape:

1. Start a transaction.
2. Read `active_generation` and `schema_version` from `jurisearch_control.corpus_state` for the corpus.
3. Convert `active_generation` with `schema_for_generation`.
4. Resolve references against that schema's `documents`.
5. Stamp `resolved_generation = active_generation` and `resolved_schema_version = schema_version`.
6. Commit the app-reference updates.

This does not miss a multi-generation case for a single corpus; the topology intentionally has one active generation per corpus. Multiple active generations only exist across different corpora.

The only concurrency caveat is async retired-generation cleanup. If validation reads a cursor and then a re-baseline switches the corpus before the document query, you may validate the just-retired generation. For a background validator that is acceptable if it is retried after apply, but the robust implementation is to do the cursor read and resolution in one short transaction and, if the schema lookup/query fails because the schema disappeared, reread the cursor and retry once.

## Q3: Status Vocabulary and Change Semantics

**ADJUST the vocabulary.** `resolved` / `changed` / `missing` is not enough because some rows are malformed rather than missing. Add:

- `unvalidated`: default before first validation
- `invalid`: unsupported `target_kind` or insufficient identity columns for that kind

Then keep your definitions:

- `resolved`: target exists and either this is the first validation or the resolved document is the same as the previous `resolved_document_id`
- `changed`: a logical target with a previous `resolved_document_id` now resolves to a different `document_id`
- `missing`: the reference is well-formed but no target exists in the active generation
- `invalid`: the reference row cannot be interpreted

Yes: a re-baseline that re-resolves a pinned `document_id` to the same `document_id` should be `resolved`, not `changed`. For pinned `document_version` / `decision`, the semantic target is the immutable `document_id`; a generation or schema stamp changing underneath it is not a target change.

For logical references, only mark `changed` when there was a previous non-null `resolved_document_id` and the new resolved ID differs. On first validation, a found logical reference should be `resolved`, not `changed`.

## Q4: Validator Placement

**Make it a storage function, called after apply by the service or tests.** That matches §7.1/§8.2: reference validation is the one genuinely post-cursor background task.

Do not wire it inside the low-level apply transaction. `apply_baseline`, `apply_rebaseline`, and `apply_incremental` currently have a clean invariant: package rows, indexes, postconditions, and cursor movement succeed or fail together. App-reference validation is advisory app state and should not be able to roll back a successfully applied package.

Recommended P8 scope:

- `jurisearch_storage::reference::{resolve_reference, validate_references}`
- tests call it directly after apply
- `syncd` may expose/call it after successful apply, but scheduler/retry wiring can wait for P9

Return a `ValidationReport` with counts by status and maybe `generation`/`schema_version` validated against. That gives the service enough observability without entangling it with app UX.

## Q5: Supersession, Re-Baselines, and Logical Resolution

Pin-by-`document_id` survival works if the producer's authoritative corpus retains superseded version rows. The package system copies the current producer state into baselines/re-baselines; it does not invent old rows. So the invariant is not “re-baseline magically preserves pins,” it is “producer public retains superseded rows, and re-baseline packages include them.” That matches the decided architecture and current tests around `valid_to` replication.

The logical-article query should be:

```sql
WHERE d.corpus = $corpus
  AND ($source IS NULL OR d.source = $source)
  AND (
      ($version_group IS NOT NULL AND d.version_group = $version_group)
      OR ($version_group IS NULL AND d.source_uid = $source_uid)
  )
  AND (d.valid_from IS NULL OR d.valid_from <= $as_of::date)
  AND (d.valid_to IS NULL OR $as_of::date < d.valid_to)
ORDER BY d.valid_from DESC NULLS LAST, d.document_id
LIMIT 1;
```

Do not use `as_of <= valid_to`; that would make the end date belong to both the closing version and the next version on boundary days.

Also be strict about shape:

- `document_version`: require `document_id`
- `decision`: prefer `document_id`; optionally support `(source, source_uid)` as a repair path, but do not blur it with temporal article logic
- `logical_article`: require `as_of_date` plus either `version_group` or `source_uid`; include `source` when available to avoid cross-source collisions
- `chunk` / `zone_unit`: require `document_id` or logical article identity plus `anchor_json`; validate the parent document only in P8 unless you implement quote/offset re-anchoring

## Implementation Notes

Keep the validator write set narrow: update only `resolved_document_id`, `resolved_generation`, `resolved_schema_version`, `validated_at`, and `validation_status`. Do not rewrite the semantic identity columns.

Use generated-column-aware expectations. You do not insert `documents.corpus`; it is generated from `source`, but it is available for filtering in both `public` and generation schemas.

Handle missing corpus cursor distinctly. If `corpus_state` has no row for the referenced corpus, mark rows `missing` or report corpus-not-installed; do not fall back to `public` or `jurisearch_server` empty views silently.

Avoid hard cross-schema FKs completely. Even `NOT VALID` FKs from app tables into generation schemas would reintroduce exactly the reload coupling §8 rejects.

## Bottom Line

Ship the `jurisearch_app.app_reference` table as a thin reference implementation, not a mandatory app framework. Resolve directly against the corpus's active physical generation, stamp the cursor generation/schema used, and use half-open validity windows. Add `unvalidated`/`invalid` plus an `anchor_json` column if chunk/zone references are in scope. Keep validation post-commit and retryable; never let it participate in the package apply transaction.
