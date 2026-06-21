# Claude Review - Hierarchy Eval Fixtures

Verdict: GO

Reviewed: uncommitted Phase 1.2 hierarchy-sensitive eval fixtures
(`crates/jurisearch-core/src/eval.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`).
Reviewer: Claude (Opus 4.8), 2026-06-21.

This step is additive, correct, well-tested, and satisfies the stated intent. The fixture-tiering
design correctly quarantines unverified seed data as non-gating, the machine-checkable hierarchy
expectations cover exactly the requested fields, and the two seed fixtures are grounded in real,
present DILA archive members. No blocking issues.

## Blocking findings

None.

## What I verified

- **No regression surface.** The baseline `eval.rs` (39 lines) contained only the types plus
  `is_release_gating`, with **no** pre-existing fixtures, and a workspace grep finds **no external
  callers** of `is_release_gating`, `LegalRetrievalFixture`, `FixtureTier`, or
  `phase1_hierarchy_dev_fixtures`. So the `is_release_gating` semantics change (now also requiring
  `tier == ReleaseGating`) cannot silently downgrade any existing fixture or break any consumer — the
  module is self-contained schema/data.
- **Gating logic is correct and matches intent.** `is_release_gating` = `ReleaseGating` tier AND
  `is_source_verified` (status ∈ {OfficialSourceChecked, HumanReviewed, HeldOut} AND non-empty
  `verified_against`) AND status ∈ {HumanReviewed, HeldOut} AND non-empty `reviewer`. The stricter
  human-review requirement correctly makes an `OfficialSourceChecked` fixture source-verified but
  *not* release-gating. The unit test exercises the tier gate, empty `verified_against`, and
  whitespace-only reviewer.
- **`HierarchyExpectation` covers all requested machine-checkable fields:** `context_document_id`,
  `as_of`, `expected_ancestry_titles`, `required_sibling_ids`, `forbidden_sibling_ids`. Serde
  round-trip (incl. the new `tier`/`hierarchy` fields) is tested.
- **Seed fixtures are grounded, not fabricated.** Both are `tier=Dev`, `reviewer=None`,
  `review_status=OfficialSourceChecked`, so `is_release_gating()` is false (asserted by test). I
  confirmed every cited UID — `LEGITEXT000005615128` and `LEGIARTI000006850357/359/360/361/372/373` —
  is present in `Freemium_legi_global_20250713-140000.tar.gz`, and the ancestry titles match the
  actual decree text I had previously extracted from `LEGITEXT000005615128.xml` (Décret n°94-46;
  "TITRE Ier : Dispositions relatives aux organismes génétiquement modifiés…" verbatim). The two
  fixtures are also **temporally self-consistent**: `…850360@1994` is a *required* sibling at
  `as_of=1996` but a *forbidden* sibling at `as_of=2000`, while `…850361@1999-03-28` (the same-NUM
  replacement) is the target at 2000 — exactly the version-filtering behaviour the eval is meant to
  pin. This is reasoned seed data, not noise.
- **Plan accuracy.** The Done line says coverage is "represented in `jurisearch-core`" (definitions),
  not that a harness executes it — no overclaim. `cargo test -p jurisearch-core eval` (3 passed) and
  `cargo clippy -p jurisearch-core --all-targets -- -D warnings` pass locally.

## Non-blocking suggestions

1. **Fixtures are inert until a harness runs them.** `phase1_hierarchy_dev_fixtures()` /
   `HierarchyExpectation` are only exercised by unit tests; nothing yet calls `context_documents_json`
   and compares ancestry / required / forbidden siblings, so these "cases" provide no regression
   protection yet. Recommend tracking the eval-harness-wiring follow-up explicitly so the fixtures
   don't sit unused (the plan's "Done" reads slightly stronger than "definitions exist").
2. **Add a fixture self-consistency test.** Assert per-fixture invariants:
   `required_sibling_ids ∩ forbidden_sibling_ids == ∅`, the context target is not in its own
   sibling sets, and (optionally) `context_document_id` is among `expected_ids`. A contradictory
   fixture currently wouldn't be caught.
3. **Add an explicit legacy-deserialization test.** The round-trip test always serializes *with*
   `tier`/`hierarchy`; a test that deserializes a minimal JSON omitting both (confirming
   `tier → Dev`, `hierarchy → None`) would lock the `#[serde(default)]` backward-compat guarantee,
   which matters if fixtures are ever persisted as JSON files.
4. **Make the reviewer's verification mechanical.** Data accuracy is correctly deferred to a named
   legal reviewer before any promotion to `ReleaseGating`; to make that promotion low-effort,
   consider recording in `verified_against` (or a structured field) the exact `LEGISCTA` section UID
   each required/forbidden sibling belongs to and the validity window driving each temporal
   inclusion/exclusion, so the reviewer can check membership/dates directly against the cited members.
5. Minor: `is_source_verified`'s `OfficialSourceChecked` branch is effectively re-narrowed by
   `is_release_gating`'s stricter `{HumanReviewed, HeldOut}` check — intended and correct, but a
   one-line doc comment on each method clarifying the "source-verified ⊋ release-gating" relationship
   would help future readers.
