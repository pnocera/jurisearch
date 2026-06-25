# Business-Law Expansion: Complementary Sources and Features

Date: 2026-06-24
Status: research / product-analysis note, no implementation
Scope: complementary corpora and features that would make `jurisearch` materially more useful to a French business lawyer (`avocat d'affaires` / `juriste d'entreprise`) beyond the current LEGI + jurisprudence foundation.

---

## Executive Recommendation

`jurisearch` is already strong on French primary law: codified LEGI articles, official jurisprudence, temporal article versions, citation lookup, hybrid retrieval, zone-aware Cassation retrieval, and authority-aware ranking work. The best business-law expansion is not "more generic documents"; it is a **source-typed business-law graph** that adds:

1. **Adjacent official DILA/Legifrance corpora**: `JORF`, `KALI`, `CNIL`, `CONSTIT`, `CIRCULAIRES`, `DOLE`, and eventually `SARDE`.
2. **EU law and CJEU materials**: EUR-Lex / Cellar metadata and content, with CELEX/ELI/ECLI citation resolution.
3. **Official tax doctrine**: BOFiP, because business lawyers constantly need the administration's published position, not only statutes and cases.
4. **Regulator doctrine, decisions, and sanctions**: AMF, ACPR, Autorite de la concurrence, CNIL, and selected INPI materials.
5. **Company-publicity and market context data**: Sirene, RNE/Data INPI, BODACC, BALO / info-financiere, BOAMP/TED. These should be linked as entity/context records, not ranked as if they were legal authorities.

The safest implementation strategy is to extend the existing official-source discipline: each new corpus gets a source registry entry, canonical record type, citation/link model, authority tier, freshness manifest, and an explicit "legal force" label. That prevents a BOFiP comment, AMF position, BODACC announcement, Cassation decision, and Code de commerce article from being mixed as equivalent legal sources.

---

## What Business Lawyers Need That The Current Corpus Does Not Yet Cover

### 1. Administrative doctrine and regulator practice

For many business-law questions, the practical answer is not only in a code article or a court decision. It is in a tax doctrine page, regulator position, enforcement decision, guideline, sanction, or market authority FAQ.

High-value domains:

- tax: BOFiP doctrine, rescrits of general scope, ministerial answers, comments on case-law;
- financial markets: AMF general regulation, doctrine, sanctions, transactions, issuer/market decisions;
- banking and insurance: ACPR decisions, sanctions, recommendations, positions, instructions, guidelines;
- competition: Autorite de la concurrence decisions, opinions, merger-control decisions, related appeal/court information;
- data protection: CNIL sanctions, deliberations, guidelines, recommendations, reference frameworks;
- IP and corporate formalities: INPI decisions, BOPI, RNE filings and documents.

### 2. EU law as a first-class authority layer

Business law is heavily EU-shaped: GDPR, competition, corporate law directives, financial regulation, AML, market abuse, MiCA, consumer rules, public procurement, sanctions, and sector regulation. A French-only answer is often incomplete unless it can surface the relevant regulation/directive, national transposition, CJEU interpretation, and French implementing text.

### 3. Entity-aware due diligence context

For due diligence and litigation prep, lawyers often need to pivot from a legal issue to a company or counterparty:

- identify the legal entity and establishments by SIREN/SIRET;
- fetch RNE acts, statutes, and non-confidential annual accounts;
- see BODACC events such as creations, sales/transfers, insolvency notices, dissolutions;
- inspect listed-company regulated information;
- find AMF/ACPR/CNIL/competition sanctions tied to the entity;
- connect those facts back to the governing legal rules.

This is not legal authority, but it is very useful legal-work context.

---

## Priority Source List

### Tier 1: Add Next

#### 1. KALI - national collective agreements

Why it matters:

- Business lawyers often cross into employment issues: executive departures, transfer of undertakings, incentive plans, restructuring, sector obligations, non-competes, working time, and collective-bargaining constraints.
- `KALI` is a DILA/Legifrance official XML/API corpus, so it fits the existing official-source posture.

