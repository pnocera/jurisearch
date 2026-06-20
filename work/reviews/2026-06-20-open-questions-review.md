# Open-question review: `jurisearch`

Review date: 2026-06-20

Reviewed files:

- `work/01-design/README.md`
- `work/01-design/DESIGN.md`
- `work/01-design/DECISIONS.md`
- `work/01-design/RESEARCH.md`

## Verdict

The updated design is now directionally coherent. The former product questions are correctly closed:

- Backend: embedded Postgres + `pgvector` + `pg_search`.
- Embeddings: OpenAI-compatible endpoint by default, including local `llama.cpp`.
- Corpus: official DILA/LEGI XML from day 1.
- Ambition: production-grade best-in-class French juridic search for AI agents, not an MVP.

The remaining "open questions" are really validation gates. Below are the answers I would proceed with.

## Answers

### 1. Backend Spike

**Answer:** proceed with embedded Postgres + `pgvector` + `pg_search` as the chosen backend. The spike validates this stack; it should not reopen backend selection unless a hard criterion fails.

Expected stack:

- Postgres child process managed by Rust.
- `pgvector` for dense vectors.
- `pg_search` for BM25/full-text search.
- Postgres tables for documents, chunks, metadata, graph edges, manifests, and eval traces.
- Custom Rust RRF and authority scoring above the DB layer.

Acceptance criteria should remain strict:

- pinned Postgres, `pgvector`, and `pg_search` versions;
- no public network exposure by default;
- clean startup/shutdown/crash recovery;
- single-writer locking;
- schema/index migration story;
- warm query JSON under 500 ms for common queries;
- correct temporal prefiltering at corpus scale;
- French legal BM25 quality for elisions, accents, statutory refs, and rare legal terms.

Fallbacks should be invoked only on hard failure:

- native Postgres FTS if `pg_search` packaging fails;
- standalone Tantivy + local vector index + SQLite/Arrow if embedded Postgres itself fails;
- LanceDB only if the Postgres route fails both packaging and quality gates;
- Qdrant remains out of scope.

### 2. Embedding Model

**Answer:** start with `BAAI/bge-m3` as the default benchmark candidate, served through the configured OpenAI-compatible endpoint. Keep French-specialist models in the eval set, but do not choose them by intuition.

Reasoning:

- `bge-m3` is multilingual, supports long context, outputs 1024-dim dense vectors, and is designed for dense/sparse/multi-vector retrieval.
- The legal corpus includes French statutes, court decisions, identifiers, and occasionally multilingual/EU-adjacent material, so a robust multilingual default is sensible.
- French-specialist models such as `sentence-camembert-large` and Solon may beat it on some French semantic tasks, but they must win on the project eval set, not on generic STS.

Recommended model gate:

- Benchmark `bge-m3`, `sentence-camembert-large`, Solon embeddings, and at least one strong hosted multilingual embedding model if available through the endpoint.
- Choose the model that wins legal retrieval metrics after hybrid fusion, not just standalone dense recall.
- If dense-only differences are small, prefer `bge-m3` because it preserves the path to learned sparse / ColBERT-style upgrades.

Endpoint note:

- A local `llama.cpp` endpoint is acceptable only if it serves a dedicated embedding model with stable pooling and dimension. The design should not assume every embedding model is easy or equivalent under `llama.cpp`; the provider fingerprint and dimension check are mandatory.

### 3. Reranker

**Answer:** evaluate the reranker before the first production LEGI release. It should ship in Phase 1 if it materially improves legal ranking within latency budget; otherwise it moves to Phase 3 with a documented eval result.

I would use `BAAI/bge-reranker-v2-m3` as the first candidate because it is multilingual and aligns with the BGE stack. But do not hardwire it as the only path.

Adoption gate:

- Run reranking on fused top-K only, e.g. 50 -> 8.
- Measure recall@k, nDCG, citation exactness, stale-citation handling, latency, and token/call efficiency.
- Require a meaningful improvement over hybrid+authority, not just marginal score movement.
- If local Rust inference is the blocker but quality gain is large, add a reranker-provider abstraction rather than dropping reranking from the best-in-class release.

Practical answer: expect reranking to be needed for best-in-class legal nuance, but let the eval decide the implementation path.

### 4. Scope / Phase Bar

**Answer:** Phase 1 can be best-in-class **LEGI/statutory search**. The project should not claim best-in-class **French juridic search** overall until Phase 2 includes Judilibre and administrative justice with zone-aware chunking, authority signals, graph relationships, and citation verification.

This distinction matters:

- Phase 1: production-quality official-XML LEGI search.
- Phase 2: full French legal search across statutes and jurisprudence.
- Phase 3: ranking frontier work and corpus expansion.

So the docs should avoid implying a LEGI-only release is the complete best-in-class juridic engine.

## Residual Review Findings

### P1: `RESEARCH.md` still contains stale bootstrap language

`RESEARCH.md` still says `AgentPublic/legi` must be reconstituted and re-chunked. That was correct before D7, but now the rule is stronger: `AgentPublic/legi` is comparison-only and never feeds a real index.

Update the research appendix to say:

- derived HF corpora are useful for smoke tests and regression comparisons;
- they are never authoritative ingestion inputs;
- their chunks and embeddings must not influence model or chunking decisions.

### P1: `RESEARCH.md` still frames embedded Postgres as "preferred"

The design and decisions now say embedded Postgres + `pgvector` + `pg_search` is decided. `RESEARCH.md` still says "preferred default candidate" and "lock it via the spike."

Change this to:

- selected backend: embedded Postgres + `pgvector` + `pg_search`;
- spike validates packaging/quality;
- fallbacks engage only on hard failure.

### P1: Reranker provider should be abstracted

The design allows endpoint-based embeddings but still frames reranking mostly as local Rust inference. For best-in-class quality, the reranker should have a provider abstraction too:

- `reranker.provider = "disabled" | "local" | "http"`;
- local cross-encoder via `ort`/Candle;
- HTTP rerank endpoint if quality justifies it and local packaging lags.

This keeps the quality bar from being blocked by Rust inference packaging.

### P2: Fix stale citation cross-reference

`DESIGN.md` §2.1 still points citation verification to `§11`, but grounding is now `§10.5`. Update that reference.

### P2: Clean up HF wording in architecture

`DESIGN.md` still mentions optional Python helpers for "HF conversion" and "HF-dataset experiments." That is acceptable for research, but the architecture diagram should not imply HF conversion is part of the ingestion path.

Suggested wording: "derived-dataset comparison tools" instead of "HF conversion."

## Recommended Final Answers To Record

- **Backend:** embedded Postgres + `pgvector` + `pg_search`; validate, do not reselect.
- **Embedding model:** `bge-m3` as default benchmark candidate; choose final model by legal eval; endpoint-served.
- **Reranker:** benchmark before Phase 1; include if quality gain is material within latency budget.
- **Phase claim:** Phase 1 is best-in-class LEGI search; full best-in-class French juridic search requires Phase 2 jurisprudence coverage.

## Sources Checked

- `pg_search` BM25 in Postgres: https://pgxn.org/dist/pg_search/
- `pgvector` vector search in Postgres: https://github.com/pgvector/pgvector
- `BAAI/bge-m3`: https://huggingface.co/BAAI/bge-m3
- `BAAI/bge-reranker-v2-m3`: https://huggingface.co/BAAI/bge-reranker-v2-m3
- MTEB-French: https://arxiv.org/html/2405.20468v2
- llama.cpp `/v1/embeddings`: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md
