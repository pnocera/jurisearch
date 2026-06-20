# Research appendix — evidence behind `DESIGN.md`

Web research conducted 2026-06-20 to ground the design. Findings are summarized; primary URLs listed per section. Verify API specifics against live docs at build time (endpoints/coverage dates move).

---

## 1. French legal data sources & APIs

### Légifrance / PISTE API (DILA)
- DILA exposes French legal data through the **PISTE** portal, free after registration. Auth is **OAuth2 client-credentials**.
- Endpoints: production API `https://api.piste.gouv.fr/dila/legifrance/lf-engine-app`; OAuth token `https://oauth.piste.gouv.fr/api/oauth/token`; sandbox equivalents under `sandbox-api.piste.gouv.fr` / `sandbox-oauth.piste.gouv.fr`. Swagger docs on PISTE.
- Covers codes, lois, JORF, and jurisprudence; metadata updated daily.
- Free reuse established by the **décret of 24 June 2014** (DILA legal databases) under **Licence Ouverte / Etalab**.
- Python client **`pylegifrance`** wraps auth + queries (Pydantic models); there is also a `mcp-server-legifrance`.

### Bulk open data (preferred for full build)
- **LEGI** ("Codes, lois et règlements consolidés") published on **data.gouv.fr** and the DILA exchange repo (`echanges.dila.gouv.fr`), **XML** format.
- Temporal fields per article version: **`dateDebut`** (entry into force) and **`dateFin`** (termination/supersession). Statuses: **`VIGUEUR`** (in force), **`MODIFIE`**, **`ABROGE`** (repealed), **`ABROGE_DIFF`** (deferred repeal, still valid until end date).
- Article IDs: **`LEGIARTI…`** (articles), `LEGITEXT…` (texts), `LEGISCTA…` (sections). JORF: `JORFTEXT…`, `JORFARTI…`. `NOR` numbers identify originating texts.
- Derived HF datasets exist for reference: **`AgentPublic/legi`** (≈629k rows, Etalab, LEGI-like metadata, `chunk_text` + BGE-M3 vectors) and **louisbrulenaudet `legalkit`** per-code datasets + `legalkit-pipeline`. **Non-authoritative (D7/D14):** since the authoritative index is built from official DILA/LEGI XML only, these are useful **for smoke tests and regression comparisons** but are **never ingestion inputs** — their chunks/embeddings (LangChain `RecursiveCharacterTextSplitter` 1024-char splits, stringified BGE-M3 vectors) must **not** influence model or chunking decisions (DESIGN §4, §13.4).

### Judilibre (Cour de cassation)
- Open database of publicly delivered decisions, exposed as a **REST API over HTTPS** on PISTE; specs are open-source (`Cour-de-cassation/openjustice-specs`, `judilibre-search`).
- `GET /decision` returns full content + metadata: jurisdiction, chamber, formation, appeal number (pourvoi), **ECLI**, date, publication level, solution. `/search`, `/export`, `/taxonomy`, and **`/transactionalhistory`** (create/update/delete log → clean **incremental sync** for a local mirror).
- The public OpenAPI spec separates two things the design must model differently: (a) **text zones** — *introduction, exposé/visa, moyens, motivations, dispositif, moyens annexes, summary (sommaire)* — as **character offsets** into the full text (fragments **can be non-sequential**, so chunking reassembles by zone identity, not text order); and (b) **decision metadata** including **applied texts** and **jurisprudence rapprochements**. The latter two are **relationships, not zones** → graph edges (`applies_article`, `rapprochements`), never chunked (DESIGN §5.1, §6, §8). Publication/solution arrive as **raw taxonomy keys** (e.g. publication `b/r/l/c`) that the design normalizes while preserving the raw values.
- Coverage timeline: **Cour de cassation since 2021-09-30**; first-instance penal decisions from 2024-12-31; appeal-court decisions rolling out through 2025. Decisions are **pseudonymised**.

### Conseil d'État / justice administrative
- **`opendata.justice-administrative.fr`**: all **Conseil d'État** decisions since 2021-09-30, **9 CAA** (cours administratives d'appel) since 2022-03-31, **42 TA** (tribunaux administratifs) since 2022-06-30, in **XML**. CE decisions published next day (Tue–Sat).
- **ArianeWeb** holds older / jurisprudentially-significant decisions (250k+), non-exhaustive. Decision IDs **`CETATEXT…`** + ECLI.

