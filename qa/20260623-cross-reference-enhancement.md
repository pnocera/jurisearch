# Q&A — 20260623-cross-reference-enhancement

## Question

We have a large corpus of documents and want to ENHANCE cross-referencing between them — discover
and build more (and better) links between documents than we currently have. This is a general
document-linking question, NOT about any specific citation format or domain.

What we can build on (jurisearch stack):
- Embedded PostgreSQL with pgvector (dense ANN over bge-m3 1024-dim embeddings, per text chunk) and
  pg_search (BM25 full-text).
- A `graph_edges` table that already holds some explicit document-to-document links (each edge has a
  source/target document, an `edge_source` tag, and a JSON payload).
- Per-document text plus chunked text; CLI-only, single-node, no external services.

Be concrete and opinionated. Web-research current techniques and tools where it helps (general
document-linking / relatedness / entity-linking / link-prediction — not domain-specific).

1. What are the main families of techniques for discovering/enhancing cross-references between
   documents, and when does each win? Cover at least: dense-embedding nearest-neighbour relatedness,
   lexical/BM25 overlap, shared-entity / entity linking, co-citation & bibliographic coupling on the
   existing edge graph, graph link-prediction / GNNs, and LLM-based relation/reference extraction.
   Which give the best precision-per-effort?

2. Given our stack (pgvector ANN + pg_search BM25 + an existing edge graph, single node), what is the
   highest-leverage, lowest-effort pipeline to materialise NEW cross-reference edges at scale? How to
   generate candidates, rank/score them, set thresholds, and control precision (avoid flooding the
   graph with weak links)?

3. How to keep distinct KINDS of links separate and trustworthy: explicit/authoritative links vs
   derived "related" links — edge typing, per-edge confidence/score, provenance (which method
   produced it), and how a consumer should treat each.

4. How to make derived cross-references EVALUABLE (precision/recall) without circularity — e.g. using
   the existing explicit edges as partial ground truth, held-out splits, and spot-check sampling.
   What metrics and what guards against "we built it from X and then graded it against X".

5. Architecture: build at ingestion time vs a separate batch enrichment pass; how to store derived
   edges (edge_source, method, confidence) and keep them incrementally up to date as documents are
   added/changed; cost at corpus scale.

6. Prioritised recommendation: one concrete first increment that delivers visible value quickly, then
   the fuller build.

A few sentences per question is fine — I want a prioritised, buildable direction.


## Answer

# Enhancing Cross-References Between Documents

## 1. Main technique families and when each wins

The best practical answer is not one technique. Use several cheap candidate generators, then only materialise links that have enough independent evidence. Keep "explicit reference" links distinct from "derived relatedness" links.

**Dense-embedding nearest-neighbour relatedness** wins when two documents use different vocabulary but discuss the same concept, fact pattern, process, event, or issue. With your bge-m3 chunk embeddings in pgvector, this is already the easiest high-recall signal. It is weaker for precise cross-references because "semantically similar" does not mean "should be linked", and chunk-level matches can over-link boilerplate or broad background sections. Use it as a candidate generator, not as the only edge criterion.

**Lexical/BM25 overlap** wins when important terms, names, identifiers, quoted phrases, titles, section names, or domain-specific vocabulary recur. It is cheap, explainable, and often higher precision than dense retrieval for exact reference discovery. It misses paraphrase and synonymy. In practice, dense plus BM25 with reciprocal rank fusion or a small learned ranker is a very strong first baseline.

**Shared entity / entity linking** wins when documents mention the same people, organizations, places, products, events, works, codes, or internal identifiers. Entity overlap is a good precision booster because it makes a dense/BM25 match less likely to be accidental. Plain NER is useful, but canonicalization matters: "IBM", "International Business Machines", and "I.B.M." should collapse when possible. Local tools worth considering include spaCy for conventional NER, GLiNER for flexible local entity extraction, and REL or similar systems for entity linking where a knowledge base is available. If there is no reliable KB, build a corpus-local entity table with aliases and confidence.

**Co-citation and bibliographic coupling over existing graph edges** win when the current explicit edge graph is already meaningful. Co-citation asks "are these two documents both pointed to by the same documents?" Bibliographic coupling asks "do these two documents point to many of the same documents?" These are cheap SQL graph features and often produce high-trust "related" links because they reuse authorial or authoritative structure already present in the graph. They are less useful for isolated new documents or sparse graph regions.

