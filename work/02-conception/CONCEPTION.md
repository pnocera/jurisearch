# `jurisearch` — Conception Document

Date: 2026-06-20  
Status: locked conception, reconciled with `work/reviews/2026-06-20-conception-review.md` and derived from `work/01-design/DESIGN.md` / `work/01-design/DECISIONS.md`  
Nature: **conception document**, not an implementation plan

---

## 1. Purpose

`jurisearch` is a local-first legal search engine exposed as a CLI for AI agents. Its purpose is to return the smallest set of authoritative, date-correct, precisely citeable French legal passages needed for legal research workflows.

The tool does **not** answer legal questions by itself. It finds, structures, ranks, dates, and verifies legal sources so an external agent can reason from evidence.

The product ambition is explicit: **the best-in-class French juridic search engine for AI agents**, production-grade from the start. It is not an MVP, demo, generic RAG prototype, chatbot, or legal-advice product.

The claim is phased:

- **Phase 1:** best-in-class LEGI / statutory search.
- **Phase 2:** best-in-class French juridic search across statutes and jurisprudence.
- **Later phases:** additional corpora and ranking improvements only after the core legal correctness model is proven.

---

## 2. Product Shape

`jurisearch` is a **CLI-only** product.

The agent-facing contract is the binary itself:

- `jurisearch --help`
- `jurisearch <command> --help`
- `jurisearch help agent`
- `jurisearch help schema --json`
- one-shot commands with JSON output
- long-lived stdin/stdout JSONL session mode

There is deliberately:

- no MCP server;
- no HTTP server;
- no long-running `serve` daemon;
- no Qdrant or external vector/search service.

Warm multi-call agent workflows are handled through `jurisearch session --jsonl`, which remains a subprocess interface.

---

## 3. Users and Consumers

The primary consumer is an AI agent that can spawn subprocesses and parse structured output.

Secondary users are legal engineers, researchers, and maintainers who need a reproducible local legal search index.

The conception optimizes for:

- exact source provenance;
- stable machine-readable schemas;
- low token usage;
- repeatable search/fetch/cite loops;
- deterministic error semantics;
- no hidden legal reasoning inside the search tool.

---

## 4. Source Authority

The authoritative corpus is built from official sources only.

### Authoritative Sources

- **LEGI / DILA official XML:** codes, laws, regulations, article versions, hierarchy, validity windows, status, and identifiers.
- **Légifrance / PISTE API:** targeted lookups, deltas, and online citation confirmation.
- **Judilibre:** Cour de cassation decisions and structured metadata.
- **Justice administrative open data:** Conseil d'État, CAA, and TA decisions.
- **JORF:** official publication source, used where needed for provenance and published texts.

### Non-Authoritative Sources

Derived datasets such as `AgentPublic/legi` or `legalkit` are never authoritative ingestion sources.

They may be used only as:

- comparison fixtures;
- regression baselines;
- smoke-test data;
- external reference material for research.

They must never seed canonical records, chunks, embeddings, or production index content.

---

## 5. Canonical Legal Objects

The conceptual model is source-first. Internal objects preserve official identifiers, dates, hierarchy, and provenance.

### Document

A `Document` is either:

- a versioned statutory article; or
- a judicial/administrative decision.

Every document carries:

- a stable source-namespaced internal ID;
- official identifiers such as `LEGIARTI`, `LEGITEXT`, `LEGISCTA`, `JORFTEXT`, `NOR`, `ECLI`, `pourvoi`, or `CETATEXT`;
- a canonical citation;
- an official source URL;
- source provenance and build manifest references.

### Article Version

An article version is a statutory document with:

- hierarchy path;
- `valid_from`;
- `valid_to`;
- raw validity fields preserved for audit;
- status such as `VIGUEUR`, `MODIFIE`, `ABROGE`, or `ABROGE_DIFF`;
- a version group linking all temporal versions of the same article.

Open-ended validity is normalized conceptually as `valid_to = null`, while source sentinels remain preserved as provenance.

### Decision

A decision carries:

- court, chamber, formation, and date;
- publication and solution metadata;
- ECLI and decision-specific identifiers;
- full pseudonymised text;
- publisher-provided text zones when available;
- applied texts and related decisions as relationships, not zones.

### Chunk

