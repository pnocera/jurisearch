VERDICT: FIXES_REQUIRED

# Review — First LEGI ARTICLE parser slice (`7f47873`)

The slice is well-structured and the design intent is correct: official-XML-only stance, typed `LEGIARTI` validation, required-field checks, `DATE_FIN` sentinel normalization with `valid_to_raw` preservation, explicit `UnsupportedRoot` classification, and a second canonical `validate()` pass. The struct maps cleanly onto the `documents` table (`crates/jurisearch-storage/src/migrations.rs:27`), and the plan honestly defers the bulk of 0.5. However, the body-text extraction silently deletes characters, which is a correctness defect in code claimed "Done", so this cannot pass as-is.

## Blocking fixes

### 1. XML entity references are silently dropped from body/nota text — `crates/jurisearch-ingest/src/legi/mod.rs:194-222`

In quick-xml 0.39, character and general entity references (`&amp;`, `&lt;`, `&gt;`, `&apos;`, `&quot;`, and numeric refs `&#233;` / `&#xE9;`) are **not** part of `Event::Text` — they are emitted as separate `Event::GeneralRef` events. The parse loop has no arm for `GeneralRef`; it falls into `Ok(_) => {}` (line 215) and is discarded. The result is not "entity left escaped" — the character it represents is **removed entirely**.

I verified this against quick-xml 0.39.4 with the project's exact pattern (`trim_text(false)`, `Event::Text` + `.decode()`, all else ignored):

```
input:  <CONTENU><p>Droit &amp; obligations &lt;ref&gt; caf&#233;</p></CONTENU>
output: "Droit  obligations ref caf"
        (expected: "Droit & obligations <ref> café")
```

Impact: every LEGI article body containing an escaped `&` (common in legal text and mandatory in XML), or any numeric character reference, will be ingested with characters missing. This corrupts the primary searchable/canonical field with no error raised. The synthetic `article_fixture()` contains no entities, so the passing tests give false confidence — the existing tests would not catch this regression.

Fix direction: handle `Event::GeneralRef(r)` in the loop and resolve it before routing through `assign_article_text` — predefined entities (`amp`/`lt`/`gt`/`quot`/`apos`) plus `BytesRef::resolve_char_ref()` for numeric refs (see `quick-xml-0.39.4/src/events/mod.rs:1657`). Note the resolved fragment must be concatenated to the adjacent text **without** a separator; routing it through the current `append_text` helper would inject a spurious `\n` around every entity (see related concern #2). Add a fixture/test whose body contains `&amp;`, `&#233;`, and `&lt;…&gt;` to lock the behavior.

### 2. `append_text` fragments inline content onto separate lines — `crates/jurisearch-ingest/src/legi/mod.rs:249-250, 409-414`

Every `CONTENU` text node is joined with `\n` (`append_text`). For paragraph-level `<p>` blocks this is acceptable, but for inline markup within a paragraph (`<p>foo <i>bar</i> baz</p>`) the text events `"foo"`, `"bar"`, `"baz"` are emitted separately and become `"foo\nbar\nbaz"`, splitting a single sentence across three lines and dropping the inter-word spaces. Combined with #1, body text is not faithfully reconstructed. This should be resolved together with the entity fix (e.g. accumulate a run of adjacent `Text`/`GeneralRef` events into one buffer and only break on block boundaries) so that "Done: parses ARTICLE XML into canonical body" is actually true. If a full inline-vs-block model is out of scope for this slice, at minimum preserve adjacent-fragment continuity rather than newline-joining everything.

## Suggestions / Recommendations (non-blocking; may be applied without re-review)

- **`source_payload_hash` is computed over the decoded `&str`, not raw member bytes** (`mod.rs:302`). The input has already been charset-decoded/normalized, so the hash is not anchored to the on-disk archive bytes. When this is wired to the `archive` reader (which yields bytes), compute the SHA-256 over the raw member bytes for reproducible provenance integrity, and pass the digest in rather than re-deriving it from the string. Worth a note in the plan's remaining-0.5 list.
- **`nota` is extracted but never used** (`mod.rs:144, 252`). `RawArticle.nota` is collected and then dropped in `into_document`. Either carry it into the canonical record (e.g. `canonical_json`) or remove the extraction to avoid dead work and reviewer confusion.
- **`validate_date` is shape-only** (`mod.rs:376-401`). It accepts impossible calendar dates (`2021-02-30`, `2021-04-31`) since day is checked as `1..=31` regardless of month. Fine for well-formed LEGI data, but consider real calendar validation (or a typed date) before 0.5 is marked complete.
- **`sha2 = "0.10.9"` duplicates the dependency tree.** `sha2 0.11.0` is already resolved in `Cargo.lock` (via `postgres`), so pinning 0.10.9 pulls in a parallel stack (`block-buffer 0.10`, `crypto-common 0.1`, `digest 0.10`, `cpufeatures 0.2`, `generic-array`, `version_check`). Depending on `sha2 = "0.11"` would dedupe; the `Sha256::digest` API used here is unchanged.
- **`quick-xml`'s `serialize` feature is enabled but unused.** The parser uses only the event reader API (no `quick_xml::de`/`se`). Consider dropping `features = ["serialize"]` from the root `Cargo.toml`.
- **Two passes over the XML.** `detect_root` and `parse_article` each construct a `Reader` and scan the input. Acceptable for a slice; could be folded into a single pass later.
- **Plan wording.** `IMPLEMENTATION_PLAN.md:341` says it parses into a canonical `Document`; the type is `CanonicalDocument` and it carries fields with no `documents` column (`source_status`/`source_nature`/`source_article_type`/`hierarchy_path`/`canonical_version`/`source_archive`/`source_member_path`) that presumably belong in `canonical_json`. The canonical→DB mapping is still unestablished — fine to leave in the remaining-work list, just don't read the current "Done" bullet as implying persistence is wired.

The plan's remaining-0.5 enumeration (real-XML subset, `SECTION_TA`/`TEXTELR` or deferral rationale, publisher-link graph edges, structural chunks, DTD re-verification) correctly avoids overclaiming completion. Once the body-extraction defects above are fixed and covered by an entity-bearing fixture, this is a solid first slice.