Access / evidence:

- DILA lists `KALI` as "Conventions collectives nationales" with XML and API access via Legifrance/PISTE.
- Source: https://www.dila.gouv.fr/services/repertoire-des-informations-publiques/les-donnees-juridiques

Ingestion shape:

- canonical kind: `collective_agreement`, `agreement_article`, `agreement_section`;
- citation support: IDCC, convention title, article numbers, effective dates;
- retrieval filters: IDCC, sector, date, article, agreement status;
- temporal model similar to LEGI, but keep agreement-specific amendment history and extension status separate.

Caveats:

- Do not rank collective-agreement provisions as statutes. They are binding in their scope, but authority and applicability are conditional on sector/employer coverage.

#### 2. JORF - raw Journal officiel texts

Why it matters:

- LEGI gives consolidated law; business lawyers also need original decrees, orders, transitional provisions, entry-into-force text, reports, notices, and publication history.
- JORF is the bridge from enacted text to codified/consolidated article versions.

Access / evidence:

- DILA lists `JORF` as "Textes publies au Journal officiel de la Republique francaise" with XML and API access.
- Legifrance states that legal data are reusable under Licence Ouverte 2.0 and that the stable API has been available since 2023-04-04 via PISTE.
- Sources:
  - https://www.dila.gouv.fr/services/repertoire-des-informations-publiques/les-donnees-juridiques
  - https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api

Ingestion shape:

- canonical kind: `jorf_text`, `jorf_article`, `jorf_notice`;
- graph edges: `jorf_text modifies legi_article`, `jorf_text creates code`, `jorf_text transposes eu_act`, `jorf_text cited_by decision`;
- feature: "show original publication / entry into force / consolidation path".

Caveats:

- JORF is not a substitute for the consolidated version. Retrieval should distinguish "original promulgated text" from "current/as-of consolidated law".

#### 3. BOFiP - official tax doctrine

Why it matters:

- For tax-driven M&A, financing, restructuring, transfer pricing, VAT, withholding tax, and corporate tax, BOFiP is often the first practical source after the Code general des impots.
- It contains administrative comments, rescrits of general scope, ministerial answers, and comments on case-law affecting doctrine.

Access / evidence:

- data.gouv describes BOFiP-Impots as containing comments on legislative/regulatory tax provisions, general-scope rescrits, innovative ministerial answers, and comments on jurisprudence affecting doctrine.
- A separate "publications en vigueur" dataset is under Licence Ouverte 2.0 and was updated on 2026-06-18 in the observed data.gouv page.
- Sources:
  - https://www.data.gouv.fr/datasets/bulletin-officiel-des-finances-publiques-impots
  - https://www.data.gouv.fr/datasets/bofip-impots-publications-en-vigueur

Ingestion shape:

- canonical kind: `tax_doctrine`, `tax_doctrine_section`, `rescrit_general`, `ministerial_answer`;
- IDs: BOI identifier, publication date, update date, plan hierarchy;
- graph edges: `doctrine comments legi_article`, `doctrine cites decision`, `doctrine supersedes doctrine`;
- features: `search --domain tax`, `fetch --as-of doctrine-date`, "administrative doctrine vs statute" warnings.

Caveats:

- BOFiP is official administrative doctrine, not legislation and not court law. The UI/JSON must say so. For contentious questions, show whether the doctrine is supported, limited, or contradicted by case-law.

#### 4. CNIL deliberations, sanctions, and open datasets

Why it matters:

- Data protection is now core business law: due diligence, SaaS contracts, marketing, HR, security incidents, AI/data projects, and M&A risk.
- CNIL materials are both in the DILA legal corpus and on CNIL/data.gouv.

Access / evidence:

