# LEGI Phase 0.5 Root and DTD Audit

Date: 2026-06-21

Real-data roots:

- Official local archive source: `/home/pierre/Apps/juridocs/opendata`
- LEGI archive used by the Phase 0.5 smoke: `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz`
- DTD reference set: `/home/pierre/Apps/juridocs/DTD`

## Phase 0.5 Root Scope

Phase 0.5 is an `ARTICLE` canonicalization spike. It emits:

- canonical article documents;
- one structural article chunk per article version;
- publisher graph-edge candidates extracted from article links;
- unsupported-root classifications for non-article XML roots.

The following roots are intentionally deferred from Phase 0.5 canonical document output:

| Root | Local DTD evidence | Phase 0.5 decision |
|---|---|---|
| `SECTION_TA` | `DTD/jorf/jorf_section_ta.dtd`, `DTD/kali/kali_section_ta.dtd` define section table-of-contents roots with `STRUCTURE_TA (LIEN_SECTION_TA|LIEN_ART)*` plus context. | Defer to Phase 1 hierarchy assembly. It is structure metadata, not an article body document. |
| `TEXTELR` | `DTD/jorf/jorf_texte_struct.dtd` defines text structure with `STRUCT (LIEN_SECTION_TA|LIEN_ART)*`. | Defer to Phase 1 hierarchy/version assembly. It is structural metadata, not an article body document. |
| `TEXTEKALI` | `DTD/kali/kali_texte_struct.dtd` defines KALI text structure with `STRUCT (LIEN_SECTION_TA|LIEN_ART)*`. | Defer to Phase 1 hierarchy/version assembly. It is structural metadata, not an article body document. |
| `TEXTE_VERSION` | `DTD/jorf/jorf_texte_version.dtd` and `DTD/kali/kali_texte_version.dtd` define text-version metadata roots. | Defer to Phase 1 text-level canonicalization. It is not an article-version body. |

This matches the current parser contract: non-`ARTICLE` roots return `ParsedLegiXml::UnsupportedRoot { root }` and are not reported as successful inserts.

## Article DTD Required-Field Check

The local DTD set does not contain a top-level `DTD/legi` profile. LEGI archive members are organized by fond/source profile; the first real sample is JORF-flavoured.

Observed article DTD profiles:

- `DTD/jorf/jorf_article.dtd`
  - `@root ARTICLE`
  - `ARTICLE (META, CONTEXTE, VERSIONS, SM, BLOC_TEXTUEL, LIENS?)`
  - `META_ARTICLE (NUM, MCS_ART, DATE_DEBUT, DATE_FIN, TYPE)`
  - `ETAT` is not present in the JORF article DTD.
- `DTD/kali/kali_article.dtd`
  - `@root ARTICLE`
  - `ARTICLE (META, CONTEXTE, VERSIONS, BLOC_TEXTUEL, NOTA?, CONDITION_DIFFERE?, LIENS)`
  - `META_ARTICLE (NUM, TITRE?, ETAT, CALIPSOS, HISTORIQUE, DATE_DEBUT, DATE_FIN, DATE_DEB_EXT, DATE_FIN_EXT, TYPE)`
  - `ETAT` is required by KALI article DTD.

Phase 0.5 parser validation currently requires the cross-profile fields needed to build an article canonical record:

- `META_COMMUN/ID`
- `META_COMMUN/NATURE`
- `META_ARTICLE/NUM`
- `META_ARTICLE/TYPE`
- `META_ARTICLE/DATE_DEBUT`
- `META_ARTICLE/DATE_FIN`
- `BLOC_TEXTUEL/CONTENU`

Phase 0.5 treats `META_ARTICLE/ETAT` as optional because it is absent from the JORF article DTD and can be empty in JORF-flavoured real data. When present, it is preserved as `source_status`; when absent or empty, canonical versions record `etat=absent`.

Fields such as KALI `CALIPSOS`, `HISTORIQUE`, `DATE_DEB_EXT`, and `DATE_FIN_EXT` are not yet canonical output fields in the Phase 0.5 article spike. Full profile-specific field retention belongs in Phase 1 full LEGI canonicalization, alongside `SECTION_TA`/text-structure assembly.
