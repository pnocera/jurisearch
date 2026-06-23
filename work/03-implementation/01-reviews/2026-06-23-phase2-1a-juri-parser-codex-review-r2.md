# Codex Review r2 - Phase 2.1-A JURI Parser

Reviewed HEAD `d7d7e40` against `bcd2ebf`, scoped to the r1 follow-up fixes in `crates/jurisearch-ingest/src/juri/mod.rs` and `crates/jurisearch-ingest/src/juri/tests.rs`.

## BLOCKER

None.

## WARN

None.

## NIT

None.

## Verified claims

- WARN1 / NIT1 is resolved. Body text capture and self-closing/end block boundaries now share `in_body_context()`, which requires the `BLOC_TEXTUEL/CONTENU` path. Adjacent `<P>...</P><P>...</P>` blocks get a single paragraph boundary, inline tags stay continuous, and closing `CONTENU` / `BLOC_TEXTUEL` cannot add a boundary because those names are not in `is_body_block_boundary()`. The end handler reads `stack.last()` before `stack.pop()`, so the closing block tag is still visible when deciding whether to append a boundary.
- `append_block_boundary()` trims trailing inline spaces and only appends `\n` when the buffer is non-empty and not already newline-terminated; `finish_body()` splits on those boundaries, collapses whitespace, drops empty lines, and rejoins with single newlines, so leading/trailing empty paragraphs and doubled newlines are not emitted.
- WARN2 is resolved. `CanonicalDecision::validate()` now rejects non-`heuristic` `chunking_provenance`, non-`juri_decision:v1` `canonical_version`, and chunks whose `chunk_builder_version` differs from `juri_decision_heuristic:v1`; normally parsed decisions still populate those fields with the expected values and validate.
- WARN3 is resolved. `validate_iso_date()` now checks the parsed day against `days_in_month(year, month)`, including leap-year handling, so shape-valid calendar-invalid dates like `2025-02-31` are rejected while `2024-02-29` is accepted.
- WARN4 is resolved. `JuriFamily::for_source()` maps `cass` / `capp` / `inca` to judicial and `jade` to administrative, `parse_juri_xml()` rejects source/root-family mismatches, and `CanonicalDecision::validate()` independently derives `ArchiveSource::from_token(&self.source).and_then(JuriFamily::for_source)` and compares it to `self.source_family`.
- WARN5 is resolved. `split_body()` returns `BodyPiece { text, boundary }`, packs natural paragraph chunks as `paragraph`, labels over-long paragraph fragments as `hard_split`, uses `chars().count()` consistently for projected size checks, and `hard_split()` chunks by characters rather than bytes. The flush closure uses `std::mem::take()` only after pushing non-empty current text, so it does not drop or duplicate buffered paragraphs.
- Mismatch and validation coverage was added for `parse_juri_xml(ArchiveSource::Jade, JUDI_XML)`, `parse_juri_xml(ArchiveSource::Cass, ADMIN_XML)`, `ArchiveSource::Legi`, dishonest chunking provenance, calendar-invalid dates, leap day acceptance, adjacent block boundaries, inline markup, and hard-split chunk boundaries.
- No LEGI code is changed in the reviewed diff, and the deterministic ordering properties from r1 remain intact: raw metadata stays in `BTreeMap`, summaries and publisher edges keep XML encounter order, and the r2 changes introduce no randomized or time-dependent behavior.

VERDICT: GO
