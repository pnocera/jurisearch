# Additional local opendata candidates for jurisearch

Date: 2026-06-24

Scope: quick local inventory of `/mnt/nas/opendata`, checked against `work/03-implementation/IMPLEMENTATION_PLAN.md`, especially Phase 2 jurisprudence ingestion and later corpus expansion. This is not an exhaustive file-by-file audit; it identifies source families that are useful enough to prioritize for ingestion work.

## Executive recommendation

Use `/mnt/nas/opendata` as a useful upstream mirror/check source, but keep the current Phase 2 canonical staging path at `/mnt/models/opendata` unless the plan is intentionally changed. The NAS tree contains the exact mandatory DILA bulk jurisprudence families needed by Phase 2:

- `/mnt/nas/opendata/CASS`
- `/mnt/nas/opendata/INCA`
- `/mnt/nas/opendata/CAPP`
- `/mnt/nas/opendata/JADE`
- `/mnt/nas/opendata/DTD_LEGIFRANCE`

Those are the high-value ingest targets. They align directly with the plan's mandatory DILA bulk adapter for `TEXTE_JURI_JUDI` and `TEXTE_JURI_ADMIN`. Everything else should be treated as follow-on corpus expansion or evaluation material, not as a blocker for Phase 2.

## Evidence sampled

Top-level legal families present under `/mnt/nas/opendata` include `LEGI`, `CASS`, `INCA`, `CAPP`, `JADE`, `CONSTIT`, `KALI`, `JORF`, `JORFSIMPLE`, `CNIL`, `CIRCULAIRES`, and `DTD_LEGIFRANCE`.

Approximate directory sizes:

| Family | Size | Notes |
|---|---:|---|
| CASS | 252M | Cour de cassation bulk jurisprudence baseline + deltas. |
| INCA | 659M | Judicial bulk jurisprudence baseline + deltas. |
| CAPP | 280M | Cour d'appel bulk jurisprudence baseline + deltas. |
| JADE | 1.3G | Administrative jurisprudence baseline + deltas. |
| LEGI | 1.6G | Statutory corpus already central to Phase 1. |
| KALI | 205M | Collective agreements; good Phase 3 corpus candidate. |
| JORF | 2.2G | Official gazette; useful for provenance/cross-links, not first Phase 2 target. |
| JORFSIMPLE | 1.4G | Simpler JORF feed; same caveat as JORF. |
| CONSTIT | 14M | Constitutional Council decisions; good targeted expansion. |
| CNIL | 19M | Regulatory decisions/deliberations; specialist expansion. |
| CIRCULAIRES | 40G | Includes large PDF archives plus smaller XML; later expansion only. |
| DTD_LEGIFRANCE | 11M | Technical DTD bundle and migration notes. |

Archive counts observed:

| Family | `*.tar.gz` count | Baseline observed |
|---|---:|---|
| CASS | 18 | `Freemium_cass_global_20250713-140000.tar.gz` |
| INCA | 19 | `Freemium_inca_global_20250713-140000.tar.gz` |
| CAPP | 14 | `Freemium_capp_global_20250713-140000.tar.gz` |
| JADE | 109 | `Freemium_jade_global_20250713-140000.tar.gz` |
| LEGI | 145 | `Freemium_legi_global_20250713-140000.tar.gz` |
| KALI | 90 | `Freemium_kali_global_20250713-140000.tar.gz` |
| JORF | 269 | `Freemium_jorf_global_20250713-140000.tar.gz` |
| JORFSIMPLE | 269 | `Freemium_jorf_simple_20250713-140000.tar.gz` |
| CNIL | 36 | `Freemium_cnil_global_20250713-140000.tar.gz` |
| CONSTIT | 9 | `Freemium_constit_global_20250713-140000.tar.gz` |
| DOLE | 83 | delta-style legislative dossier archives |

Sampled archive members confirm the expected official XML families:

| Source | Sample member / root |
|---|---|
| CASS | `JURITEXT...xml`, root `TEXTE_JURI_JUDI` |
| CAPP | `JURITEXT...xml`, expected same judicial DILA root family |
| INCA | `JURITEXT...xml`, expected same judicial DILA root family |
| JADE | `CETATEXT...xml`, expected administrative DILA root family |
| CONSTIT | `CONSTEXT...xml`, root `TEXTE_JURI_CONSTIT` |
| CNIL | `CNILTEXT...xml`, root `TEXTE_CNIL` |
| DOLE | `JORFDOLE...xml`, root `DOSSIER_LEGISLATIF` |
| KALI | `KALICONT...xml` baseline structure observed |

## Priority 1: Phase 2 court-decision corpus

Ingest first:

1. `CASS`
2. `INCA`
3. `CAPP`
4. `JADE`

Why:

- They match the current Phase 2 mandatory DILA bulk adapter scope.
- They provide local full-corpus coverage without using Judilibre API egress for the baseline.
- They let `status.phase2_gate` prove both judicial and administrative jurisprudence corpus presence.
- They fit the current provenance model: archive/member path, source payload hash, source family, parser/schema/code versions, replay signatures, and heuristic chunk provenance where official zones are absent.

Recommended handling:

- Treat `Freemium_*_global_20250713-140000.tar.gz` as the baseline per family.
- Apply subsequent family deltas in timestamp order using the existing archive precedence/replay logic.
- Store source family separately (`cass`, `inca`, `capp`, `jade`) so status can report coverage by family.
- Keep chunk provenance honest: DILA bulk records are full-corpus official records, but not Judilibre zone-accurate records.
- Keep Judilibre enrichment separate and additive: use it for zone detail, cite-online verification, and deltas where appropriate.