### Identifiers
- **ECLI** (European Case Law Identifier) is the cross-court stable id for decisions (Judilibre + justice administrative).
- Statutory ids: `LEGIARTI`/`LEGITEXT`/`LEGISCTA`; published-text ids `JORFTEXT`/`NOR`; admin decisions `CETATEXT`.

### Prior art (positioning)
- **`pylegifrance`** + `mcp-server-legifrance` (open source, thin API wrapper).
- **`mcp-juridique.fr` / MCP Factory** — commercial, ~17 MCP servers / 120+ tools over Légifrance, JORF, LODA, JURI, Judilibre, KALI, BOFIP, SIRENE, etc.; semantic search; €40/mo. Positions against "static RAG".
- **Gap / differentiation for `jurisearch`:** existing tools are mostly **API wrappers** (several are MCP servers over Légifrance). None bundle structure-/zone-aware chunking + temporal time-travel + hybrid retrieval + legal knowledge graph + citation verification as a **local-first, Rust, CLI-only agent contract**. That is the foundation document's thesis.

**Sources:**
- https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api
- https://www.dila.gouv.fr/home/open-data-et-api
- https://piste.gouv.fr/api-catalog-sandbox
- https://www.data.gouv.fr/dataservices/legifrance
- https://www.data.gouv.fr/datasets/legi-codes-lois-et-reglements-consolides
- https://huggingface.co/datasets/AgentPublic/legi
- https://github.com/SocialGouv/legi-data
- https://www.data.gouv.fr/dataservices/api-judilibre
- https://github.com/Cour-de-cassation/openjustice-specs
- https://github.com/Cour-de-cassation/judilibre-search
- https://github.com/Cour-de-cassation/judilibre-search/blob/dev/public/JUDILIBRE-public.json
- https://opendata.justice-administrative.fr/
- https://www.conseil-etat.fr/decisions-de-justice/donnees-ouvertes-open-data
- https://fondamentaux.org/2021/open-data-des-decisions-de-justice-et-api-des-cours-francaises-et-europeennes/
- https://pylegifrance.github.io/pylegifrance/
- https://github.com/pylegifrance/mcp-server-legifrance
- https://github.com/louisbrulenaudet/legalkit-pipeline
- https://mcp-juridique.fr/mcp-juridique

---

## 2. French legal embedding / encoder models

- **`BAAI/bge-m3`** — multilingual (incl. strong French), max ~8192 tokens, 1024-dim dense, and produces **dense + learned-sparse (+ ColBERT multi-vector)** from one model → ideal for hybrid. Recommended default dense model.
- **`Lajavaness/sentence-camembert-large`** — French sentence embeddings (CamemBERT-large base, fine-tuned), updated 2025; strong on French STS. Good French-specialist alternative. Also `dangvantuan/sentence-camembert-base/large`.
- **`OrdalieTech/Solon-embeddings-large`** and other MTEB-French leaders — French-tuned options to benchmark.
- **JuriBERT** — smaller French legal-domain BERT (encoder); useful as a domain signal / fine-tuning base, not a turnkey sentence embedder.
- **Serving (decided, D5):** embeddings are consumed over an **OpenAI-compatible `/v1/embeddings` HTTP endpoint** by default — a hosted API (Voyage/Cohere/Mistral/OpenAI) *or* a **local self-hosted server** such as `llama.cpp` on loopback (privacy/offline without coupling search quality to Rust inference packaging). `llama.cpp`'s server exposes `POST /v1/embeddings` and **requires a pooling mode other than `none`**; use a dedicated embedding model, not a chat model. In-process Rust (`fastembed-rs`) remains an optional offline backend. The chosen model/dimension/normalization/pooling are pinned in the index fingerprint with a hard dimension check (DESIGN §11.2).
- **MTEB-French** is the benchmark to consult for current French embedding rankings.

**Sources:**
- https://huggingface.co/BAAI/bge-m3
- https://huggingface.co/Lajavaness/sentence-camembert-large
- https://arxiv.org/html/2405.20468v2  (MTEB-French)
- https://huggingface.co/docs/transformers/model_doc/camembert
- https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md  (OpenAI-compatible `/v1/embeddings`, pooling requirement)

---

## 3. Reranking, sparse retrieval & hybrid fusion

