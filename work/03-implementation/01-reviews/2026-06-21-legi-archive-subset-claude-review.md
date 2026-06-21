# Claude Review: LEGI archive subset smoke

Verdict: GO

Reviewed commit `9a7f1dd` (Add LEGI archive subset smoke) against parent `764d843`.

## Findings

- **Low / plan accuracy — `work/03-implementation/IMPLEMENTATION_PLAN.md:342`.** The status note claims "the real archive smoke and `DTD/jorf/jorf_article.dtd` showed `META_ARTICLE/ETAT` can be absent or empty." The smoke part is not supported by the sample: the first 25 ARTICLE members in tar order (under `.../TNC_non_vigueur/JORF/TEXT/.../article/LEGI/ARTI/...`, e.g. `LEGIARTI000006850357/359/361`) all carry `<ETAT>ABROGE</ETAT>` inside `META_ARTICLE`. The only authoritative justification I could verify is the JORF article DTD (`DTD/jorf/jorf_article.dtd:30`: `META_ARTICLE (NUM, MCS_ART, DATE_DEBUT, DATE_FIN, TYPE)` — no `ETAT`), which describes the JORF fond, not the LEGI-flavoured articles currently parsed. Recommend rewording so the optional-`ETAT` decision is attributed to the JORF DTD (and noting the LEGI sample still carried `ETAT`). Non-blocking: making a required field optional is a safe relaxation — it can only accept more, never reject previously-valid records — and `source_status` is still preserved when present. No code correctness issue.

- No correctness bugs or ingestion-contract regressions found in the code.

## Suggestions

- `crates/jurisearch-ingest/tests/legi_archive_subset.rs:13` — the hardcoded `DEFAULT_LEGI_ARCHIVE` is machine-specific (`/home/pierre/...`). It is overridable via `JURISEARCH_LEGI_ARCHIVE` and skips gracefully when the file is missing, so it is not fragile, but documenting the env var (in the test module header or the plan) would let other machines run `--ignored` without editing source.
- `legi_archive_subset.rs:110` re-implements `sha256_hex` because the crate copy is private. Consider a small `pub`/`pub(crate)` hashing helper to keep the two implementations from drifting (the `sha256:` prefix and lowercase hex must stay byte-identical for the assertion to mean anything).
- `legi_archive_subset.rs:91` asserts `unsupported_roots.contains("TEXTELR")`, which depends on `TEXTELR` appearing within the first 25 articles' worth of members in tar order. Fine for the current archive and `#[ignore]`-gated (never runs in normal CI), but the coupling to archive ordering is worth a comment.
- Consider unit tests for (a) an empty `<ETAT></ETAT>` element vs a fully absent element to lock in the `optional_non_empty` trim path, and (b) `SourceProvenance::from_archive_member`'s `archive_name` fallback when the path has no file name.

## Verification Notes

- `git show 9a7f1dd` reviewed in full against parent `764d843`.
- `cargo test -p jurisearch-ingest`: 13 lib tests + 3 contract tests pass; the real-archive smoke is correctly reported as `ignored`.
- `cargo clippy -p jurisearch-ingest --tests`: clean, no warnings.
- `cargo test -p jurisearch-ingest --test legi_archive_subset -- --ignored --nocapture`: parsed 25 ARTICLE members after visiting 27 XML members; unsupported roots `{TEXTELR, TEXTE_VERSION}` — matches the plan's claimed numbers exactly. `parse_errors` empty confirms no non-predefined named-entity failures in the sample.
- **Raw member-byte hash (contract):** `SourceProvenance::from_archive_member` sets `payload_hash = Some(sha256_hex(member.bytes))` (`legi/mod.rs:31`); `RawArticle::into_document` prefers the provenance hash and only falls back to hashing the XML string (`legi/mod.rs:317-319`). On the archive path the hash is therefore the raw member bytes — asserted by both `parse_member_uses_raw_archive_member_hash_and_provenance` and the smoke. (For valid UTF-8, `xml.as_bytes()` equals `member.bytes`, so even the fallback is equivalent; `parse_legi_member` rejects non-UTF-8 before reaching `into_document`.)
- **Stop-capable reader preserves behavior:** `for_each_xml_member` now delegates to `for_each_xml_member_until` with a `Continue`-only closure (`reader.rs:55-58`). `visited` is incremented before the `Stop` check, so a stopped member is still counted and the `Ok(visited)` return semantics are unchanged; the pre-existing `streaming_reader_visits_xml_members_and_enforces_limits` still passes, and the new `streaming_reader_can_stop_after_a_bounded_sample` confirms early-stop counts the boundary member.
- **JORF DTD:** `DTD/jorf/jorf_article.dtd:30` defines `META_ARTICLE` without `ETAT`, confirming `ETAT` is legitimately absent for JORF-flavoured article members (justifies optional handling at the DTD level).
- **LEGI sample contradiction:** extracted `LEGIARTI000006850357/359/361.xml` — all contain `<ETAT>ABROGE</ETAT>`; the LEGI smoke sample does not itself demonstrate ETAT absence (basis for the Finding above).
- **Scope:** `git grep` confirms `parse_legi_member` / `for_each_xml_member_until` / `ArchiveVisit` have no production callers yet (tests only), consistent with the Phase 0.5 spike scope; `source_status` was already `Option<String>` on `CanonicalDocument` and `validate()` does not require it, so no downstream validation regression.
