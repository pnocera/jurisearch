# Direction-lock review: `jurisearch`

Review date: 2026-06-20

Reviewed files:

- `work/01-design/README.md`
- `work/01-design/DESIGN.md`
- `work/01-design/DECISIONS.md`
- `work/01-design/RESEARCH.md`

User decisions applied in this review:

- **D3:** backend is **embedded Postgres + `pgvector` + `pg_search`**.
- **D5:** query embeddings are **remote endpoint by default**. "Remote" includes a local/self-hosted HTTP endpoint such as `llama.cpp` serving OpenAI-compatible `/v1/embeddings`.
- **D7:** corpus source is **official XML from day 1**. No HF bootstrap path for the authoritative build.
- Product ambition: **not an MVP, not a toy**. Target is the **best-in-class French juridic search engine for AI agents**.

## Verdict

The design is close, but it still reads like a pragmatic MVP plan with three open decisions. Those are now decided. The next revision should remove the ambiguity and reframe the project as a production-grade, official-source-first search engine.

The legal architecture is strong. The remaining blockers are product-direction issues, not conceptual search issues.

## Findings

### P0: D3/D5/D7 must be moved from open to decided

`DECISIONS.md` still lists D3, D5, and D7 as open. `DESIGN.md` and `README.md` still repeat those open questions.

Update them as follows:

- **D3 decided:** embedded Postgres + `pgvector` + `pg_search` is the chosen backend. The spike now validates packaging, performance, and operational limits; it no longer chooses among LanceDB/Qdrant/native FTS/Tantivy as peers.
- **D5 decided:** default embedding provider is an HTTP embeddings endpoint. It may be a hosted API or a local/self-hosted endpoint, including `llama.cpp` serving OpenAI-compatible `/v1/embeddings`.
- **D7 decided:** official DILA/LEGI XML from day 1. `AgentPublic/legi` can remain a reference dataset, smoke-test fixture, or benchmark comparison, but not a bootstrap path for the authoritative index.

The open-question summary should be deleted or replaced with remaining benchmark/eval gates.

### P0: Remove “MVP” and “fast demo” framing

The current docs still say "MVP", "prove the core", and "fast demo". That conflicts with the product goal.

Use phase names that reflect seriousness:

- **Phase 0: Backend and ingestion validation** — packaging, official XML parser, schema validation, canonical records, evaluation harness.
- **Phase 1: Production-quality LEGI search** — official LEGI XML, temporal correctness, citation verification, CLI/JSONL contract, Postgres+vector+BM25 index, eval gates.
- **Phase 2: Full jurisprudence coverage** — Judilibre, justice administrative, zone-aware decisions, graph relationships, authority weighting.
- **Phase 3: Best-in-class ranking** — reranker, learned sparse/ColBERT if useful, tuned legal evals, agent workflow benchmarks.

The important change is not wording polish. It prevents shortcuts like importing HF chunks, skipping official XML edge cases, or accepting "works on a sample" as a milestone.

### P0: Official XML must be the first authoritative ingestion path

The design currently allows `AgentPublic/legi` bootstrap and says official XML happens in parallel. That should be removed.

Required replacement:

- Build the official LEGI XML parser first.
- Preserve raw source identifiers, dates, hierarchy, status, links, and source provenance.
- Emit canonical records from official XML only for authoritative builds.
- Use PISTE/API for targeted lookup, deltas, and citation confirmation.
- Use derived datasets only as non-authoritative comparison fixtures.

This is necessary for a best-in-class legal engine. The first ingestion pipeline defines every downstream invariant: temporal correctness, hierarchy paths, citation graph edges, source freshness, and reproducibility.

### P0: Embeddings should be endpoint-first, not in-process-first

`DESIGN.md` still recommends local Rust `fastembed-rs` as preferred, with remote optional. D5 is now the opposite: endpoint default.

Recommended model:

- `embedding.provider = "openai_compatible"` by default.
- `embedding.base_url` can point to OpenAI/Voyage/Cohere/Mistral-compatible services, or to local `llama.cpp`/other self-hosted embedding servers.
- `embedding.model`, vector dimension, normalization behavior, pooling mode, and provider fingerprint are recorded in the index manifest.
- Document embeddings and query embeddings must use the same configured provider/model fingerprint unless the index explicitly declares a migration.
- In-process Rust embeddings remain an optional backend for offline/single-binary setups, not the primary design path.

This keeps query runtime simpler, aligns with external embedding servers, and lets local `llama.cpp` satisfy privacy/offline requirements without coupling search quality to Rust inference packaging.

### P1: `llama.cpp` endpoint support should be explicit and constrained

The design should add a concrete local endpoint profile:

```toml
[embedding]
provider = "openai_compatible"
base_url = "http://127.0.0.1:8080/v1"
model = "bge-m3-or-other-embedding-model"
api_key = "no-key"
```

Contract requirements:

- Support OpenAI-style `POST /v1/embeddings`.
- Require dedicated embedding models, not chat models used casually as embedders.
- Record vector dimension and normalization behavior.
- Fail fast if returned vector dimension differs from the index.
- Treat local HTTP as "remote provider" from the CLI perspective, even when hosted on `127.0.0.1`.