- DILA lists `CNIL` as "Deliberations de la CNIL" with XML and API access.
- CNIL's Open CNIL page says its public datasets are available on data.gouv and reusable for free.
- CNIL also says its decisions include positions, guidelines, recommendations, reference frameworks, sanctions, and formal notices.
- Sources:
  - https://www.dila.gouv.fr/services/repertoire-des-informations-publiques/les-donnees-juridiques
  - https://www.cnil.fr/fr/opendata
  - https://www.cnil.fr/fr/textes-officiels/les-decisions-de-la-cnil

Ingestion shape:

- canonical kind: `regulator_decision`, `regulator_guideline`, `sanction`, `formal_notice`;
- regulator: `cnil`;
- fields: sanction amount, legal basis, GDPR article, anonymization/publication limit, appeal status when available;
- graph edges: CNIL decision -> GDPR article, Loi Informatique et Libertes article, Conseil d'Etat appeal, company/entity.

Caveats:

- Sanction pages and datasets may include personal data and publication-duration limits. Preserve upstream anonymization and add a retention/compliance policy before entity indexing.

#### 5. EUR-Lex / Cellar - EU legislation and official metadata

Why it matters:

- Many business-law answers need the EU source: regulations, directives, implementing/delegated acts, recitals, consolidated versions, and CJEU interpretation.
- EUR-Lex/Cellar provides CELEX-stable identifiers, metadata relationships, multilingual content, and bulk/access mechanisms.

Access / evidence:

- EUR-Lex content is stored in Cellar; Cellar metadata and content can be accessed through a SPARQL endpoint and RESTful API.
- EUR-Lex also offers a legal-acts-in-force data dump for CELEX sector 3, requiring EU Login, and data.europa.eu offers Official Journal lists from 2004 onward with XML Formex links.
- Sources:
  - https://eur-lex.europa.eu/content/help/data-reuse/reuse-contents-eurlex-details.html
  - https://op.europa.eu/en/web/cellar/cellar-data

Ingestion shape:

- canonical kind: `eu_act`, `eu_article`, `eu_case`, `eu_oj_notice`;
- identifiers: CELEX, ELI, ECLI, OJ reference, procedure number;
- graph edges: directive -> French JORF/LEGI transposition, regulation -> cited French decision, CJEU case -> national case/legal issue;
- retrieval filters: EU act type, date, CELEX sector, in-force, language, EuroVoc/domain.

Caveats:

- CJEU data is accessible through EUR-Lex/InfoCuria, but Curia itself is not as clean a bulk source as DILA/XML. Treat CJEU ingestion as a separate adapter with explicit provenance and rate/format constraints.

### Tier 2: Important For Business Practice

#### 6. Autorite de la concurrence - decisions, opinions, merger control

Why it matters:

- Competition, distribution, abuse of dominance, cartels, digital markets, merger control, and damages litigation are core business-law topics.
- The authority's decisions and opinions often link to related court/jurisprudence history.

Access / evidence:

- The authority states its decisions and opinions include opinions, contentious decisions, interim measures, and related jurisprudence; it also says they are available in a single free-access database on data.gouv.
- Source: https://www.autoritedelaconcurrence.fr/fr/liste-des-decisions-et-avis

Ingestion shape:

- canonical kind: `competition_decision`, `competition_opinion`, `merger_control_decision`;
- fields: decision number, case type, sector, parties, sanction amount, commitments, appeal/court links;
- graph edges: decision -> Code de commerce articles, EU competition law, court appeals, company entities.

Caveats:

- PDFs and website pages may need scraper/parser work. Use the data.gouv database if it contains structured records; otherwise isolate a regulator-web adapter from official DILA ingestion.

#### 7. AMF - financial-market regulation, doctrine, sanctions, transactions

Why it matters:

- Listed companies, market abuse, public offers, asset management, intermediaries, crypto-assets, disclosure, and compliance questions depend heavily on AMF doctrine and enforcement.

Access / evidence:

- AMF navigation exposes the General Regulation, Doctrine, sanctions decisions, sanction jurisprudence, transactions, RSS subscriptions, and recent sanction/transaction records.
- Observed AMF sanction page shows decision numbers, themes, amounts, appeal status, and downloadable decisions.
- Sources:
  - https://www.amf-france.org/fr/sanctions-transactions/sanctions-et-transactions-accueil
  - https://www.amf-france.org/fr/reglementation