**Graph link prediction / GNNs** win only after you have a large, reasonably clean graph, node features, and a repeatable evaluation set. Classical graph features such as common neighbours, Adamic-Adar, preferential attachment, SimRank/PageRank-ish proximity, co-citation count, and bibliographic-coupling count are the right first step. GNNs such as GraphSAGE/GAT/SEAL-style link prediction can improve recall in mature graphs, but they add training, leakage risk, negative sampling choices, reproducibility issues, and explainability cost. I would not make GNNs the first increment.

**LLM-based relation/reference extraction** wins when you need relation labels and rationale, or when references are implicit in prose and not captured by lexical/embedding similarity. For example: "this document supersedes that one", "criticizes", "extends", "duplicates", "summarizes", "depends on", or "mentions as authority". In a CLI-only, single-node setup with no external services, an LLM is best used later on a small top-K candidate set or on high-value documents, not as a full-corpus all-pairs extractor. It can produce high precision, but only if constrained to structured output with evidence spans and abstention.

Best precision-per-effort:

1. Hybrid dense ANN plus BM25 candidate generation with reciprocal rank fusion, top-N caps, and conservative thresholds.
2. Add graph features from the existing explicit edges: co-citation, bibliographic coupling, existing path proximity, and shared-neighbour counts.
3. Add corpus-local entity extraction/canonicalization as a precision booster.
4. Add a cheap supervised ranker trained from existing explicit edges plus hard negatives.
5. Use LLM extraction only for relation typing or reranking the top ambiguous/high-value cases.
6. Defer GNNs until the graph and evaluation harness prove the simpler model is saturated.

## 2. Highest-leverage pipeline for your stack

Build a separate enrichment batch that produces derived candidate edges, scores them, and materialises only a controlled subset into `graph_edges`. Do not run a quadratic all-document comparison.

Recommended first pipeline:

1. **Candidate generation per source document**
   - Dense: for each document, query pgvector using either a document centroid embedding or the top representative chunks; retrieve top 100-300 candidate target documents by max chunk similarity and mean of top matching chunks.
   - BM25: query pg_search using title, headings, extracted keyphrases, and high-signal chunks; retrieve top 100-300 target documents.
   - Graph: from existing explicit edges, generate candidates by co-citation, bibliographic coupling, and two-hop proximity.
   - Entity: if entity extraction exists, generate candidates that share rare/canonical entities, especially entities that occur in titles, headings, or lead sections.

2. **Normalize and fuse candidates**
   - Store candidates in an intermediate table keyed by `(source_document_id, target_document_id, method_version)`.
   - Keep per-method features: dense rank, dense score, BM25 rank, BM25 score, shared entity count, rare shared entity count, co-citation count, bibliographic-coupling count, existing shortest-path-ish signal, reciprocal match boolean, same collection/date/type flags if relevant, and boilerplate penalties.
   - Use reciprocal rank fusion as the first fusion method because it is robust across differently scaled dense and lexical scores.
   - Then move to a small logistic regression or gradient-boosted ranker once you have labels.

3. **Score for materialisation**
   - Compute a final `confidence` in 0..1 and a `rank_score` for sorting.
   - Require either strong single evidence or multiple independent weak evidences. Example policy:
     - High confidence: dense top-20 and BM25 top-20, or dense top-20 plus strong graph/entity support, or BM25 top-10 plus rare shared entity support.
     - Medium confidence: dense/BM25 agreement in top-100 plus one supporting signal.
     - Low confidence: dense-only or BM25-only topical similarity. Keep these out of `graph_edges` unless a consumer explicitly asks for suggestions.

4. **Thresholds and caps**
   - Start conservative: materialise at most 5-10 derived related links per source document.
   - Require a minimum score and a rank margin: the chosen target should be clearly above the source document's local candidate tail.
   - Use reciprocal confirmation when possible: A retrieves B and B retrieves A, or A retrieves B and a graph/entity signal confirms it.
   - Separate near-duplicate detection from relatedness. Near duplicates should be their own edge type because they can otherwise dominate "related" links.
   - Penalize boilerplate-heavy chunk matches, very short documents, and overly common entities.

5. **Materialise**
   - Insert only high-confidence derived edges into `graph_edges`.
   - Keep lower-confidence candidates in a separate `document_link_candidates` table for inspection, future ranker training, or UI suggestions.

