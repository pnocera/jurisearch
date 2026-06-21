# Claude Review: LEGI Metadata Roots

Verdict: FIXES_REQUIRED

Scope reviewed: commit `e2c63a9` "Parse LEGI metadata roots" against `IMPLEMENTATION_PLAN.md`
Phase 1.1 (full LEGI canonicalization) and Phase 1.0 (ingest-accounting contracts).
Working tree is clean except untracked `.codegraph/` (ignored, not part of the change), so the
live repo state equals the commit. Verified parser paths, ID/date validation, provenance/payload
hashing, CLI accounting semantics, and parser-version compatibility against the real corpus at
`/home/pierre/Apps/juridocs/opendata/LEGI` and the JORF-family DTDs at `/home/pierre/Apps/juridocs/DTD`.

The slice is well-built and overwhelmingly correct on real data: ID validation, sentinel date
normalization, payload-hash/provenance handling, parser-version compatibility, and the separation
of `parsed_metadata_roots` from `unsupported_roots` all hold up across the full corpus. One
real-data robustness defect with an accounting consequence, plus its missing test coverage, is the
reason for FIXES_REQUIRED. It is a small, localized fix.

## Findings

- **Major — real-data coverage / accounting gap.** `crates/jurisearch-ingest/src/legi/mod.rs:929`
  (`RawTextVersion::into_text_version`) makes `META_COMMUN/NATURE` a hard-required field for
  `TEXTE_VERSION`. Across the full global archive (144,551 `TEXTE_VERSION` members), **2 in-force
  (`<ETAT>VIGUEUR</ETAT>`) texts carry an empty `<NATURE/>`** and are otherwise fully valid (valid
  `LEGITEXT` id, `TITRE`, `DATE_DEBUT`, `DATE_FIN`):
  - `.../TNC_en_vigueur/.../JORFTEXT000000337687/texte/version/LEGITEXT000049371154.xml`
    ("Arrêté du 12 avril 1956")
  - `.../TNC_en_vigueur/.../JORFTEXT000024232917/texte/version/LEGITEXT000024235014.xml`
    ("Annexe au décret n° 2011-573 du 24 mai 2011")

  Impact: under this parser those 2 members fall into the `Err` arm of
  `process_legi_archive_member` (`crates/jurisearch-cli/src/main.rs:889`) as
  `MissingRequiredField` → recorded as `IngestMemberStatus::Failed`, written to `ingest_error`
  (`validation_error` / `validation_missing_required_field`), and quarantined when a quarantine dir
  is set. Because `run_status = Completed` only when `failed_members == 0`
  (`crates/jurisearch-cli/src/main.rs:705`), any such member contributes to a `run_status:
  "failed"` on a full-corpus run. This is a **regression**: before this commit `TEXTE_VERSION` was
  an `UnsupportedRoot` (clean `Skipped`, no error, no quarantine). It also contradicts the
  established tolerance pattern of the article slice, which makes the analogous metadata field
  optional (`etat = optional_non_empty(...)` → `absent`, `mod.rs:860`), and the plan's own
  guidance to "re-verify required fields against the current official DTDs before making parser
  validation authoritative" (Plan §1.1). The JORF DTD does not even mandate non-empty NATURE
  semantics for these old TNC texts.

  Required fix (pick one, and cover it with a test):
  - Preferred: make `nature` optional for `TEXTE_VERSION` with an `absent` fallback, mirroring
    article `etat`. The only forced consumer is the canonical-version string at `mod.rs:959`
    (`format!("legi_text_version:v1:nature={nature}")`), which can use
    `nature.as_deref().unwrap_or("absent")` exactly as the article path does at `mod.rs:903-906`.
  - Or: consciously decide empty-`NATURE` texts are quarantine-worthy failures, but then add a test
    that asserts that behavior and record the decision in the plan, so the clean-skip→failed
    regression is intentional and visible rather than silent.

- **Major — test gap.** No test exercises the real-data empty-required-field variation for any
  metadata root. The three new unit tests (`parses_text_version_metadata_root`,
  `parses_section_ta_metadata_root_with_context`, `parses_textelr_metadata_root_with_date_hint`)
  use only well-formed inputs, and the ignored real-archive smoke (`legi_archive_subset.rs`) visits
  a bounded sample that does not reach the 2 rare files. Whatever the intended behavior for the
  finding above, it must be pinned by a test (e.g. a `TEXTE_VERSION` fixture with `<NATURE/>`)
  before this is real-corpus-trustworthy. This is the gating gap alongside the first finding.

## Suggestions

- **`hierarchy_path` accumulates multiple/duplicate text-title versions.**
  `crates/jurisearch-ingest/src/legi/mod.rs:777` (`assign_section_text`) pushes every
  `CONTEXTE/TEXTE/TITRE_TXT` into `hierarchy_path`. Real `SECTION_TA` members frequently carry more
  than one `TITRE_TXT` (in the delta sample: 135/676 had 2, 2 had 4), one per text version, often
  with identical text — so ~20% of sections get duplicated/version-redundant entries rather than a
  clean structural ancestry. Not projected in this slice, and Plan §1.1 explicitly defers hierarchy
  assembly, so non-blocking — but de-duplicate (or take only the most-recent `TITRE_TXT`) before
  this feeds real `hierarchy_path` assembly.