A `Chunk` is the retrieval unit. It never crosses a legal structure boundary.

For statutes, the natural chunk is the article version or a structural sub-part of a long article.

For decisions, chunks follow official publisher text zones such as:

- introduction;
- visa / exposé;
- moyens;
- motivations;
- dispositif;
- moyens annexes;
- summary / sommaire.

`rapprochements` and applied texts are not chunks. They are relationship metadata.

When publisher zones arrive as multiple or non-sequential fragments, chunks are reassembled by **zone identity**, not by raw text position. When official offsets are absent, heuristic or regex splitting is allowed only as a flagged fallback: the chunk records boundary provenance such as `chunking: heuristic` so agents and evals know the boundary is approximate.

### Graph Edge

Graph edges represent legal relationships, for example:

- a decision applies an article;
- an article is interpreted by a decision;
- a decision cites another decision;
- an article version supersedes another version;
- a publisher provides a jurisprudence rapprochement.

Edges are evidence-bearing candidates. They do not assert legal conclusions such as *jurisprudence constante* by themselves.

---

## 6. Non-Negotiable Invariants

The following invariants define the product.

### Official Provenance

No indexed object exists without an official source identifier, source URL, and manifest trace.

### Temporal Correctness

Statutory retrieval is date-aware by construction. Historical queries must return the law applicable at the requested date, not current law accidentally matching the query.

### Structure Preservation

Chunks follow legal structure. Arbitrary fixed-size text splitting is not an authoritative retrieval model. Decision chunks preserve official zone identity, reassemble non-sequential fragments by that identity, and record boundary provenance when a heuristic fallback is used.

### Citation Verifiability

Every returned citation must be resolvable locally when possible and verifiable against official identifiers. Ambiguous, stale, fabricated, or missing citations must be surfaced explicitly.

### Relationship Discipline

Publisher metadata such as applied texts and jurisprudence rapprochements populate graph edges. They are not textual zones and are not chunked as passage text.

### Agent Token Discipline

Search returns compact candidates. Fetch returns full text only when requested. Cite verifies identifiers and citations separately.

### Runtime Boundary

Search, indexing, schema validation, ranking, CLI behavior, and query execution are Rust responsibilities. Python may exist only before the canonical-record boundary as offline ingestion assistance.

---

## 7. Retrieval Conception

Retrieval combines exact legal search, semantic search, temporal filtering, authority signals, and citation grounding.

The conceptual query flow is:

1. parse the agent query and filters;
2. optionally map lay vocabulary to formal legal terminology;
3. apply exact pre-filters such as kind, court, code, date, or jurisdiction;
4. retrieve lexical candidates using French legal BM25;
5. retrieve dense semantic candidates using embeddings;
6. fuse ranked candidates;
7. optionally rerank the fused set if benchmark results justify the latency;
8. apply authority-aware ranking signals;
9. return compact, citeable results.

Lexical retrieval is necessary for exact citations, article numbers, rare legal terms, and identifiers.

Dense retrieval is necessary for conceptual queries where the user asks in natural language rather than legal terminology.

Hybrid retrieval is the default because French legal research needs both.

Authority weighting is a ranking prior, not a hard filter. It may boost higher courts, publication levels, or recency when useful, but it must step aside when the agent asks for a specific court, jurisdiction, period, or local trend.

### Vocabulary and Query Expansion

Legal vocabulary mapping is a first-class retrieval concept. It maps lay or agent phrasing to formal French legal terminology and close synonyms, for example "virer un employé" to "licenciement" or "annuler un contrat" to "résolution", "nullité", and "résiliation" when the legal framing is ambiguous.

Expansion is conservative, offline-built, and grounded in curated legal vocabulary plus indexed corpus signals. It applies to the lexical leg only; dense retrieval already handles paraphrase. It is exposed both automatically through search when expansion is enabled and explicitly through `expand` so an agent can inspect candidate terms before searching. Expanded terms are reported back for transparency and must not rewrite exact identifiers or citations.

---

## 8. Index and Storage Conception

The selected storage conception is a single local embedded Postgres store managed by the CLI.

Postgres conceptually owns:

- documents;
- chunks;
- metadata;
- temporal columns;
- graph edges;
- manifests;
- eval traces;
- dense vectors through `pgvector`;
- lexical BM25 search through `pg_search`.