Ingestion shape:

- canonical kind: `market_regulation`, `market_doctrine`, `market_sanction`, `market_transaction`;
- fields: theme, amount, appeal status, instrument/entity, public-offer type, AMF doc type;
- graph edges: AMF sanction -> Code monetaire et financier, AMF General Regulation, MAR/MiCA/Prospectus Regulation, court appeals, listed issuer.

Caveats:

- AMF site content is partly HTML/PDF and RSS rather than a clean bulk corpus. Start with sanctions/transactions and formal doctrine; keep a source manifest that flags parser quality.

#### 8. ACPR - banking/insurance doctrine, official register, sanctions

Why it matters:

- Banking, insurance, payment services, AML/CFT, prudential issues, conduct risk, and distribution duties are heavily ACPR-shaped.

Access / evidence:

- ACPR has an official register with decisions, instructions, guidelines, lists, notices, positions, principles, recommendations, and a sanctions collection.
- The sanctions collection covers Commission des sanctions decisions by publication year back to 2011 in the observed page.
- Sources:
  - https://acpr.banque-france.fr/fr/reglementation/registre-officiel/decisions
  - https://acpr.banque-france.fr/fr/reglementation/recueil-des-sanctions
  - https://acpr.banque-france.fr/fr/reglementation/registre-officiel/recommandations

Ingestion shape:

- canonical kind: `prudential_decision`, `prudential_guideline`, `prudential_sanction`, `prudential_recommendation`;
- fields: sector, institution type, AML/CFT flag, sanction amount, appeal result, document category;
- graph edges: ACPR decision -> CMF/insurance code articles, EU prudential texts, Conseil d'Etat appeal, entity.

Caveats:

- Some pages can be difficult to fetch reliably and many source docs are PDFs. Treat PDF extraction confidence as a first-class field.

#### 9. CONSTIT - Conseil constitutionnel decisions

Why it matters:

- QPC and constitutionality decisions affect corporate tax, sanctions, financial regulation, employment law, and competition/administrative enforcement.

Access / evidence:

- DILA lists `CONSTIT` as decisions of the Conseil constitutionnel with XML/API access.
- The Conseil constitutionnel also exposes open data channels, but the DILA path is the lower-risk first adapter because it matches existing official XML/API work.
- Source: https://www.dila.gouv.fr/services/repertoire-des-informations-publiques/les-donnees-juridiques

Ingestion shape:

- canonical kind: `constitutional_decision`;
- graph edges: decision -> statute article, code article, QPC/court referral, repeal/effective date effects;
- feature: "constitutionality status" badge on statutory articles where applicable.

Caveats:

- Constitutional effect dates and reservations of interpretation are legally subtle. Do not model them as simple "valid/invalid" booleans without a dedicated status schema.

#### 10. CIRCULAIRES and SARDE

Why it matters:

- Circulars/instructions can explain administration practice; SARDE can improve thematic navigation across legal texts.

Access / evidence:

- DILA lists `CIRCULAIRES` and `SARDE` with XML/API access.
- Source: https://www.dila.gouv.fr/services/repertoire-des-informations-publiques/les-donnees-juridiques

Ingestion shape:

- `CIRCULAIRES`: source-typed administrative instruction records with ministry, date, theme, cited legal basis.
- `SARDE`: taxonomy/thesaurus-like enrichment, not ordinary legal text; use it to improve facets, expansion, and browse, not as a legal authority.

Caveats:

- Circulars can be obsolete, withdrawn, or non-binding. The JSON/result contract must distinguish "administrative instruction" from law.

### Tier 3: Entity / Due-Diligence Context Layer

#### 11. Sirene API

Why it matters:

- It gives the identity spine for company-aware search: SIREN/SIRET, current and historical units, establishments, names, addresses, activity codes.

Access / evidence:

- data.gouv says the Sirene API exposes companies and establishments registered since 1973, including closed units, with unit, multicriteria, and phonetic search.
- Source: https://www.data.gouv.fr/dataservices/api-sirene-open-data

Ingestion shape:

- canonical kind: `entity`, `establishment`;
- fields: SIREN, SIRET, denomination, legal category, NAF/APE, address, open/closed status, dates;
- graph edges: entity -> BODACC notice, RNE act/account, AMF/ACPR/CNIL/competition decision, court decision mention.

Caveats:

- Sirene is identity/reference data, not legal analysis. It should power entity resolution and filters, not become a legal authority result source.

#### 12. RNE / Data INPI - acts, statutes, annual accounts, formalities

Why it matters:

- Corporate due diligence needs statutes, acts, annual accounts, management/control formalities, beneficial context, and filing chronology.

Access / evidence:

- Data INPI says RNE data are available for free via SFTP and via APIs; RNE/API data include JSON/PDF, non-confidential accounts since 2017, and daily updates.
- Source: https://data.inpi.fr/content/editorial/Acces_API_Entreprises

Ingestion shape:

- canonical kind: `rne_formality`, `company_act`, `company_accounts`, `company_statutes`;
- link to Sirene entity spine;
- extracted fields: filing date, act type, confidentiality flag, account year, event type.

Caveats:

- Public/non-confidential boundaries matter. Do not ingest confidential accounts or non-public documents. PDF extraction should preserve source files and confidence.

#### 13. BODACC - civil and commercial notices

Why it matters:

- BODACC is central for insolvency, business sales, registrations, modifications, removals, succession notices, and commercial publicity.

Access / evidence:

- data.gouv describes BODACC notices as civil and commercial announcements governed by Code de commerce article R.123-209, emitted by commercial registries, civil commercial courts, or insolvency practitioners.
- Source: https://www.data.gouv.fr/datasets/bodacc

Ingestion shape:

- canonical kind: `bodacc_notice`;
- fields: notice type, publication date, SIREN, registry, insolvency event, sale/transfer event, address;
- features: entity timeline, event alerts, "show all BODACC events around transaction date".

Caveats:

- BODACC events are public notices, not a legal opinion. They should be used as facts/context.

#### 14. BALO / info-financiere - regulated information for listed companies

Why it matters:

- Listed-company work needs regulated information, issuers' filings, financial reports, prospectus/public-offer context, and market announcements.

Access / evidence:

- data.gouv describes the info-financiere API as the central official storage mechanism for regulated information from listed companies, transmitted by issuers and the AMF under the Transparency Directive/OAM.
- The Journal officiel open-data page notes BALO covers information for companies making public offerings and banking/financial institutions.
- Sources:
  - https://www.data.gouv.fr/dataservices/api-info-financiere
  - https://www.journal-officiel.gouv.fr/pages/donnees-ouvertes-et-api/

Ingestion shape:

- canonical kind: `regulated_information`, `balo_notice`;
- fields: issuer, ISIN/SIREN when available, publication type, date, OAM category, document URL;
- graph edges: issuer -> AMF decision -> market regulation -> corporate action.

Caveats:

- This is partly financial disclosure rather than legal source material. It belongs in the entity/context layer and due-diligence workflows.

#### 15. BOAMP and TED - public procurement notices

Why it matters:

- Public procurement is important for commercial contracts, regulated sectors, compliance, and litigation. French BOAMP plus EU TED links are useful.

Access / evidence:

- data.gouv says the BOAMP API is free, supports querying/filtering, preview/download, multiple filters, CSV/JSON/Excel, keyword and criteria search.
- Source: https://www.data.gouv.fr/dataservices/api-bulletin-officiel-des-annonces-des-marches-publics-boamp

Ingestion shape:

- canonical kind: `procurement_notice`;
- fields: buyer, supplier when available, CPV, procedure type, deadlines, award result, amount;
- graph edges: procurement notice -> Code de la commande publique article, company entity, litigation decision.

Caveats:

- Procurement notices are transactional facts. They should not pollute legal-authority ranking.