- **`SECTION_TA` validity is taken from the parent text's `TITRE_TXT@debut/@fin`, not the section's
  own validity.** `mod.rs:977-979` requires `valid_from` from `TITRE_TXT@debut`. The section's true
  validity lives in the parent `TEXTELR`'s `<LIEN_SECTION_TA debut/fin>`, not in the context text's
  version dates. The "last `TITRE_TXT` wins" overwrite (`mod.rs:788`) is also order-dependent.
  Empirically this is benign today — in 676 sampled sections the last `TITRE_TXT` debut was never
  the `2999` sentinel, so `valid_from` lands on a sensible non-sentinel date — but it is a heuristic
  proxy. Fine as a date anchor for non-projected accounting; revisit when assembling authoritative
  section temporal spans.

- **`TEXTE_VERSION` `canonical_version` embeds per-record data** (`mod.rs:959`,
  `legi_text_version:v1:nature={nature}`), unlike the constant strings for `SECTION_TA`
  (`legi_section_ta:v1`) and `TEXTELR` (`legi_textelr:v1`). Mixing record nature into a version
  identifier is inconsistent and makes the field less useful as a schema/parser-version marker. The
  article path does the same thing (`mod.rs:903`), so this is a pre-existing style choice, not new
  — flagging for consistency only.

- **Pre-existing, related real-data context (not this commit).** The article parser requires
  `META_ARTICLE/NUM` (`mod.rs:861`); in a 60,000-file article sample, 32 real articles carry an
  empty `<NUM/>` and would already fail. Combined with the NATURE finding, this is a recurring
  "DTD-required but sometimes-empty in real LEGI data" pattern. Worth a single, deliberate tolerance
  policy (which fields are hard-required vs. defaulted) so a full-corpus run's `failed`/quarantine
  set reflects genuinely broken payloads rather than valid texts with sparse DILA metadata.

## Verification Notes

What I inspected:
- Full commit diff for `legi/mod.rs`, `cli/src/main.rs`, `cli_contract.rs`, `legi_archive_subset.rs`,
  and both `legi_canonical_retrieval.rs` tests; confirmed the storage-test change is a non-substantive
  match-arm addition and the ingest-test list matches the stat.
- Parser internals: `parse_text_version` / `parse_section_ta` / `parse_text_struct`, their
  `assign_*` path routing, and the `into_*` validators; plus shared helpers `validate_id`
  (prefix + 12 digits), `validate_date` (calendar-correct incl. leap years), `normalize_end_date`
  (`2999-01-01` / `2999-12-31` → `None`), `required`, and `optional_non_empty`.
- CLI accounting flow `process_legi_archive_member`: the new metadata-root arm records `Skipped`
  members with `source_entity = source_uid()` (root-name fallback) and a `date_anchor`, increments
  `parsed_metadata_members` + `parsed_metadata_roots`, and keeps truly unsupported roots in
  `unsupported_roots`; `run_status` derivation; resume actions (skip-compatible / retry /
  blocked-incompatible). Parser-version bump `legi_article_parser:v1` →
  `legi_article_metadata_parser:v2` correctly forces blocked-incompatible reprocess on a v1-built
  index, matching Plan §1.0 ("block blind recovery on parser/schema/code mismatch").

Real-data checks I ran (`/home/pierre/Apps/juridocs/opendata/LEGI`):
- Extracted delta `LEGI_20250715-205701.tar.gz` and used the already-extracted global
  `Freemium_legi_global_20250713-140000/` (14 GB) for corpus-wide scans.
- Confirmed real LEGI structures match the parser's paths: `TEXTE_VERSION`
  `META/META_COMMUN/{ID,NATURE}` + `META/META_SPEC/META_TEXTE_VERSION/{TITRE,TITREFULL,ETAT,
  DATE_DEBUT,DATE_FIN}`; `SECTION_TA` `ID`/`TITRE_TA`/`CONTEXTE/TEXTE@cid`/`TITRE_TXT@debut|fin`;
  `TEXTELR` `META_TEXTE_CHRONICLE/{CID,NUM,NOR,DATE_PUBLI,DATE_TEXTE}` + `LIEN_*@debut` hint.
- ID-prefix validation holds corpus-wide: 144,551/144,551 `TEXTE_VERSION` `<ID>` = `LEGITEXT`;
  236,589/236,589 `SECTION_TA` = `LEGISCTA`; 144,551/144,551 `TEXTELR` = `LEGITEXT`.
- Empty-required-field scan (full corpus): `TEXTE_VERSION` empty `ETAT`=0, `DATE_DEBUT`=0,
  `DATE_FIN`=0, `TITRE`=0, **`NATURE`=2** (the finding). `SECTION_TA` missing `<TITRE_TXT>`=0,
  empty `<TITRE_TA/>`=0. `TEXTELR` requires no per-record date, robust to empty `DATE_PUBLI/TEXTE`.
- Member size vs. 16 MiB default cap (`DEFAULT_MEMBER_BYTE_LIMIT`): largest real `TEXTELR` ≈ 99 KB,
  largest `SECTION_TA` ≈ 68 KB — no cap interaction for metadata roots.
- Sampled the article path for context: 0 empty `NATURE`, 0 empty `TYPE`, 32 empty `NUM` per 60k
  files (pre-existing).

Reproduced-by-reading (did not re-run) the pre-review verification set; the new `cli_contract`
assertions (`parsed_metadata_members == 1`, `parsed_metadata_roots["SECTION_TA"] == 1`,
`unsupported_roots` empty, `source_entity == LEGISCTA000006089696`) are consistent with the parser
and accounting code. The blocking items above are about real-corpus members the existing tests do
not reach, not about the committed tests failing.