This is "embedded" in the product sense: `jurisearch` manages a local Postgres child process and local index directory. It is not a separate user-operated service.

`pg_search` is the selected lexical engine. `pgvector` is the selected vector store. Packaging/runtime fit is a validation gate, not an open product choice.

Fallbacks exist only on hard failure of the selected stack:

- native Postgres FTS if `pg_search` packaging fails;
- standalone Tantivy plus local vector/metadata storage if embedded Postgres itself fails;
- LanceDB only if the Postgres route fails both packaging and quality gates.

Qdrant and similar separate services are outside the conception.

---

## 9. Embeddings and Reranking Conception

Embeddings are produced through an OpenAI-compatible `/v1/embeddings` endpoint by default.

The endpoint may be:

- hosted remotely; or
- local on loopback, for example `llama.cpp` serving a dedicated embedding model.

In-process Rust embeddings are allowed as an optional offline backend, but they are not the default product assumption.

When the optional in-process backend is used, local models must be fetched explicitly before query time. Search and session calls fail with an actionable error rather than downloading models silently, unless the operator explicitly allows downloads.

The index records an embeddings fingerprint:

- provider;
- model;
- dimension;
- normalization;
- pooling;
- build date;
- schema version.

Query-time mismatches are hard errors, not silent degradation.

The default benchmark candidate is `BAAI/bge-m3`, but the final model is chosen by legal retrieval evals after hybrid fusion, not by standalone embedding benchmarks.

Reranking is benchmark-gated. If a reranker materially improves legal recall, nDCG, citation exactness, or stale-citation handling within latency and token budgets, it is part of the best-in-class release. If not, the release records the eval result and ships without it.

The reranker provider is conceptually pluggable:

- disabled;
- local;
- HTTP.

This prevents local Rust inference packaging from lowering the ranking-quality bar.

---

## 10. Agent Contract

The CLI contract is part of the product, not external documentation.

### Command Concepts

- `search`: returns compact ranked candidates.
- `fetch`: returns full source text for selected IDs.
- `cite`: verifies citations and identifiers.
- `related`: returns graph neighbours with authority signals.
- `context`: returns the structural neighbourhood of an object: ancestry path and sibling articles for codes, or neighbouring zones for decisions.
- `expand`: returns legal-vocabulary expansions for a query so the agent can inspect or choose terms.
- `status`: reports corpus coverage, freshness, model fingerprints, and index health.
- `help agent`: exposes the complete agent-facing contract.
- `help schema --json`: exposes machine-readable schemas.
- `session --jsonl`: supports warm multi-call agent workflows.

### Output Concepts

Outputs are structured and stable. JSON stdout is reserved for machine-readable results. Diagnostics belong on stderr.

The one-shot and JSONL session contracts are correlatable, order-preserving, and machine-readable: each request can be matched to its response, success and error shapes are stable, stdout carries structured output only, and diagnostics stay on stderr.

Results carry:

- stable IDs;
- canonical citations;
- snippets or full text depending on command;
- official URLs;
- source provenance;
- temporal validity;
- confidence and ambiguity metadata where relevant.

### Citation Verification States

`cite` returns explicit states:

- `exact`;
- `normalized`;
- `ambiguous`;
- `stale_version`;
- `not_found`;
- `source_unavailable`.

Strict mode treats anything other than `exact` or `normalized` as failure.

Citation verification is local-by-default for repeatability and privacy. Official APIs are used only when the agent/operator explicitly asks for online confirmation or when the local index reports that the source needed for verification is unavailable.

---

## 11. Temporal Semantics

Temporal correctness is central to the conception.

For statutes:

- `valid_from <= as_of`;
- `valid_to` is null or `as_of < valid_to`;
- historical versions remain searchable;
- modified and abrogated versions are preserved;
- current-law leakage into historical queries is unacceptable.

For decisions:

- the primary temporal field is decision date;
- publication metadata informs authority and freshness, not statutory validity;
- links to applied texts must resolve to the relevant article version when possible.

The default date is "current law today", but the agent can specify `--as-of` to reason at a historical date.

---

## 12. Legal Knowledge Graph

The graph is a bounded retrieval aid, not an autonomous legal-reasoning engine.

It supports one-hop and limited two-hop navigation:

- article → interpreting decisions;
- decision → applied articles;
- decision → cited or related decisions;
- decision → appeal chain where available;
- article version → predecessor/successor versions.

The graph returns ranked candidate material with authority signals. It must not claim that a doctrine is settled merely because related edges exist.

---

## 13. Quality Conception

"Best-in-class" is an evidence claim, not a marketing label.

The eval set must cover:

- realistic French legal research workflows;
- known article lookup;
- conceptual statutory retrieval;
- historical `as-of` queries;
- jurisprudence lookup by facts, issue, court, date, and citation;
- citation verification;
- stale or ambiguous citation handling;
- full agent loops such as `search → fetch → cite`.

The quality gates include:

- official-source fidelity;
- temporal accuracy;
- citation exactness;
- ranking quality;
- token efficiency;
- tool-call efficiency;
- reproducibility;
- coverage and freshness reporting;
- JSON/JSONL contract stability;
- inline help completeness.

No release may claim best-in-class status merely because it works on a sample corpus.

---

## 14. Security, Compliance, and Licensing

The project accepts AGPL-3.0, which makes `pg_search`/ParadeDB acceptable.

Data reuse is based on official French open-data terms and must preserve attribution, source URLs, and manifest traceability.

Pseudonymisation from source decisions must be preserved. The product must not attempt re-identification or cross-linking designed to reverse pseudonymisation.

Secrets such as API credentials and hosted embedding keys are configuration concerns and must not leak into outputs, logs, or index artifacts.

The tool frames itself as legal research infrastructure, not legal advice.

---

## 15. Phase Boundaries

### Phase 0 — Validation Foundation

This phase validates the selected architecture and source assumptions. It does not reopen product direction unless a hard criterion fails.

### Phase 1 — LEGI / Statutory Search

The release target is best-in-class statutory search over official LEGI XML:

- official source ingestion;
- structural article chunking;
- temporal search;
- hybrid retrieval;
- citation verification;
- complete CLI agent contract.

### Phase 2 — Full French Juridic Search

The release target expands to jurisprudence:

- Judilibre;
- justice administrative;
- official decision-zone chunking;
- graph relationships;
- authority-aware decision ranking;
- statutory + jurisprudence workflows.

### Phase 3+ — Expansion and Ranking Depth

Later phases may add:

- reranker refinements;
- learned sparse or ColBERT-style retrieval if evals justify it;
- EU law;
- KALI;
- BOFIP;
- doctrine.

These expansions must not weaken the core source, temporal, citation, and agent-contract invariants.

---

## 16. Conception Lock

The conception is locked on these product decisions:

- name: `jurisearch`;
- surface: CLI-only;
- agent discovery: complete inline help;
- warm operation: JSONL session;
- runtime/search/indexing path: Rust;
- Python: offline ingestion helpers only, before canonical-record validation;
- authoritative corpus: official XML / official APIs from day one;
- backend: embedded Postgres + `pgvector` + `pg_search`;
- embeddings: OpenAI-compatible endpoint default, including local loopback servers;
- derived datasets: comparison-only;
- Qdrant and external search/vector services: out of scope;
- best-in-class claim: eval-gated and phase-scoped;
- legal vocabulary expansion: retained as a foundation pillar and exposed through search / `expand`.

Remaining uncertainty is validation, not design indecision:

- selected backend packaging/runtime quality;
- embedding model winner under legal retrieval evals;
- reranker adoption under latency and quality gates.

---

## 17. Reference Trail

Local source documents:

- `work/00-foundation/search.md`
- `work/00-foundation/assessment.md`
- `work/01-design/DESIGN.md`
- `work/01-design/DECISIONS.md`
- `work/01-design/RESEARCH.md`
- `work/reviews/2026-06-20-lock-readiness-review.md`
- `work/reviews/2026-06-20-conception-review.md`
- `work/reviews/2026-06-20-conception-readiness-review.md`

Primary external references checked on 2026-06-20:

- Légifrance open data and API: https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api
- LEGI official dataset: https://www.data.gouv.fr/datasets/legi-codes-lois-et-reglements-consolides
- Judilibre API: https://www.data.gouv.fr/dataservices/api-judilibre
- `pgvector`: https://github.com/pgvector/pgvector
- `pg_search`: https://pgxn.org/dist/pg_search/
- `llama.cpp` server embeddings: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