---

## Feature Recommendations

### 1. Source-typed authority model

Add a durable `authority_profile` or equivalent source registry concept before ingesting heterogeneous sources.

Minimum dimensions:

- `source_family`: statute, case-law, EU law, regulator doctrine, regulator sanction, administrative doctrine, company-publicity fact, procurement notice;
- `legal_force`: binding law, binding within scope, court decision, official doctrine, soft law, administrative practice, public notice/fact;
- `issuer`: DILA, EUR-Lex/Publications Office, DGFiP, AMF, ACPR, CNIL, Autorite de la concurrence, INPI, INSEE, DILA/BODACC;
- `temporal_model`: consolidated/as-of, publication-only, current-only, versioned, event timeline;
- `citation_keys`: CELEX, ELI, ECLI, BOI id, IDCC, SIREN/SIRET, NOR, decision number, sanction number.

Why this matters:

- A user asking "what is the rule?" should not receive a BODACC notice beside a Code de commerce article as if they were peers.
- A user asking "what is the regulator's current view?" should see AMF/ACPR/CNIL doctrine before unrelated cases.

### 2. Business-domain facets

Add domain facets that are source-aware:

- `corporate`
- `mna`
- `tax`
- `employment`
- `financial-markets`
- `banking-insurance`
- `competition`
- `data-protection`
- `ip`
- `public-procurement`
- `insolvency`

Implementation approach:

- Use curated seed vocabularies plus source metadata, not LLM labels as authority.
- Let SARDE and official tax/regulator classifications enrich facets.
- Keep domain facets advisory; do not hide results unless the user explicitly filters.

### 3. Cross-source citator and relationship graph

High-value graph edges:

- JORF text creates/modifies LEGI article;
- EU directive/regulation cited by French statute/regulator/case;
- French statute transposes EU directive;
- BOFiP comments a CGI article and cites cases;
- CNIL/AMF/ACPR/competition decision applies code or EU provisions;
- regulator sanction appealed to Conseil d'Etat or Cour d'appel;
- court decision cites regulator decision or EU/CJEU case;
- company entity linked to sanctions, BODACC notices, RNE filings, AMF disclosures.

User-facing features:

- `related --why`: show typed edges, not just similar text;
- `context --issue tax --as-of DATE`: statute + BOFiP + cases;
- `context --eu CELEX`: EU act + French implementing texts + French/CJEU case-law;
- `context --company SIREN`: public legal events + regulator/court hits + relevant law.

### 4. "As-of" across more than LEGI

Extend temporal honesty beyond statute articles:

- LEGI: already as-of.
- KALI: as-of collective-agreement provisions.
- BOFiP: doctrine version/update as-of, if historical archives are available; otherwise current-only with update date.
- JORF: publication/entry-into-force date.
- EU law: in-force / consolidated-date if using EUR-Lex consolidated texts and metadata.
- Regulator doctrine: publication and update date, current/archived status when known.
- Entity context: event timeline, not legal in-force status.

Do not fake historical doctrine for sources that only expose current state. Return `temporal_coverage: current_only` or `unknown_history` explicitly.

### 5. Regulator practice mode

Add a query mode for "what does the regulator do/say?".

Example output shape:

- controlling law: code/EU provisions;
- official doctrine/guidelines: AMF/ACPR/CNIL/competition;
- enforcement examples: sanctions/transactions/decisions;
- appeal status: if available;
- caveat: soft law vs binding law vs sanction decision.

This is valuable because lawyers often need risk posture and regulator expectations, not only court holdings.

### 6. Entity-aware due diligence mode

Add an entity resolution and context layer:

- resolve company name -> SIREN/SIRET through Sirene;
- link RNE acts/accounts, BODACC events, BALO/info-financiere disclosures;
- surface sanctions and regulator decisions involving the entity;
- connect to relevant law and case-law by domain.

This should be an opt-in context command, not part of default legal search.

Proposed CLI surface:

