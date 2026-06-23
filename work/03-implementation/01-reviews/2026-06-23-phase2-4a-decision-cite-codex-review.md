# Codex review — Phase 2.4-A: cite resolves decision identifiers

Review target: `a253739` on `main`, diff vs `5cd785d`.

## BLOCKER

### Decision document IDs can still classify as `stale_version` under `--as-of`

- Location: `crates/jurisearch-cli/src/main.rs:8050-8060`, `crates/jurisearch-cli/src/main.rs:8241-8251`
- Issue: `parse_citation_target` routes `cass:`, `capp:`, `inca:`, and `jade:` decision document IDs into the generic `ParsedCitationTarget::DocumentId` variant. `classify_citation_state` then handles all `DocumentId` values with the existing LEGI version-validity rule: if `--as-of` is present, it requires `candidate_valid_on(candidate, requested_as_of)` before returning `Exact`; otherwise it returns `StaleVersion`.
- Why this violates the Phase 2.4-A constraint: decision projection stores `valid_from = decision_date` and `valid_to = NULL` as an indexing convenience (`crates/jurisearch-storage/src/projection.rs:281-284`, `crates/jurisearch-storage/src/projection.rs:329-330`). For an existing decision such as `cass:JURITEXT000051824029` dated `2025-06-04`, `jurisearch cite cass:JURITEXT000051824029 --as-of 2024-01-01` will find the decision but classify it as `stale_version`. The review instructions explicitly require decisions to be existence-based and never `stale_version`.
- Concrete fix: distinguish decision document IDs from LEGI document IDs before classification. For example, add a dedicated parsed variant for decision document IDs or add enough kind/source information to the `DocumentId` branch so `cass|capp|inca|jade` document IDs use raw match-count decision semantics. Add a regression test covering `cass:JURITEXT... --as-of` before `decision_date` and asserting `state == "exact"` and strict succeeds.

## WARN

### `--online` still sends decision identifiers to the Légifrance search probe

- Location: `crates/jurisearch-cli/src/main.rs:2963-2971`, `crates/jurisearch-cli/src/main.rs:8353-8371`, `crates/jurisearch-official-api/src/lib.rs:261-266`
- Issue: every non-malformed cite target, including `DecisionSourceUid`, `DecisionEcli`, `DecisionPourvoi`, and decision `DocumentId`, calls `apply_online_citation_confirmation`, which posts the raw query to `/dila/legifrance/lf-engine-app/search`. The response note still says the online confirmation is a Légifrance search and does not distinguish statutory from jurisprudence identifiers.
- Impact: local decision citation state remains based on the offline index, but `--online` can now perform an irrelevant or misleading corroboration request for a decision identifier. For missing decision targets, `cite_payload` also prelabels the state as `source_unavailable` whenever `--online` is set and local matches are empty, even though the offline decision lookup itself was simply `not_found`.
- Concrete fix: either make `--online` an explicit no-op for decision targets with a clear note, or add a decision-aware official probe before marking online state as checked/source-unavailable. Cover at least one decision target with `--online` in tests or document the intentional no-op behavior.

## NIT

### `parse_pourvoi` accepts right groups of four or six digits, but the comment documents only five

- Location: `crates/jurisearch-cli/src/main.rs:8284-8295`
- Issue: the doc comment describes `NN-NNNNN` / `NN-NN.NNN`, while the implementation accepts `(4..=6)` right-side digits.
- Concrete fix: either update the comment/test examples to reflect accepted four- and six-digit forms, or tighten the parser if those shapes are not intended.

## Checks Performed

- Reviewed `crates/jurisearch-storage/src/citation.rs` decision lookup SQL. User-derived `source_uid`, uppercased `ecli`, and normalized `pourvoi` all pass through `sql_string_literal`. `DecisionSourceUid` filters `d.kind = 'decision'`; `DecisionEcli` compares `upper(d.canonical_json->>'ecli')`; `DecisionPourvoi` strips dots/spaces on both sides and coalesces missing `case_numbers` to `[]`.
- Reviewed parser precedence in `parse_citation_target`. LEGI `legi:`/`LEGIARTI`/`LEGITEXT`/`LEGISCTA` paths still precede decision UID/ECLI/pourvoi detection. ECLI is not caught by source-UID extraction because it lacks a `JURITEXT`/`CETATEXT` substring. Free-text article parsing precedes pourvoi parsing.
- Reviewed `parse_pourvoi` precision. It rejects short forms, non-digit shapes, and date-like `2024-01-01`; a statutory `article ...` input routes before pourvoi.
- Reviewed LEGI state-classification arms against the parent commit. The existing `DocumentId`, `FreeTextArticle`, and LEGI UID/NOR classification logic is unchanged, but that unchanged `DocumentId` logic is the source of the decision-document-ID blocker above.
- Reviewed `crates/jurisearch-cli/tests/cli_contract.rs::cite_resolves_decision_identifiers` and parser unit tests. They cover UID/ECLI/pourvoi/doc-id happy paths and missing UID, but do not cover `--as-of` or `--online` for decision identifiers.
- Ran `git diff --check 5cd785d..a253739 -- crates/jurisearch-storage/src/citation.rs crates/jurisearch-cli/src/main.rs crates/jurisearch-cli/tests/cli_contract.rs`; no whitespace errors were reported.

VERDICT: FIXES_REQUIRED