This gives visible value quickly because it uses pgvector, pg_search, and the existing graph with mostly SQL plus one batch job.

## 3. Keep link kinds separate and trustworthy

Do not overload one edge type. Consumers need to know whether a link is explicit evidence or inferred relatedness.

Use at least these concepts:

- `edge_source`: high-level provenance, for example `explicit_import`, `explicit_extracted`, `derived_hybrid_related_v1`, `derived_graph_related_v1`, `derived_entity_related_v1`, `llm_relation_v1`.
- `edge_type` or payload field: semantic kind, for example `references`, `cites`, `same_entity_context`, `related`, `near_duplicate`, `supersedes`, `contradicts`, `summarizes`, `depends_on`.
- `confidence`: calibrated-ish 0..1 score for derived edges. Explicit imported edges can be `1.0` or null with `is_authoritative=true`.
- `rank_score`: uncalibrated score used only for ordering within a method version.
- `method`: structured method name, for example `dense_bm25_rrf_graph_features`.
- `method_version`: immutable version string or hash of code, model, thresholds, and feature config.
- `evidence`: JSON payload containing top matching chunks, BM25 snippets, shared entities, graph evidence counts, and source feature values.
- `created_at`, `updated_at`, `source_document_version`, `target_document_version`.

Consumer rule:

- Explicit/authoritative links can be shown as document structure.
- Derived high-confidence links can be shown as "related documents" or "suggested cross-references".
- Medium-confidence links should be opt-in, reviewable, or used only for ranking/search expansion.
- Low-confidence candidates should not be graph edges by default.

This protects trust. A consumer should never have to infer from a generic edge that something was author-written when it was algorithmically guessed.

## 4. Make derived links evaluable without circularity

Use the existing explicit graph as partial ground truth, but do not pretend it is complete. Missing explicit edges are not necessarily negatives.

Evaluation setup:

1. **Hold out explicit edges**
   - Split explicit edges into train/dev/test.
   - Prefer temporal split if timestamps exist: train on older explicit edges and test on newer ones.
   - If random splitting, split by source document or connected component where practical to reduce leakage.

2. **Prevent graph-feature leakage**
   - When testing graph-based features, remove held-out explicit edges before computing co-citation, bibliographic coupling, common neighbours, PageRank/proximity, or any graph-derived feature.
   - Keep method-specific evaluations: if a candidate was generated from dense/BM25 only, evaluate that separately from graph-assisted reranking.

3. **Use hard negatives**
   - Random negatives are too easy.
   - Use hard negatives from dense/BM25 top results that are not known positives, plus same-entity non-linked pairs.
   - Treat them as "assumed negatives" for ranking metrics, not proof of irrelevance.

4. **Metrics**
   - For retrieval: Recall@K against held-out explicit edges, MRR, MAP, nDCG@K.
   - For materialised links: Precision@K per source document, sampled precision by confidence bucket, and edge volume per document.
   - For graph health: distribution of derived edges per document, isolated-document coverage, component growth, and percentage of edges with evidence from multiple methods.

5. **Human spot checks**
   - Sample by confidence bucket, method, corpus segment, and document length.
   - Ask reviewers to label: good cross-reference, weak but related, near duplicate, wrong, unsure.
   - Track precision separately for explicit-reference-like links and broad relatedness links.

Guardrail: never grade a method only against the same signal used to build it. For example, if graph co-citation generated the edge, also evaluate whether dense/BM25/entity/spot-check evidence supports it. If dense generated the edge, measure against held-out explicit edges and human samples, not against dense similarity itself.

## 5. Architecture and storage

Use a separate batch enrichment pass, not ingestion-time full linking. Ingestion should compute document text, chunks, embeddings, BM25 indexes, and extracted entities. Cross-reference enrichment should run as an idempotent batch that can be resumed, versioned, and re-run with new thresholds.

Suggested tables:

```sql
-- Optional staging table for inspectable candidates.
CREATE TABLE document_link_candidates (
  source_document_id bigint NOT NULL,
  target_document_id bigint NOT NULL,
  method_version text NOT NULL,
  candidate_sources text[] NOT NULL,
  rank_score double precision NOT NULL,
  confidence double precision,
  features jsonb NOT NULL,
  evidence jsonb,
  source_document_version text,
  target_document_version text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (source_document_id, target_document_id, method_version)
);
```