- `entity resolve "company name"`
- `entity timeline --siren ...`
- `context --company ... --issue "market abuse"`
- `search --entity ... --sources amf,acpr,cnil,competition,cases`

### 7. EU/French transposition and hierarchy views

Features:

- resolve CELEX/ELI/ECLI citations;
- show EU act -> French implementing JORF/LEGI provisions;
- show CJEU cases interpreting an EU act;
- show French decisions applying the EU act;
- distinguish regulation/directive/decision/recommendation and direct applicability.

This is especially important for GDPR, financial services, competition, public procurement, corporate reporting, and consumer law.

### 8. Alerting and incremental sync

Business users need change monitoring:

- watched code articles;
- watched BOFiP plan nodes;
- watched regulator topics;
- watched company SIREN;
- watched EU act/CELEX;
- watched case-law query.

Implementation notes:

- Use source-specific manifest tables already aligned with ingest-health concepts.
- Emit machine-readable deltas: added/changed/withdrawn/superseded.
- For PDFs/web pages, hash source payloads and extracted text separately.

### 9. Evaluation categories for business law

Current legal retrieval gates should not be stretched to claim business-law competence. Add measured categories:

- known tax doctrine lookup: query excerpt -> BOFiP section;
- statute + doctrine workflow: CGI article -> BOFiP comments + cases;
- EU/French workflow: CELEX directive/regulation -> French implementing articles + cases;
- regulator sanction lookup: sanction number/entity/theme -> AMF/ACPR/CNIL decision;
- competition decision lookup: decision number/theme/entity -> Autorite decision;
- entity context lookup: SIREN/name -> correct Sirene/RNE/BODACC context;
- collective agreement lookup: IDCC/article/as-of date -> KALI provision.

Keep these as source-specific measured gates first. Do not publish a broad "business law answer quality" claim until these pass.

---

## Implementation Phasing

### Phase B0 - Source registry and canonical extensions

Build first:

- source registry with `authority_profile`;
- canonical record variants for doctrine, regulator decisions, EU acts, company facts;
- source manifests and freshness/coverage metrics;
- shared citation-key table: NOR, CELEX, ELI, ECLI, BOI, SIREN/SIRET, IDCC, sanction/decision numbers;
- JSON result contract fields: `source_family`, `legal_force`, `issuer`, `temporal_coverage`, `extraction_confidence`.

Exit gate:

- existing LEGI/jurisprudence queries remain byte-identical unless new source filters are requested.

### Phase B1 - DILA-adjacent official corpora

Add:

- KALI;
- JORF;
- CNIL via DILA;
- CONSTIT;
- CIRCULAIRES and SARDE as lower-risk enrichments.

Why first:

- Same official XML/API posture as current ingestion.
- Highest fit with existing parser/projection/storage architecture.

### Phase B2 - BOFiP

Add:

- current BOFiP publications;
- BOI identifier resolver;
- plan hierarchy;
- statute/case citation extraction;
- "tax doctrine" search/context mode.

Why second:

- Highest business-law value per implementation unit after DILA-adjacent corpora.

### Phase B3 - EU law

Add:

- EUR-Lex/Cellar adapter;
- CELEX/ELI resolver;
- EU legal-act metadata graph;
- French transposition links through JORF/LEGI when available;
- CJEU ingestion only after metadata/content route is proven.

Why third:

- Essential, but a different metadata model and multilingual corpus.

### Phase B4 - Regulators

Add in order:

1. Autorite de la concurrence decisions/opinions.
2. AMF sanctions/transactions + doctrine.
3. ACPR sanctions/register/guidelines.
4. CNIL website-only additions beyond DILA.
5. INPI BOPI/IP decisions where useful.

Why fourth:

- High value, but often HTML/PDF/web-specific and less uniform than DILA/EUR-Lex/BOFiP.

### Phase B5 - Entity/context layer

Add:

- Sirene;
- RNE/Data INPI acts/accounts/formalities;
- BODACC;
- BALO/info-financiere;
- BOAMP/TED where procurement matters.

