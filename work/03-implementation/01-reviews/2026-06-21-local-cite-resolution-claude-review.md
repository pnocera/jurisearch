# Claude Review — Local Statutory Citation Verification (Phase 1.4)

Reviewed: uncommitted Phase 1.4 local-first `cite` slice — new storage citation
lookup API (`jurisearch-storage/src/citation.rs`, `storage/src/lib.rs`), CLI + JSONL
session `cite` payload, citation parser/state classifier (`cli/src/main.rs`), strict
handling, schema/contract updates (`core/src/schema.rs`, `core/src/contract.rs`,
`core/src/lib.rs`), the plan note (`IMPLEMENTATION_PLAN.md`), and CLI contract tests
(`cli/tests/cli_contract.rs`).
Reviewer: Claude (Opus 4.8), 2026-06-21.

The slice is correct, coherent, safely parameterized, and well-tested. It resolves the
documented local input classes (internal `legi:` document IDs, `LEGIARTI`/`LEGITEXT`/
`LEGISCTA` identifiers, NOR via TEXTELR, and numeric free-text articles) into explicit
citation states with validity-aware annotations, and the `cite --online` deferral is
explicitly disclosed and matches the W8 follow-up in the plan. Findings below are
limitations and nits, not blockers.

## What I verified

- **Build/tests/lints green.** `cargo check -p jurisearch-storage -p jurisearch-cli
  --tests` clean; `cargo clippy` for both crates: no warnings; the new
  `cite_resolves_local_statutory_citations_and_strict_states` integration test passes
  (Postgres present here — it self-skips otherwise, matching the existing
  `discover_pg_config` pattern); `help_schema_json_is_valid_and_lists_commands` and
  `jurisearch-core` tests pass.
- **Contract/schema consistency.** `cite` flipped `Stub`→`Implemented`;
  `CiteRequest`/`CiteResponse` added to `compiled_schema()`. The schema `input_class`
  enum and `state` enum match `ParsedCitationTarget::input_class()` /
  `citation_state_name()` exactly (7 and 6 variants). `agent_help` already documents
  exit `2` as covering "strict citation". `recursion_limit = "256"` on core is a
  legitimate fix for the enlarged `json!` schema macro expansion.
- **SQL injection safety.** Every interpolated user-derived value passes through
  `sql_string_literal` (escapes `'`). Identifiers are further constrained upstream:
  `extract_known_source_uid` only emits `LEGIxxx` + exactly 12 digits; NOR is reduced
  to ASCII-alnum; the free-text path interpolates only the parsed article number
  (alnum + `-`) and the literal `code civil`. Safe within the repo's string-built-SQL
  convention.
- **Validity logic.** `documents.valid_from/valid_to` and the metadata roots are
  `date` columns, so `::text` yields `YYYY-MM-DD` and `candidate_valid_on`'s
  lexicographic compare equals chronological order. The interval is half-open
  `[from, to)` (`as_of < valid_to`), consistent with `to_exclusive: true`. The
  historical `--as-of` test (article 88, two versions) correctly selects only the
  version live on the requested date.
- **State classification matches intent.** Pinned `document_id` → `exact` when the
  exact identifier resolves (version-pinned, so validity-as-of-today is intentionally
  not required); identifier/NOR roots → `exact`/`ambiguous`/`stale_version` by valid
  match count; free-text → `normalized`/`ambiguous`/`stale_version`; malformed →
  `not_found`. Strict failure routes through `ErrorCode::NoResults` → `ProcessExit::User`
  = exit `2`, asserted by the test.

## Findings

1. **(Medium — coverage gap) L./R./D.-prefixed article numbers with a separating
   space or dot are not parsed.** `parse_article_number` (`cli/src/main.rs`) only
   accepts the single token immediately after `article` and requires it to contain a
   digit. So `article L. 121-1` normalizes to tokens `article l 121 1` and falls
   through to `Malformed` → `not_found`. The compact form `article L121-1`
   (→ `l121-1`) does match. French statutory citations lean heavily on L./R./D.
   numbering, so the dotted/spaced form is a real-world miss. Not a blocker (the slice's
   numeric-article scope is internally consistent and tested), but it should either be
   supported or explicitly scoped out in the plan's cite section.

2. **(Low — data coupling) Free-text resolution assumes `documents.citation`/`title`
   literally embeds "article &lt;number&gt;".** `free_text_article_lookup_sql` matches
   `lower(concat_ws(' ', citation, title)) LIKE '%article <n>%'`. The test fabricates
   citations in exactly that shape (`"Code civil article 1240"`), so it does not
   exercise real LEGI-ingested article text. If ingested citations format the number
   differently (e.g. without the word "article"), free-text cites silently miss. Worth a
   follow-up test over actually-ingested LEGI rows, or a documented note on the coupling.

3. **(Low — limitation) `code_hint` disambiguation is hardcoded to `code civil`.**
   `parse_citation_target` only sets a code hint for `code civil`; any other code
   (pénal, travail, consommation, assurances, …) gets no hint, so same-numbered
   articles across codes return `ambiguous`. Reasonable for v1, but a known limitation
   worth recording rather than leaving implicit.

4. **(Low — mislabel) `looks_like_nor` over-accepts.** Any 10–20-char ASCII-alnum
   string with at least one letter and one digit not starting with `LEGI` is classified
   as `input_class: "nor"` (e.g. a decision-style string "Cass civ 1re 14 mars 2018").
   The outcome is harmless (`not_found`, no match), but the reported `input_class` is
   wrong. Tightening to the NOR shape (4 alpha / 7 digit / 1 alpha) would remove the
   mislabel.

5. **(Info — by design, scoped) `--strict --online` currently certifies on local
   evidence.** With `--online` and a local hit, top-level `state` reports the local
   resolution while `online.checked=false` plus a disclosure note are emitted; the
   `source_unavailable` override only fires for online + non-malformed + zero local
   matches. So a strict+online query passes when local resolution is `exact`/`normalized`
   even though no upstream check ran. This is exactly the disclosed W8 deferral and the
   `online` sub-object makes it explicit, so it is acceptable — just flagging that strict
   does not yet imply online confirmation.

6. **(Nit) Validity is computed twice** — once inside `classify_citation_state`
   (internal `valid_match_count`) and again in `annotate_valid_matches`. Same input,
   negligible cost; could be shared. Also `limit: 25` is hardcoded and candidate
   truncation is not disclosed in the response (classification only needs `>1`, so
   states are unaffected).

## Recommendations

- Extend article-number parsing to absorb an L./R./D. (and dotted/spaced) prefix, or
  explicitly scope numeric-only article support in the plan's cite section (addresses #1).
- Add a free-text cite test against real LEGI-ingested article rows to lock down the
  citation/title text coupling (#2).
- Generalize the code hint beyond `code civil` via a small table of known code names to
  cut false `ambiguous` results (#3).
- Optionally tighten `looks_like_nor` to the canonical NOR shape (#4).
- Optionally surface a truncation flag when the 25-candidate cap is hit (#6).

None of the above blocks merge: the implemented scope is correct, parameterized safely,
schema/contract-consistent, fully exercised by passing tests, and the online gap is
disclosed in both the response and the plan.

Verdict: GO