Research note: llama.cpp server docs expose OpenAI-compatible `POST /v1/embeddings`, with pooling requirements; that makes it a viable local endpoint provider.

### P1: Backend alternatives should become fallbacks, not active design options

The current backend section still says "build and compare three prototypes". With D3 decided, keep only one primary backend:

- Embedded Postgres data directory.
- `pgvector` for dense vectors.
- `pg_search` for BM25/full-text.
- Postgres tables for documents, chunks, metadata, graph, manifests, eval traces.

Keep native Postgres FTS, standalone Tantivy, LanceDB, and no-Postgres layouts as documented fallbacks only if the chosen backend fails hard acceptance criteria. Qdrant remains out of scope.

The backend spike should validate:

- local child-process packaging;
- pinned Postgres/`pgvector`/`pg_search` versions;
- startup/shutdown/recovery;
- socket/loopback binding;
- index migration;
- warm query latency;
- temporal prefilter behavior;
- BM25 quality with French legal tokenization;
- hybrid fusion quality.

It should not reopen the product decision unless the chosen stack fails.

### P1: “Best-in-class” needs measurable acceptance criteria

The docs say evaluation matters, but the ambition now requires sharper quality gates.

Add explicit best-in-class criteria:

- **Official-source fidelity:** every result traceable to official source ID, source URL, source version, and build manifest.
- **Temporal accuracy:** no current-law leakage into historical `--as-of` queries in the eval set.
- **Citation exactness:** fabricated, ambiguous, stale, and malformed citations are rejected or disambiguated.
- **Agent efficiency:** search→fetch→cite workflows stay within strict token and call budgets.
- **Ranking quality:** benchmark BM25, dense, hybrid, authority, and reranker variants on French legal tasks.
- **Corpus coverage:** status reports exact coverage for LEGI, Judilibre, and justice administrative, with freshness.
- **Operational reproducibility:** rebuild index from official inputs and manifest.

Without these gates, "best-in-class" is only a slogan.

### P1: Reranker staging should be rephrased

The design currently says the MVP ships without a reranker and reranker comes in v1.1. For best-in-class, this should be framed differently.

Recommended wording:

- Phase 1 may launch without a reranker only if evals prove hybrid+authority meets the quality bar.
- Reranker is not a nice-to-have roadmap item; it is a benchmark-gated ranking component.
- If reranking materially improves legal recall/nDCG/citation exactness within latency budgets, it belongs before the first "best-in-class" release.

This preserves engineering pragmatism without lowering the product ambition.

### P2: `AgentPublic/legi` should be demoted in docs

The previous review already fixed "do not import HF chunks as final chunks." With D7 decided, go further:

- Remove HF bootstrap from README open questions.
- Remove HF bootstrap from roadmap.
- Keep `AgentPublic/legi` only in research notes as a derived dataset useful for comparison/smoke tests.
- Do not let HF precomputed embeddings influence the default embedding model decision.

This avoids confusing agents and future implementers about the source of truth.

## Concrete Documentation Changes

1. Change `DECISIONS.md` D3 to **DECIDED: embedded Postgres + `pgvector` + `pg_search`**.
2. Change `DECISIONS.md` D5 to **DECIDED: remote/OpenAI-compatible endpoint default**, with local `llama.cpp` endpoint explicitly supported.
3. Change `DECISIONS.md` D7 to **DECIDED: official XML from day 1**.
4. Delete the open-questions summary or replace it with "remaining validation gates".
5. Replace all "MVP", "fast demo", and "prove the core" language with production-quality phases.
6. Remove `AgentPublic/legi` as a bootstrap path from `README.md`, `DESIGN.md`, and `DECISIONS.md`.
7. Change `DESIGN.md` §11/§13/§14 from in-process local embeddings preferred to endpoint-first embeddings.
8. Add a `llama.cpp` local endpoint profile and vector-dimension validation rule.
9. Change backend spike text from choosing a backend to validating the decided backend.
10. Add best-in-class acceptance criteria to the evaluation section.

## Research Notes

- llama.cpp server supports OpenAI-compatible `POST /v1/embeddings`; the endpoint requires a pooling mode other than `none`: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
- `pg_search` provides BM25 full-text search inside Postgres and is built on Tantivy: https://pgxn.org/dist/pg_search/
- `pgvector` stores vectors inside Postgres and supports exact/approximate nearest-neighbor search: https://github.com/pgvector/pgvector
- `AgentPublic/legi` is useful context but derived; it contains `chunk_text` and stringified BGE-M3 embeddings, with long articles chunked by `RecursiveCharacterTextSplitter`: https://huggingface.co/datasets/AgentPublic/legi

## Bottom Line

The design should now stop presenting itself as an MVP path with open choices. Lock the stack and source policy:

- official XML first;
- embedded Postgres + `pgvector` + `pg_search`;
- endpoint-based embeddings by default, including local `llama.cpp`;
- production-grade quality gates;
- no toy demo path.

That aligns the design with the stated goal: the best French juridic search engine for AI agents, not a quick RAG prototype.