Why last:

- Very useful for due diligence, but it changes the product boundary from legal research to legal-work context. Keep it isolated and opt-in.

---

## Caveats And Non-Goals

- **Commercial doctrine is still missing.** Dalloz, LexisNexis/JurisClasseur, Lamy, Navis/Francis Lefebvre, and practitioner commentary are not open official corpora. `jurisearch` can become excellent at official primary/administrative/regulator sources, but not a full replacement for paid doctrine.
- **Regulator doctrine is not law.** AMF/ACPR/CNIL/competition positions and guidance can be extremely persuasive and practically decisive, but they need legal-force labels.
- **Company filings are facts, not authority.** RNE/BODACC/Sirene/info-financiere results should support diligence and entity context, not answer legal-rule questions directly.
- **PDF extraction is a quality risk.** AMF/ACPR/INPI and some BOFiP/regulator assets may require PDF parsing. Store the original asset, extraction hash, and extraction confidence.
- **Personal data and publication windows matter.** CNIL/AMF/ACPR sanctions and company filings may contain personal data or anonymization/publication rules. Preserve source redactions and implement deletion/refresh policies.
- **EU/CJEU ingestion is not a simple DILA clone.** EUR-Lex/Cellar has strong metadata access, but CJEU/Curia full case-law harvesting is a separate adapter problem.
- **No legal advice claim.** Even with these sources, `jurisearch` remains retrieval/context infrastructure. It can cite and organize sources; it should not claim to draft opinions or guarantee completeness.

---

## Lowest-Risk Product Claim After These Additions

If implemented with the authority/source model above, the honest claim becomes:

> `jurisearch` is a local, official-source-first French and EU business-law research engine: it retrieves consolidated statutes, case-law, tax doctrine, EU materials, regulator doctrine/enforcement, and public company context with provenance, legal-force labels, temporal coverage, and citation links.

Avoid claiming:

- "complete business-law coverage";
- "replaces doctrine databases";
- "knows current good law" unless revirement/withdrawal/supersession modeling is implemented and validated;
- "company due diligence automation" unless entity matching, false-positive handling, and document completeness are separately evaluated.

---

## Source Links Checked

- DILA legal corpora list: https://www.dila.gouv.fr/services/repertoire-des-informations-publiques/les-donnees-juridiques
- Legifrance open data/API terms and PISTE API: https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api
- BOFiP dataset: https://www.data.gouv.fr/datasets/bulletin-officiel-des-finances-publiques-impots
- BOFiP current publications dataset: https://www.data.gouv.fr/datasets/bofip-impots-publications-en-vigueur
- EUR-Lex data reuse / Cellar / data dump: https://eur-lex.europa.eu/content/help/data-reuse/reuse-contents-eurlex-details.html
- Cellar data access: https://op.europa.eu/en/web/cellar/cellar-data
- Autorite de la concurrence decisions/opinions: https://www.autoritedelaconcurrence.fr/fr/liste-des-decisions-et-avis
- AMF sanctions/transactions: https://www.amf-france.org/fr/sanctions-transactions/sanctions-et-transactions-accueil
- ACPR official register decisions: https://acpr.banque-france.fr/fr/reglementation/registre-officiel/decisions
- ACPR sanctions collection: https://acpr.banque-france.fr/fr/reglementation/recueil-des-sanctions
- CNIL open data: https://www.cnil.fr/fr/opendata
- CNIL decisions: https://www.cnil.fr/fr/textes-officiels/les-decisions-de-la-cnil
- Sirene API: https://www.data.gouv.fr/dataservices/api-sirene-open-data
- Data INPI / RNE APIs: https://data.inpi.fr/content/editorial/Acces_API_Entreprises
- BODACC dataset: https://www.data.gouv.fr/datasets/bodacc
- BOAMP API: https://www.data.gouv.fr/dataservices/api-bulletin-officiel-des-annonces-des-marches-publics-boamp
- Info-financiere API: https://www.data.gouv.fr/dataservices/api-info-financiere
