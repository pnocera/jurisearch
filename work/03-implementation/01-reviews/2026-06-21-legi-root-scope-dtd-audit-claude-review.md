# Claude Review: LEGI root scope DTD audit

Verdict: GO

Reviewed commit `f996ae4` (Record LEGI root scope audit) against parent `1c9abd0`.
Scope: `crates/jurisearch-ingest/src/legi/mod.rs` test change, new evidence file
`work/03-implementation/02-evidence/2026-06-21-legi-phase0-root-dtd-audit.md`, and
the `IMPLEMENTATION_PLAN.md` 0.5 status update.

## Findings

- None blocking.

  The root-deferral decision is correctly grounded in the local DTDs and the real
  archive layout:
  - `/home/pierre/Apps/juridocs/DTD` has `jorf/` and `kali/` profiles but **no
    `DTD/legi`** directory — the audit's "no top-level LEGI profile" claim is true.
  - Every deferred root is a real DTD `@root` and a real member type in
    `Freemium_legi_global_20250713-140000.tar.gz`: `SECTION_TA` (538,001 members,
    `jorf/kali_section_ta.dtd` → `STRUCTURE_TA (LIEN_SECTION_TA|LIEN_ART)*`),
    `TEXTELR` (286,128, `jorf_texte_struct.dtd` → `STRUCT (LIEN_SECTION_TA|LIEN_ART)*`),
    `TEXTE_VERSION` (286,128, `jorf/kali_texte_version.dtd`), and `TEXTEKALI`
    (`kali_texte_struct.dtd`, KALI-fond variant). All are structure/version metadata,
    not article bodies, so deferring them from an `ARTICLE` spike is correct.

- `SECTION_TA`/`TEXTELR`/`TEXTEKALI`/`TEXTE_VERSION` are correctly classified as
  unsupported, not falsely ingested. `parse_legi_xml` (mod.rs:289-295) routes only
  `ARTICLE` to `parse_article` and returns `ParsedLegiXml::UnsupportedRoot { root }`
  for everything else; the new `classifies_unsupported_roots` test (mod.rs:1224-1252)
  asserts all four. Test run passed: `cargo test -p jurisearch-ingest --lib
  legi::tests::classifies_unsupported_roots` → 1 passed.

- The DTD required-field audit is accurate for both article DTDs.
  - `jorf_article.dtd`: `ARTICLE (META, CONTEXTE, VERSIONS, SM, BLOC_TEXTUEL, LIENS?)`
    and `META_ARTICLE (NUM, MCS_ART, DATE_DEBUT, DATE_FIN, TYPE)` — the audit quotes
    these verbatim, and there is indeed **no `<!ELEMENT ETAT>`** in the JORF article DTD.
  - `kali_article.dtd`: `META_ARTICLE (NUM, TITRE?, ETAT, CALIPSOS, HISTORIQUE,
    DATE_DEBUT, DATE_FIN, DATE_DEB_EXT, DATE_FIN_EXT, TYPE)` — `ETAT` is required,
    as the audit states.
  - The parser's cross-profile required set (mod.rs:455-467: `META_COMMUN/ID`,
    `META_COMMUN/NATURE`, `META_ARTICLE/NUM`, `/TYPE`, `/DATE_DEBUT`, `/DATE_FIN`,
    `BLOC_TEXTUEL/CONTENU`) matches the audit list exactly, and every one of those is
    required by both article DTDs (`ID`/`NATURE` via `meta_commun.dtd`; `CONTENU` via
    `BLOC_TEXTUEL (CONTENU)`). Treating `ETAT` as optional is correct and safe: the JORF
    DTD has no such element, while real LEGI articles carry it (verified
    `<ETAT>ABROGE</ETAT>` in `LEGIARTI000006850357.xml`) and it is preserved as
    `source_status`; the empty-status case is grounded in the prior smoke evidence
    (plan line 344) and covered by `accepts_empty_status_elements_as_absent`.

- The plan marks 0.5 complete without hiding Phase 1 work. `IMPLEMENTATION_PLAN.md:349`
  explicitly defers profile-specific field retention, `SECTION_TA`/text-structure
  hierarchy assembly, and text-level canonicalization to Phase 1 before "full LEGI
  canonicalization" can be claimed. The 0.5 acceptance bar (unsupported roots
  classified, not reported as inserts — line 337) is met by the parser contract and the
  new test.

## Suggestions

- "JORF-flavoured" is slightly imprecise (carried over from plan line 344). The real
  LEGI article declares **no DOCTYPE** and contains an `ETAT` element under
  `META_ARTICLE` that the JORF article DTD does not define — i.e. the real data is a
  LEGI-fond superset, not strictly JORF-conformant. The downstream decisions are
  unaffected; consider a one-line note in the evidence file that the JORF DTD instead
  carries legal state via the `VERSION/@etat` attribute, so "ETAT absent from the JORF
  DTD" means the element, not the concept.

- The ignored real-archive smoke only exercises `TEXTELR`/`TEXTE_VERSION` (plan
  line 343). Since `SECTION_TA` is the largest deferred member type (538k files) and
  exists as a standalone root in the archive, consider extending the smoke to visit one
  real `section_ta` member and assert `UnsupportedRoot` — the unit test already covers
  the synthetic case, so this is purely defense-in-depth.

- Acceptance criterion line 337 says unsupported roots are "classified and counted."
  Classification is tested; counting is presumably aggregated at the ingest-
  orchestration layer (Phase 1). Worth confirming a count assertion lands when that
  layer is built.

## Verification Notes

- `git show --stat f996ae4` / `git show f996ae4`: confirmed the diff is the 4-root test
  expansion, the new evidence file, and the two-line plan status update.
- DTD checks under `/home/pierre/Apps/juridocs/DTD`: confirmed no `legi/` profile;
  grepped `@root` and element models for `jorf/kali_article.dtd`,
  `jorf/kali_section_ta.dtd`, `jorf/kali_texte_struct.dtd`, `jorf/kali_texte_version.dtd`,
  and `meta_commun.dtd` — all audit quotes match the live DTDs.
- Real archive `Freemium_legi_global_20250713-140000.tar.gz`: member-type distribution
  (`3,178,450 article / 538,001 section_ta / 286,128 texte/struct / 286,128
  texte/version`); extracted samples confirm real roots `<ARTICLE>` (no DOCTYPE,
  `META>META_SPEC>META_ARTICLE`, `ETAT=ABROGE`, `BLOC_TEXTUEL/CONTENU` present),
  `<TEXTELR>`, `<TEXTE_VERSION>`, `<SECTION_TA>`.
- Parser source `crates/jurisearch-ingest/src/legi/mod.rs`: confirmed `parse_legi_xml`
  routing (lines 289-295), `detect_root` (308-330), required-field extraction
  (455-467), and `ETAT`-optional handling (`source_status`/`etat=absent`, 458/488/501-504).
- `cargo test -p jurisearch-ingest --lib legi::tests::classifies_unsupported_roots`:
  compiled and passed (1 passed, 0 failed).