## Priority 2: schema/DTD support

Use `/mnt/nas/opendata/DTD_LEGIFRANCE` as supporting material for parser validation and compatibility checks.

Observed files include:

- `DTD_LEGIFRANCE_20181017.7z`
- `Dila_Note_migration_technique_opendata_04102023.pdf`
- `DILA_NOR_technique_JORF-20240710.pdf`
- `exemples_donnees_avant_migration_donnees_migrees_Dila_20231012/`

Recommendation:

- If `/mnt/models/opendata/DTD_LEGIFRANCE/extracted/...` is already canonical and complete, keep using it.
- If a parser test fails because local DTD coverage is missing, compare against this NAS bundle before assuming the source family is unsupported.
- Do not make DTD presence a substitute for real archive-member parser tests.

## Priority 3: focused post-Phase-2 expansions

These are useful, but should not distract from the Phase 2 jurisprudence gate.

### CONSTIT

Path: `/mnt/nas/opendata/CONSTIT`

Usefulness: high, targeted expansion. It contains Constitutional Council decision XML with root `TEXTE_JURI_CONSTIT` and compact corpus size.

Recommendation: add after Phase 2 court-decision ingestion is stable. This is a good bridge between jurisprudence and constitutional review workflows, but it likely needs a distinct parser branch and eval category rather than being forced into CASS/JADE semantics.

### CNIL

Path: `/mnt/nas/opendata/CNIL`

Usefulness: medium-high for data protection/regulatory search. Sample root is `TEXTE_CNIL`.

Recommendation: later specialist corpus. It is official and citeable, but it should be reported as regulatory decisions/deliberations, not general jurisprudence. Add only after a source-specific identifier/citation contract is defined.

### KALI

Path: `/mnt/nas/opendata/KALI`

Usefulness: high for labour-law coverage, but outside the immediate Phase 2 claim.

Recommendation: Phase 3 candidate. It can materially improve labour-law search and statute-to-agreement workflows. Keep it separate from LEGI because collective agreements have different hierarchy, scope, and citation semantics.

### JORF and JORFSIMPLE

Paths: `/mnt/nas/opendata/JORF`, `/mnt/nas/opendata/JORFSIMPLE`

Usefulness: high as provenance, publication, and cross-link enrichment; lower as a first search target.

Recommendation: defer as a full searchable corpus. Use selectively for publication provenance, NOR/dossier linkage, and official text history once the core statutory and jurisprudence paths are stable. Avoid duplicating LEGI-derived texts without a clear canonical precedence rule.

### DOLE

Path: `/mnt/nas/opendata/DOLE`

Usefulness: medium for legislative dossiers. Sample root is `DOSSIER_LEGISLATIF`.

Recommendation: later graph/context enrichment. This can help explain legislative provenance and link texts to projects/proposals, but it is not jurisprudence and should not be used to satisfy Phase 2 decision-search gates.

### CIRCULAIRES

Path: `/mnt/nas/opendata/CIRCULAIRES`

Usefulness: potentially useful for administrative guidance search, but ingestion cost is high because the tree includes very large PDF archives and separate XML/txt material.

Recommendation: defer until there is a designed circulars corpus contract. Prefer XML over PDF first. Do not index the 40G PDF payload opportunistically without a clear OCR/text extraction, provenance, and citation policy.

## Not recommended for the near-term jurisearch core

These may be official or useful in another product, but they do not currently fit the locked jurisearch claim or Phase 2 gate:

- `BOAMP`, `BODACC`, `BALO`, `ASSOCIATIONS`, `COMPTES_DES_ASSOCIATIONS`, `RefOrgaAdminEtat`, `Base_donnees_locales`, `SERVICE-PUBLIC`, `ENTREPRENDRE_SERVICE-PUBLIC`, `DISCOURS_PUBLICS`, `Questions-Reponses`.
- `AMF` could be a future financial-regulatory search corpus, but it needs a separate domain contract and should not be mixed into the general French legal/jurisprudence gate.
- `SERVICE-PUBLIC` and simulators can be excellent user-help material, but they are not authoritative legal sources in the same sense as LEGI/JORF/jurisprudence and should not feed authoritative citation answers.

## Concrete next steps

1. Compare `/mnt/nas/opendata/{CASS,INCA,CAPP,JADE,DTD_LEGIFRANCE}` against `/mnt/models/opendata` and record whether NAS is fresher or merely a mirror.
2. If NAS is fresher, refresh `/mnt/models/opendata` deliberately rather than silently changing canonical input roots.
3. Add or update a Phase 2 source-inventory evidence artifact listing baseline archive, delta count, latest delta timestamp, byte size, and source path for each of CASS/INCA/CAPP/JADE.
4. Run a bounded parser smoke over one baseline member from each family and assert root classification, identifier extraction, date extraction, pseudonymisation preservation, and heuristic chunk provenance.
5. Only after Phase 2 corpus + benchmark gate are green, open separate design tickets for CONSTIT, CNIL, KALI, and JORF/DOLE enrichment.

## Bottom line

The useful immediate data is present: `/mnt/nas/opendata` contains the complete Phase 2 DILA bulk jurisprudence families plus DTD support. The lowest-risk recommendation is to use it to verify or refresh the canonical `/mnt/models/opendata` staging tree, then keep ingestion focused on CASS/INCA/CAPP/JADE before expanding to constitutional, regulatory, collective-agreement, gazette, dossier, or circular corpora.