### Rerankers (cross-encoders)
- **`BAAI/bge-reranker-v2-m3`** — multilingual cross-encoder built on bge-m3, ~0.6B params, 512-token pairs, fast; recommended for French rerank. Cross-encoders consistently beat bi-encoders on final ordering; run only on the fused top-K.
- Alternatives: Cohere Rerank multilingual, Jina reranker v2 multilingual, mxbai rerank.
- **Provider abstraction (design):** reranking is pluggable — local (`ort`/Candle) **or** an HTTP rerank endpoint (incl. hosted Cohere/Jina) — so the quality bar is not blocked by local Rust inference packaging; adopt only through the eval adoption gate (DESIGN §7.2, §14).

### Sparse / lexical
- **BM25** remains essential for exact statutory references and rare legal terms; **Tantivy** (Rust, Lucene-class) is the embeddable engine of choice and underlies LanceDB FTS.
- French analysis needs: lowercasing, careful accent folding, **elision** splitting (`l'`/`d'`/`qu'`), light lemmatization, legal stopwords.
- **Learned sparse** (SPLADE, or bge-m3's sparse output) is a Phase 3 upgrade.

### Hybrid fusion
- **Reciprocal Rank Fusion (RRF)** is the robust default: fuses by **rank**, sidestepping the fact that BM25 and cosine scores live on different, per-query-shifting scales. Normalization-based weighting (alpha) and DBSF are alternatives.
- Sparse + dense are complementary: sparse = exact token match, dense = semantic match — combine for best recall (Qdrant, LanceDB guidance).

**Sources:**
- https://huggingface.co/BAAI/bge-reranker-v2-m3
- https://qdrant.tech/articles/hybrid-search/
- https://qdrant.tech/documentation/search/hybrid-queries/
- https://app.ailog.fr/en/blog/guides/hybrid-search-rag

---

## 4. Designing tools for LLM agents (Anthropic guidance)

From Anthropic, *"Writing effective tools for AI agents"* and *"Effective context engineering"*:
- **Token efficiency:** implement **pagination, range selection, filtering, truncation** with sensible defaults. Claude Code caps tool responses at **~25,000 tokens** by default. Truncation messages should *steer* the agent toward targeted searches.
- **Verbosity control:** add a **`response_format`** enum (e.g. `concise` vs `detailed`) so agents choose context cost.
- **Identifiers:** prefer **natural-language identifiers** (`name`, `file_type`) over opaque UUIDs/`mime_type`; expose technical IDs only in detailed responses. Reduces hallucination.
- **Errors:** make them **actionable** — show correct input format / examples, not opaque codes; nudge toward token-efficient strategies (many small searches vs one broad).
- **Tool descriptions:** write like onboarding a new teammate; clear parameter names (`user_id` not `user`); refine iteratively against evals.
- **Consolidation & namespacing:** combine related ops to mirror how a human subdivides a task; namespace tools (`service_resource_action`) to avoid confusion.
- **Evaluation:** build realistic multi-call tasks with verifiers; track accuracy, tokens, tool-call count, latency, errors; use held-out sets; read agent transcripts for confusion.

**Transport — decided CLI-only (design review, 2026-06-20):** this design ships **one Rust core behind a CLI only** — one-shot `--json` plus a JSONL **session** mode for warm multi-call use. **No MCP, no HTTP.** With no MCP tool descriptions, discovery moves into the binary: `jurisearch help agent` + `jurisearch help schema --json` *are* the contract. The reusable agent-ergonomics principles above still apply (concise default, pagination, stable IDs, natural citations, actionable errors), but they are framed for **any subprocess-spawning harness**, not one ecosystem. Two-step **search → fetch** (cheap search returns ids+snippets, separate fetch returns full text) remains the canonical context-frugal retrieval pattern.

**Sources:**
- https://www.anthropic.com/engineering/writing-tools-for-agents
- https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents

---

## 5. Local-first index backends (decided: embedded Postgres + `pgvector` + `pg_search`)

The CLI-only + Rust-runtime decision reshapes this section. The backend must be **embeddable, driven from Rust, and produce a single local artifact** — and it is now **selected, not open** (D3): the spike in DESIGN §13.3 **validates packaging/quality**; fallbacks engage only on hard failure.

- **Embedded Postgres** (`pg-embed` / `postgresql_embedded`) — **the selected backend.** One local relational store for documents, chunks, metadata, temporal columns, graph edges, manifests, and eval traces, started as a local process from Rust. Paired with:
  - **`pgvector`** for dense vectors (exact/approx NN, multiple types + distance metrics);
  - **`pg_search`/ParadeDB** (Tantivy/BM25 inside Postgres) for lexical — viable because **AGPL-3.0 is acceptable**.
- **Fallbacks (hard-failure only, in precedence):** native Postgres **FTS** (if `pg_search` packaging fails); **standalone Tantivy** (Rust, Lucene-class — max French-tokenizer control) + a local Rust vector index + SQLite/Parquet/Arrow (if embedded Postgres itself fails); **LanceDB** (Rust SDK; vector + metadata + FTS + SQL + RRF, but model-based rerankers are Python-only) only if the Postgres route fails both packaging *and* quality.
- **Qdrant — out of scope.** A separate vector/search **service** conflicts with the embedded CLI shape; revisit only if every embedded option fails.
- **FAISS** (vectors only) / **Meilisearch / Typesense** (server engines) / **sqlite-vec + FTS5** (lighter, but weaker hybrid): not a fit for the embedded local-first design.

**Sources:**
- https://docs.rs/pg-embed
- https://github.com/theseus-rs/postgresql-embedded
- https://github.com/pgvector/pgvector
- https://github.com/paradedb/paradedb
- https://docs.rs/lancedb
- https://docs.lancedb.com/reranking
- https://github.com/quickwit-oss/tantivy
- https://qdrant.tech/articles/bm42/

---

## 6. Notes / caveats for build time
- Judilibre and justice-administrative **coverage dates advance**; re-check current ranges before claiming completeness in `jurisearch status`.
- PISTE has **rate limits**; rely on bulk dumps for full builds, API for deltas + live citation verification.
- Confirm **licensing/provenance** only when using **derived datasets as comparison / regression / smoke-test fixtures** (official DILA/Etalab is permissive; re-published derivatives may differ). **No derived corpus may seed canonical records, chunks, or embeddings** — the authoritative index is official-XML-only (D7/D14).
- Pseudonymisation must be **preserved**, never reversed (§16 of DESIGN).
- **Reranker in Rust is a spike, not a given:** confirm model availability, tokenizer behaviour, ONNX/Candle compatibility, latency, and packaging before adopting (benchmark-gated; DESIGN §7.2 / D11).
- **Keep Python out of the runtime:** Python may only emit canonical records offline; a Rust test must prove indexing + search with no Python in the loop (DESIGN §13.4).

---

## 7. Rust search/runtime tooling (design-review basis)

The CLI-only + Rust decision rests on a viable Rust ecosystem:
- **Tantivy** — Rust full-text search (Lucene-inspired): BM25, configurable tokenizers, incremental indexing, mmap, facets, range queries, tiny startup. Backs `pg_search` and is the standalone-lexical fallback.
- **LanceDB Rust SDK** — local persistent vector search + metadata + FTS + SQL, native Rust; reranker coverage differs by SDK (model rerankers Python-only).
- **`pg-embed` / `postgresql_embedded`** — run a local PostgreSQL process from a Rust app (Linux/macOS/Windows); the basis of the embedded-Postgres path.
- **`pgvector`** — vectors in Postgres: exact/approx NN, multiple vector types + distance metrics.
- **`pg_search`/ParadeDB** — Tantivy/BM25-style FTS inside Postgres; AGPL-3.0 (acceptable here); the **selected** lexical engine — packaging/runtime fit is a validation gate, not a licensing question.
- **`fastembed-rs`** — local ONNX inference incl. **BGE-M3** joint dense/sparse/ColBERT embeddings; default for in-process query embeddings.
- **`ort`** — Rust bindings for ONNX Runtime, for lower-level model control (and a reranker path).
- **Candle** — native Rust ML inference (CPU/CUDA/WASM), an alternative inference backend.
- **`clap`** — Rust CLI parsing: subcommands, value enums, generated help, completions — backs the inline-help contract (DESIGN §10.4).

**Sources:**
- https://github.com/quickwit-oss/tantivy
- https://docs.rs/lancedb
- https://docs.lancedb.com/reranking
- https://docs.rs/pg-embed
- https://github.com/theseus-rs/postgresql-embedded
- https://github.com/pgvector/pgvector
- https://github.com/paradedb/paradedb
- https://pgxn.org/dist/pg_search/
- https://github.com/Anush008/fastembed-rs
- https://docs.rs/ort
- https://huggingface.github.io/candle/
- https://docs.rs/clap
