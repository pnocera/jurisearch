# Lock-readiness review — updated `jurisearch` design

Date: 2026-06-20  
Scope: review of `work/01-design/*` before locking the design and writing a **conception** document.

## Verdict

The design is now **sound enough to lock after a small textual cleanup pass**. I would not reopen the architecture: the major product decisions are coherent and aligned with the stated goal of a best-in-class French juridic search engine for AI agents.

Do **not** lock it with the current wording unchanged, because there is one conceptual inconsistency left in `DECISIONS.md` around Judilibre zones vs relationships. This is not an architecture blocker, but it should be corrected before the conception document becomes the reference.

## External assumption check

The current design direction still matches the external technical/source facts checked on 2026-06-20:

- **Official sources:** Légifrance/DILA documents that Légifrance data is available for free reuse and that the API is stable; the FAQ confirms bulk XML open data and lists `LEGI` among the available fonds. This supports "official XML from day 1" as the correct source policy.
- **`pgvector`:** the project positions itself as vector similarity search inside Postgres with exact and approximate nearest-neighbor support, which supports the unified Postgres store decision.
- **`pg_search` / ParadeDB:** PGXN describes `pg_search` as BM25 full-text search over Postgres heap tables, built on Tantivy/pgrx and supported on official Postgres versions starting at v14. This supports using it as the selected lexical engine, subject to packaging validation.
- **Embedded Postgres:** `pg-embed` runs a local PostgreSQL server from Rust, downloads/caches binaries, and manages lifecycle. This confirms that "embedded Postgres" means a managed local process, not an in-process database.
- **Local embedding endpoint:** `llama.cpp` documents an OpenAI-compatible `/v1/embeddings` endpoint with a pooling requirement. The endpoint-first design, including local loopback serving, is sound if model/dimension/pooling fingerprints are enforced.
- **`AgentPublic/legi`:** the dataset is derived from official DILA/LEGI data, pre-chunked and embedded with `BAAI/bge-m3`. That makes it useful as a comparison/regression fixture, but not as an authoritative source.

References:
- https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api
- https://www.legifrance.gouv.fr/contenu/pied-de-page/foire-aux-questions-api
- https://github.com/pgvector/pgvector
- https://pgxn.org/dist/pg_search/
- https://github.com/faokunega/pg-embed
- https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
- https://huggingface.co/datasets/AgentPublic/legi

## Lock-readiness assessment

### What is now solid

- **Product identity:** `jurisearch` is the right primary name. It is explicit enough for agents scanning command inventories.
- **Runtime boundary:** Rust owns CLI/search/indexing/query execution. Python is acceptable only as offline ingestion tooling that emits canonical artifacts.
- **Transport:** CLI-only with one-shot JSON and JSONL session mode is coherent. No MCP, no HTTP daemon, no Qdrant/service dependency.
- **Source policy:** official DILA/LEGI XML from day 1 is correct. Derived datasets remain comparison-only.
- **Backend direction:** embedded Postgres + `pgvector` + `pg_search` is a coherent single-store design for documents, chunks, temporal metadata, graph edges, lexical search, and vectors.
- **Quality posture:** the design now treats "best-in-class" as an eval-gated claim, not an MVP slogan.
- **Agent contract:** inline help, `help agent`, and `help schema --json` are correctly treated as product surface, not nice-to-have documentation.
- **Legal correctness model:** temporal filtering, citation verification, source provenance, structural chunking, and relationship edges are the right invariants for French legal search.

## Findings before lock

### P1 — Fix the remaining zones/relationships contradiction

`work/01-design/DECISIONS.md:60` still says Judilibre chunking zones include `rapprochements`. This conflicts with `D15` and `work/01-design/DESIGN.md:229`, which correctly state that `rapprochements` and applied texts are relationships/metadata, not text zones.

Required fix before lock:

- Remove `rapprochements` from the D10 zone list.
- Phrase D10 as: official text zones such as `introduction`, `visa`, `moyens`, `motivations`, `dispositif`, `moyens annexes`, `summary/sommaire`; `rapprochements` and applied texts populate graph edges.

This matters because the conception document must not carry two incompatible domain models for the same source.

### P2 — Rename stale "open questions" language

`work/01-design/DECISIONS.md:1` still says "Design decisions & open questions", while the body says no product questions remain open. Before lock, rename the framing to something like:

- `Design decisions & validation gates`

Also update the introductory wording around "three reviews" now that the design has been reviewed more times. This is documentation drift, not an architectural problem.

### P2 — Make selected-backend wording consistent

The design mostly says the backend is decided, but a few places still use softer wording:

- `work/01-design/DESIGN.md:244`: `pg_search` is called the "preferred candidate".
- `work/01-design/DESIGN.md:505`: `pg_search` is called "preferred".
- `work/01-design/DESIGN.md:634` and `work/01-design/RESEARCH.md:164`: packaging/runtime fit is described as an "open question".

Recommended wording:

- `pg_search` is the selected lexical engine inside the selected Postgres backend.
- Packaging/runtime fit is a validation gate.
- Native Postgres FTS / Tantivy are fallbacks only on hard failure.

### P2 — Remove stale HF bootstrap caveat

`work/01-design/RESEARCH.md:150` still says to confirm licensing if bootstrapping from third-party corpora. The design no longer allows bootstrapping the authoritative index from HF or other derived corpora.

Recommended wording:

- Confirm licensing/provenance only when using derived datasets as comparison, regression, or smoke-test fixtures.
- No derived corpus may seed canonical records, chunks, or embeddings.

### P2 — Keep the conception document conceptual

The next document should be a **conception** document, not an implementation plan. The current design contains some implementation-plan material (`crate layout`, spike mechanics, packaging criteria). That is fine in `DESIGN.md`, but the conception document should not become a task list.

The conception document should define:

- purpose, non-goals, and phase claim boundaries;
- source authority and provenance rules;
- canonical legal objects and relationships;
- temporal and citation correctness model;
- chunking/indexing/retrieval concepts;
- agent-facing CLI contract at the conceptual level;
- quality gates required to justify "best-in-class";
- security, licensing, and compliance posture.

It should avoid:

- crate/module breakdowns;
- ticket order or milestones;
- build scripts and packaging steps;
- spike task lists;
- implementation-specific commands except interface examples needed to define the agent contract.

## Answer to the lock question

Yes: the design is **architecturally sound now**. I would lock the design after fixing the P1 inconsistency and the P2 stale wording above.

The lock should say:

- the product is `jurisearch`;
- the target is a production-grade, best-in-class French juridic search engine for AI agents, not an MVP or toy;
- the interface is CLI-only with complete inline help;
- the runtime/search path is Rust;
- Python is offline-ingestion-only when it materially improves official-source parsing;
- authoritative ingestion starts from official XML;
- the selected backend is embedded Postgres + `pgvector` + `pg_search`;
- endpoint-based embeddings are the default, including local `llama.cpp` when serving a dedicated embedding model;
- no Qdrant or external search/vector service is part of the design;
- all remaining uncertainty is handled as validation gates, not product indecision.

## Recommended next step

Apply the small wording cleanup, then write `work/01-design/CONCEPTION.md` as the locked conceptual reference. Keep `DESIGN.md` as the richer design/research record, and create an implementation plan only after the conception document is accepted.
