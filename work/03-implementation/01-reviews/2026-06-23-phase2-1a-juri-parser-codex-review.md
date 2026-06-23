# Codex Review - Phase 2.1-A JURI Parser

Reviewed HEAD `bcd2ebf` against parent `ecc4163`, scoped to the parser layer files listed in `/tmp/codex-review-phase2-1a.md`.

## BLOCKER

None.

## WARN

1. **Body reconstruction ignores DTD-allowed block boundaries and can concatenate decision text.**

   `crates/jurisearch-ingest/src/juri/mod.rs:422-435`, `crates/jurisearch-ingest/src/juri/mod.rs:495-528`, and `crates/jurisearch-ingest/src/juri/mod.rs:857-860` only add body boundaries for self-closing `br`/`BR` events. The JURI/JADE `CONTENU` DTD allows XHTML/block content, including `P`, `p`, table/list-ish block elements, and the local `BR` element. Adjacent block elements such as `<CONTENU><P>Premier motif</P><P>Second motif</P></CONTENU>` would be accumulated as `Premier motifSecond motif` unless the source happens to include whitespace text between the tags. That silently corrupts the canonical body and then the heuristic chunks.

   Recommended fix: port or share the LEGI body helpers (`append_xml_content`, `append_block_boundary`, `is_body_block_boundary`) for JURI body accumulation. Add boundaries on `End` for `p/P` and other block tags, on `Empty` for `br/BR`, and for `BR` if it appears as start/end. Keep inline tags continuous. Add unit coverage for adjacent `<P>` blocks and inline markup inside a paragraph.

2. **`CanonicalDecision::validate()` does not enforce the decision-level bulk chunking provenance.**

   `crates/jurisearch-ingest/src/juri/mod.rs:152-195` validates each chunk's `chunking == "heuristic"`, but never checks `self.chunking_provenance`. A mutated or hand-built bulk decision with `chunking_provenance = "zone"` or `"structural"` would pass validation even though the accepted Phase 2 scope explicitly says bulk records must never satisfy official-zone/structural quality by assertion.

   Recommended fix: add a validation branch requiring `self.chunking_provenance == "heuristic"` for all DILA bulk decisions. While there, also validate `canonical_version == "juri_decision:v1"` and each chunk's `chunk_builder_version == "juri_decision_heuristic:v1"` so projection cannot accept mixed provenance metadata.

3. **Calendar-invalid `decision_date` values pass validation.**

   `crates/jurisearch-ingest/src/juri/mod.rs:988-1006` validates only shape, month `1..=12`, and day `1..=31`. Dates like `2025-02-31` and `2025-04-31` therefore pass both parser validation and `CanonicalDecision::validate()`. This is a reachable invalid canonical record state and would poison date filters or downstream projections.

   Recommended fix: reuse the LEGI-style `days_in_month` and leap-year validation from `crates/jurisearch-ingest/src/legi/mod.rs:1540-1568`, or replace the helper with a real date parser. Add tests for `2025-02-31` rejected and `2024-02-29` accepted.

4. **The parser does not reject archive-source/root-family mismatches.**

   `crates/jurisearch-ingest/src/juri/mod.rs:311-322` derives `JuriFamily` only from the XML root and accepts any jurisprudence `ArchiveSource`. `CanonicalDecision::validate()` at `crates/jurisearch-ingest/src/juri/mod.rs:160-174` then only checks that the source is some jurisprudence dataset and that the UID prefix matches the root family. As a result, `parse_juri_xml(ArchiveSource::Jade, judicial_juritext_xml, ...)` can produce and validate `document_id = "jade:JURITEXT..."`, misclassifying an official source-native record.

   Recommended fix: encode the allowed mapping (`cass/capp/inca -> TEXTE_JURI_JUDI`, `jade -> TEXTE_JURI_ADMIN`) and reject mismatches in `parse_juri_xml`. Mirror that invariant in `CanonicalDecision::validate()` so invalid records cannot bypass the parser.

5. **Hard-split chunks are reported as paragraph-boundary chunks.**

   `crates/jurisearch-ingest/src/juri/mod.rs:668-678` labels every body chunk with `boundary = "paragraph"`, including pieces emitted from the hard-split path at `crates/jurisearch-ingest/src/juri/mod.rs:731-737`. The size limit is respected, but downstream diagnostics cannot distinguish a natural paragraph pack from an emergency split, which the ADR calls out as a separate fallback-quality case.

   Recommended fix: have `split_body` return chunk text plus a boundary/provenance marker, and label over-long paragraph pieces as `hard_split` while keeping bulk `chunking`/builder provenance honest.

## NIT

1. **`<br/>` handling is guarded less tightly than text body handling.**

   `crates/jurisearch-ingest/src/juri/mod.rs:857-860` adds a break whenever the stack contains `CONTENU`, while text capture at `crates/jurisearch-ingest/src/juri/mod.rs:523-528` requires both `CONTENU` and `BLOC_TEXTUEL`. A `<br/>` under `CITATION_JP/CONTENU` will not add citation text, but it can still insert a body newline. Most trailing cases collapse away, but the guard should match the text capture contract.

   Recommended fix: require both `CONTENU` and `BLOC_TEXTUEL` in `push_break`, or pass an explicit `is_body_context` predicate shared with `assign_text`.

## Verified claims

- `quick-xml` is pinned to `0.39.2`. Its local source confirms self-closing elements are emitted as `Event::Empty`, entity references such as `&amp;` are emitted as `Event::GeneralRef`, and `BytesText::decode()` decodes text bytes without resolving references. The parser's `Event::Empty`, `Event::GeneralRef`, `resolve_char_ref`, and predefined-entity handling are aligned with those semantics.
- The `BLOC_TEXTUEL/CONTENU` guard prevents `SOMMAIRE`, `CITATION_JP/CONTENU`, and `LIEN` text from being mixed into the body; the remaining body issue is boundary handling, not wholesale bucket confusion.
- Archive filename generalization preserves the LEGI baseline/delta forms and rejects cross-source names by comparing the captured token with `ArchiveSource::{as_str,delta_prefix}`. `plan_from_paths` dispatches through `ParsedArchive::parse_path(source, ...)` generically, so the new sources follow the same planning path.
- `document_id` is built as `<source>:<source_uid>`, and publisher `LIEN` edges use `edge_source = "publisher"` with unresolved evidence preserved.
- `decision_edge_id` includes `from_document_id`, the original link index, source tag, target UID, and source text, so two same-text links at different positions do not collide except by cryptographic hash collision, and a stable re-parse yields stable edge IDs.
- `raw_metadata` is a `BTreeMap`; summaries, case numbers, and publisher edges are emitted in XML encounter order, so deterministic input produces deterministic output.

VERDICT: FIXES_REQUIRED