Then insert selected candidates into `graph_edges` with a derived `edge_source` and structured JSON payload:

```json
{
  "edge_type": "related",
  "method": "dense_bm25_rrf_graph_entity",
  "method_version": "xref-v1",
  "confidence": 0.87,
  "rank_score": 42.3,
  "candidate_sources": ["dense", "bm25", "graph"],
  "features": {
    "dense_rank": 8,
    "bm25_rank": 17,
    "rrf_score": 0.028,
    "co_citation_count": 3,
    "bibliographic_coupling_count": 1,
    "shared_entity_count": 4
  },
  "evidence": {
    "matching_chunks": [
      {"source_chunk_id": 123, "target_chunk_id": 456, "similarity": 0.82}
    ],
    "shared_entities": ["example canonical entity"]
  }
}
```

Incremental update plan:

- Track document content/version hashes.
- On document add/change, recompute its chunks, embeddings, BM25 data, entities, and candidates where it is the source.
- Also recompute candidates for likely affected neighbours: documents that previously linked to it, documents sharing entities, and documents for which the changed document appears in top ANN/BM25 candidates.
- Keep `method_version` immutable. If thresholds or models change, write a new version and retire or supersede old derived edges rather than mutating them invisibly.
- Delete or mark stale derived edges when either endpoint document version no longer matches the payload.

Cost shape:

- Dense ANN: roughly `documents * representative_queries * top_k`, feasible single-node if representative chunks are capped.
- BM25: similar top-K lexical retrieval, cheap.
- Graph features: cheap SQL joins if explicit edges are indexed by source and target.
- Entity candidates: cheap with an inverted `entity_id -> document_id` table, but cap common entities.
- Reranking: cheap because it runs only on the unioned candidate set, not all pairs.

## 6. Prioritised recommendation

First increment: build `xref-v1` as a conservative derived-related linker:

1. Generate dense candidates from pgvector and lexical candidates from pg_search per document.
2. Add graph features from existing explicit edges: co-citation count and bibliographic-coupling count.
3. Fuse dense and BM25 ranks with reciprocal rank fusion.
4. Materialise only top 5 high-confidence links per document where at least two signals agree, using `edge_source = 'derived_hybrid_related_v1'` and payload fields for `edge_type`, `method`, `method_version`, `confidence`, `features`, and `evidence`.
5. Save the rest in `document_link_candidates`.
6. Evaluate with held-out explicit edges plus a 200-500 pair spot-check sample stratified by score bucket.

This is the best value first because it uses what you already have, avoids model-training complexity, gives visible related-document links quickly, and creates the data needed to train a better ranker later.

Fuller build after that:

1. Add entity extraction and corpus-local canonicalization; use rare shared entities as precision boosters.
2. Train a simple supervised ranker from held-out explicit positives and hard negatives.
3. Add relation-specific extraction for high-value cases, optionally with a local LLM constrained to evidence spans.
4. Add graph-only related edges for sparse lexical/dense cases where co-citation or bibliographic coupling is very strong.
5. Consider GNN link prediction only after the simpler ranker has stable evaluation, enough labels, and a clear product need for more recall.

## Sources checked

- pgvector README and docs on HNSW/IVFFlat indexing, approximate search, filtering, and iterative scans: https://github.com/pgvector/pgvector
- ParadeDB/pg_search documentation for PostgreSQL BM25-style search: https://docs.paradedb.com/documentation/overview
- Cormack, Clarke, and Buettcher, "Reciprocal Rank Fusion Outperforms Condorcet and Individual Rank Learning Methods": https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf
- Stanford IR book, evaluation metrics including precision/recall, MAP, and related ranked retrieval evaluation: https://nlp.stanford.edu/IR-book/html/htmledition/evaluation-in-information-retrieval-1.html
- GLiNER project for local generalist named entity recognition: https://github.com/urchade/GLiNER
- REL entity linking project: https://github.com/informagi/REL
- Kipf and Welling, "Variational Graph Auto-Encoders", a standard graph link-prediction reference point: https://arxiv.org/abs/1611.07308
- Hamilton, Ying, and Leskovec, "Inductive Representation Learning on Large Graphs" (GraphSAGE): https://arxiv.org/abs/1706.02216
- Bibliographic coupling and co-citation background definitions: https://en.wikipedia.org/wiki/Bibliographic_coupling and https://en.wikipedia.org/wiki/Co-citation
