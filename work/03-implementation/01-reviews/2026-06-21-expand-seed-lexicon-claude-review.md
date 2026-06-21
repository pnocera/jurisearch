# Review — Phase 1.3 `expand` seed-lexicon

**Date:** 2026-06-21
**Reviewer:** Claude (Opus 4.8)
**Scope:** uncommitted changes — `jurisearch-core/src/expand.rs` (new), `lib.rs`, `contract.rs`, `schema.rs`, CLI wiring in `jurisearch-cli/src/main.rs`, contract tests in `cli_contract.rs`, and `IMPLEMENTATION_PLAN.md`.

## Verification performed

- `cargo test -p jurisearch-core --lib expand` — 3 passed.
- `cargo test -p jurisearch-cli --test cli_contract expand` + `help_schema_json` — 3 passed.
- `cargo clippy -p jurisearch-core -p jurisearch-cli` — clean (no warnings/errors).
- Read full `expand.rs`, the main.rs one-shot/JSONL wiring, schema additions, and the surrounding sibling conventions.

## Findings

### Correctness — solid
- **Normalization is sound and deliberate.** `normalize_for_match` lowercases (via `to_lowercase()`, so uppercase accents fold first), strips French diacritics (`à â ä ç é è ê ë î ï ô ö ù û ü œ æ`), maps non-alphanumerics to single spaces, and trims. Padded substring matching (`" {term} "`) correctly prevents intra-word false positives (e.g. `action` will not match inside `reaction`). Hyphens collapse to spaces uniformly on both sides, so `article 1231-1` round-trips consistently for dedup while the original human-readable form is preserved in the emitted `term`.
- **Deterministic output.** Emission order follows `EXPANSION_SEEDS` then per-seed `expanded_terms` declaration order; `seen: BTreeSet` is used only for dedup membership, not ordering. No `Date`/random/env dependence. Good fit for a contract-stable CLI.
- **Query-echo suppression works.** Expanded terms already present in the query (e.g. `faute`, `dommage` for query "faute et dommage") are filtered via `contains_normalized_phrase`, so output is genuinely additive.
- **Legal citations are accurate.** Articles 1240/1241 (responsabilité délictuelle, post-2016 renumbering of 1382/1383), 1103 (force obligatoire), 1231-1 (dommages-intérêts pour inexécution), 2219/2224 (prescription extinctive; 2224 = 5-year general civil limitation) all reference the current Code civil correctly.
- **Conservative review-metadata flagging (strength).** Every term carries `review_status: "dev_seed_pending_legal_review"` and `reviewer: "pending_legal_domain_review"`. For a legal-search product this is the right posture — unreviewed expansions are not presented as authoritative, and the metadata is machine-checkable downstream.

### Design / minor
- **`matched_terms` is denormalized per term.** Each `ExpandedTerm` clones the seed's full matched-terms list, so a query hitting one seed repeats the same array on every emitted term. Harmless and arguably convenient for consumers, but verbose in JSON; a per-response `matched_terms` (or per-seed grouping) would be leaner if payload size ever matters.
- **Phrase-only matching is intentional but worth documenting.** Match terms must appear as contiguous phrases: `dommages interets` will not match query "dommages **et** interets". This is conservative and appropriate for a curated lexicon, but the boundary may surprise users; a one-line note in the seed module would help future maintainers.
- **Redundant empty-query guard.** `Command::Expand` checks `args.query.trim().is_empty()` and `expand_payload` re-checks. Not a bug — `expand_payload` is the shared validation point for the session path — and it matches the existing `context` pattern, so no change required.

### Tests
- One-shot, JSONL session, and `help schema --json` paths are all covered, including review metadata, `matched_terms`, seed provenance, `seed_version`, and the no-index requirement. Core unit tests cover match, accent/punctuation insensitivity, and the empty-expansion case.
- **Gap (minor):** the empty-query error branch (`"expand query must not be empty"`) is untested for both the one-shot CLI and the session path. The reachable session route (`{"query":""}`) deserializes fine and is rejected only inside `expand_payload`, so a regression there would be silent.

### Out of scope / housekeeping
- `.codegraph/` is untracked and **not** in `.gitignore`. It is a local index artifact unrelated to Phase 1.3 — do not let it ride along in the expand commit; consider gitignoring it separately.

## Recommendations

1. (Optional, before commit) Add one negative test asserting `expand` with an empty/whitespace query returns `bad_input` in both one-shot and session modes, to lock the validation branch.
2. (Optional) Add a short comment in `expand.rs` documenting the phrase-contiguity matching semantics and the placeholder review metadata lifecycle.
3. (Housekeeping) Add `.codegraph/` to `.gitignore` so it does not get committed with this change.
4. None of the above block landing; the plan's follow-up (wire `expand` into search-time `expanded_terms` ranking) is correctly tracked as remaining.

Verdict: GO
