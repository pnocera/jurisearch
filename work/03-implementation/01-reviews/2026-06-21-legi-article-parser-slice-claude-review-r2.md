All prior blockers are resolved, tests pass, clippy is clean, and the dependency cleanups landed. I have what I need to write the review.

VERDICT: GO

# Re-review (r2) — First LEGI ARTICLE parser slice (`3d8997d`)

Both prior blockers are genuinely fixed, covered by a new regression test, and the verification (`cargo test -p jurisearch-ingest`, `cargo clippy -p jurisearch-ingest --all-targets -- -D warnings`) reproduces green locally. This is a solid first 0.5 slice.

## Prior blockers — confirmed fixed

1. **XML entity references no longer dropped** — `crates/jurisearch-ingest/src/legi/mod.rs:218-221, 450-477`. The parse loop now has an `Event::GeneralRef` arm routed through `resolve_reference`, which maps the five predefined entities (`amp`/`lt`/`gt`/`quot`/`apos`) and falls back to `BytesRef::resolve_char_ref()` for numeric refs. Decimal `&#233;` → `é` verified by the new test; resolution happens before `append_xml_content`, so no separator is injected around the entity.
2. **Inline content no longer newline-fragmented** — `mod.rs:433-448`. `append_xml_content` accumulates adjacent `Text`/`GeneralRef` fragments into one buffer with whitespace collapsed to single spaces, replacing the old `\n`-join. Traced `<p>Droit &amp; obligations &lt;ref&gt; caf&#233; <i>inline</i> suite</p>` → `"Droit & obligations <ref> café inline suite"`, which matches the new `preserves_entities_and_inline_text_continuity` test asserting both entity fidelity and inline continuity (no `"Droit  obligations"`, no `"inline\nsuite"`).

## Prior non-blocking suggestions — addressed

- `nota` extraction removed from `RawArticle`/`into_document` (no dead field).
- `validate_date` is now calendar-aware via `days_in_month`/`is_leap_year` (`mod.rs:385-425`); `2021-02-30`, `2021-04-31`, month `00`/`13`, day `00` all reject correctly.
- `sha2` unified to `0.11` (single entry in `Cargo.lock:1136`) with manual hex formatting; `quick-xml`'s unused `serialize` feature dropped from root `Cargo.toml`.
- Payload-hash-over-raw-bytes correctly deferred to the plan's remaining-work list (`IMPLEMENTATION_PLAN.md`, "compute source payload hashes from raw archive-member bytes").

## Suggestions / Recommendations (non-blocking; may be applied without re-review)

- **Unused dev-dependency.** `serde_json.workspace = true` in `crates/jurisearch-ingest/Cargo.toml:19` is not referenced by any source or test (clippy won't flag unused dev-deps). Drop it or use it when canonical-JSON assertions land.
- **Block boundaries flatten to a single space.** `append_xml_content` collapses all whitespace, so multiple `<p>`/list blocks within `CONTENU` merge into space-separated text with no paragraph separation. Acceptable per the prior "minimum bar," but worth a structural separator (e.g. `\n` on block-end) when the deferred "emit structural chunks" work begins.
- **Unknown named entities hard-fail.** `resolve_reference`'s `_` arm returns `LegiParseError::Xml` for any non-predefined named ref (e.g. `&nbsp;`) since `resolve_char_ref` only resolves numeric. Fail-loud is the right default over silent drop, but validate this against the real LEGI XML subset (already on the remaining-0.5 list) to confirm such entities don't appear and abort otherwise-valid articles.
- **`source_payload_hash` still over the decoded `&str`** (`mod.rs:311`, `sha256_hex(xml.as_bytes())`). Tracked in the plan note; just confirm it's switched to raw archive-member bytes — not re-derived from the decoded string — when the archive reader is wired.
- **Plan "Done" wording** says it parses into a canonical `Document`; the type is `CanonicalDocument` and persistence/DB mapping remains unwired. Cosmetic; the remaining-work enumeration is otherwise honest.

The remaining-0.5 enumeration (real-XML subset, raw-bytes hashing, `SECTION_TA`/`TEXTELR` decision, publisher-link edges, structural chunks, DTD re-verification) does not overclaim. Good to merge as the first slice.
