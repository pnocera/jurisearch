# Design decisions & validation gates

ADR-lite: each decision has a decision/recommendation + rationale + alternatives. Items marked **DECIDED** are locked by the 2026-06-20 reviews; the rest are recommendations to proceed on. **No product questions remain open** — what's left is validation (see end). The design is **lock-ready** (lock-readiness review, 2026-06-20).

> **Revised 2026-06-20** per the design review, which locked four product constraints: binary name `jurisearch`, Rust search/runtime, CLI-only (no MCP/HTTP), and complete inline help as the contract. Those flip D1/D2/D6 from open to decided, reshape D3, and add D11–D13.
> **Revised again 2026-06-20** per the *updated* design review (`work/reviews/2026-06-20-updated-design-review.md`), which accepted the direction and tightened underspecified contracts → **D14–D20** (bootstrap reconstitution, zone-vs-relationship modeling, citation-verification contract, JSONL wire protocol, embedded-Postgres packaging, model-cache rule, plus `kind`/temporal/help-schema consistency).
> **Direction-locked 2026-06-20** per `work/reviews/2026-06-20-direction-lock-review.md`: the last open decisions are now closed — **D3** (embedded Postgres + `pgvector` + `pg_search`), **D5** (OpenAI-compatible embeddings endpoint default, incl. local `llama.cpp`; in-process optional), **D7** (official DILA/LEGI XML from day 1). The roadmap is reframed into production-grade phases and the old open-questions list becomes **validation gates**. No open *product* questions remain.
> **Validation-gate answers recorded 2026-06-20** per `work/reviews/2026-06-20-open-questions-review.md`: backend = *validate, don't reselect* with explicit fallback precedence (D3); embedding model = `bge-m3` default benchmark, chosen by post-fusion legal eval, with an endpoint caveat (D4); reranker = benchmark before Phase 1, **pluggable local/HTTP provider** (D11); **phase claim** = Phase 1 best-in-class LEGI, full juridic at Phase 2 (D8). Plus residual doc fixes (RESEARCH bootstrap + "preferred backend" wording, §2.1 citation cross-ref).
> **Lock-readiness cleanup 2026-06-20** per `work/reviews/2026-06-20-lock-readiness-review.md`: fixed the last zones-vs-relationships inconsistency (D10), retitled to "validation gates", and aligned **selected-backend** wording (no more "preferred"/"open question" for `pg_search`). The design is **sound to lock**; the locked conceptual reference is `work/02-conception/CONCEPTION.md`.

---

## D1. Tool name — **DECIDED: `jurisearch`**
- **Decision:** the binary, Cargo crate/workspace, config dir, and env prefix are all `jurisearch`.
- Rationale: `juri` is short but ambiguous; `jurisearch` is self-describing to agents and humans scanning a tool list — and discoverability matters more now that there is no MCP registry.
- Conventions: binary `jurisearch`; config `~/.config/jurisearch/config.toml`; env prefix `JURISEARCH_`; **internal IDs use source-based namespaces** (`legi:`, `judilibre:`, `ja:`, `ecli:`), with `jurisearch:` reserved for synthetic internal objects only — never `juri:`.
- A short alias can be added later if typing cost proves real; `jurisearch` stays the primary command.

## D2. Implementation language — **DECIDED: Rust runtime, Python offline-only**
- **Decision:** the search/runtime core and CLI are **Rust** (`jurisearch-core`, `jurisearch-cli`, `jurisearch-ingest`). **Python is permitted only for offline ingestion helpers** (`tools/ingest-python`) that emit canonical records — never on a query path.
- Rationale: CLI-only + subprocess-spawned use rewards tiny startup, single-binary distribution, and one language owning every query path. Rust delivers all three; the ecosystem is viable (Tantivy, LanceDB Rust SDK, `pgvector`, `pg_search`, `fastembed-rs`, `ort`, Candle, `clap`).
- Boundary (hard rule, DESIGN §13.4): Python may download/parse/normalize sources and emit canonical JSONL/Parquet/Arrow; Rust owns schema validation, index build, manifest, and all queries. A Rust test proves canonical records index + search with no Python in the loop.
- This reverses the prior "Python core + LanceDB, Rust deferred to v2" recommendation.

## D3. Index backend — **DECIDED: embedded Postgres + `pgvector` + `pg_search`**
- **Decision:** a single **embedded Postgres** store (`pg-embed` / `postgresql_embedded`) holds documents, chunks, metadata, temporal columns, graph edges, manifests, and vectors (`pgvector`), with **`pg_search`/ParadeDB** for BM25. This *is* the backend, not one candidate among several.
- The **Rust spike now *validates*** this stack against hard packaging + quality criteria (DESIGN §13.3) on ~50k LEGI versions + 10k decisions, target stable JSON < 500 ms warm — it does **not** pick among peers, and must not reopen the decision unless a hard criterion fails.
- **Fallbacks only on a hard failure, in precedence:** (1) native Postgres FTS **if `pg_search` packaging fails**; (2) Tantivy + local Rust vector index + SQLite/Arrow **if embedded Postgres itself fails**; (3) LanceDB (Rust SDK) **only if the Postgres route fails both packaging *and* quality** gates. **Qdrant stays out of scope** — a separate service conflicts with the embedded CLI shape.

## D4. Embedding & rerank models — **embedding DECIDED (see D21)**, rerank eval-gated (D11)
- **Dense:** `BAAI/bge-m3` (multilingual, FR-strong, 8k ctx, dense+sparse from one model) — the **benchmark-default**, not an assumption; validated on the legal eval set before lock-in. Served via the configured **embeddings endpoint** (D5; in-process `fastembed-rs` optional). Same model for docs + queries (fingerprint in the manifest).
- **Rerank:** `BAAI/bge-reranker-v2-m3` (or alt) — **benchmark-gated** (see D11); ships in Phase 1 if it clears the latency/quality gate, else Phase 3.
- **French-specialist alts to benchmark:** `Lajavaness/sentence-camembert-large`, Solon. Decide via the eval set (§15 of DESIGN), not by guessing.
- **Model gate — superseded by D21 (embedding locked to `bge-m3`).** The original plan was to benchmark `bge-m3`, `sentence-camembert-large`, Solon, and ≥1 hosted multilingual model and pick the post-fusion winner, preferring `bge-m3` on a near-tie. A local validation (2026-06-22) found bge-m3 statistically indistinguishable from CamemBERT and Solon on French-legal retrieval, so the bake-off is not run and bge-m3 is locked — see **D21**.
- **Endpoint caveat:** a local `llama.cpp` endpoint is acceptable **only** if it serves a *dedicated* embedding model with stable pooling + dimension; the provider fingerprint + dimension check are mandatory — not every model is easy or equivalent under `llama.cpp`.

## D5. Embeddings provider — **DECIDED: OpenAI-compatible endpoint default**
- **Decision:** query (and document) embeddings default to an **OpenAI-compatible HTTP `/v1/embeddings` endpoint**. "Remote" explicitly **includes a local/self-hosted endpoint** such as `llama.cpp` serving `/v1/embeddings` on loopback — so privacy/offline is met without coupling search quality to Rust inference packaging. In-process Rust embeddings (`fastembed-rs`) remain an **optional offline backend**, not the primary path.
- Contract (DESIGN §11.2, §14): a **dedicated embedding model** (not a chat model), pooling mode ≠ `none`, a recorded provider/model/dimension/normalization fingerprint, a **hard dimension check** on mismatch, and a `127.0.0.1` endpoint treated as a "remote provider" by the CLI. Same provider/model for docs + queries unless the index declares a migration.
- `JURISEARCH_EMBED_API_KEY` carries the key for hosted providers; omitted for local `llama.cpp`.

## D6. Transport — **DECIDED: CLI-only (one-shot + JSONL session)**
- **Decision:** ship a single Rust core behind a **CLI-only** surface. **No MCP, no HTTP server, no `serve` daemon.** Warm multi-call agent use is served by **`jurisearch session --jsonl`** (and `batch --jsonl`) — still a plain stdio subprocess interface.
- Rationale: matches the product constraint; removing MCP raises subprocess ergonomics, which the session mode + inline help (D12) cover.
- Consequence: the cold-start risk `serve` used to hide is handled by the session mode (load models once) — see DESIGN §11.

## D7. Corpus source — **DECIDED: official DILA/LEGI XML from day 1**
- **Decision:** the authoritative index is built from **official DILA/LEGI XML** (plus Judilibre and justice-administrative open data) from day 1 — **no HF bootstrap** of the authoritative index. The official XML parser is the first ingestion path and defines every downstream invariant (temporal correctness, hierarchy, citation edges, freshness, reproducibility), preserving raw IDs/dates/hierarchy/status/links/provenance.
- PISTE/Judilibre APIs are used for targeted lookup, deltas, and citation confirmation.
- **`AgentPublic/legi` and other derived datasets are non-authoritative** — comparison fixtures / regression baselines / smoke-test data only, and must not influence the embedding-model decision (supersedes the earlier bootstrap idea; see D14).

## D8. Scope of corpora — recommend
- **Recommendation:** Phase 1 = LEGI codes (official XML); Phase 2 = Judilibre (Cass) + justice administrative (CE/CAA/TA). Defer EU law, KALI, BOFIP, doctrine to Phase 3+ (designed-for, not ingested).
- Rationale: covers the foundation document's examples (codes + both supreme jurisdictions) without overreaching.
- **Best-in-class claim is phased (open-questions review):** Phase 1 is best-in-class **LEGI/statutory** search; the full best-in-class **French juridic** claim (statutes + jurisprudence) requires Phase 2. Docs must not present a LEGI-only release as the complete juridic engine. (DESIGN §17)

## D9. Graph store — recommend
- **Recommendation:** keep the knowledge graph as **edge tables inside the same embedded store** (1–2 hop bounded traversal), not a separate graph DB. Revisit only if multi-hop legal reasoning queries justify a dedicated graph engine.
- **Constraint (assessment):** the graph surfaces **candidate** links (incl. publisher-provided Judilibre `rapprochements`, flagged `edge_source: publisher` vs `inferred`); it must **not** auto-assert *jurisprudence constante*. Settled-law claims need authority/publication/recency/citation-frequency + a human-checkable source set, so `related` returns *ranked candidates* carrying those signals, never a verdict (DESIGN §8).

## D10. Decision chunking — official zones first — recommend
- **Recommendation:** chunk decisions on the **publisher's official *text* zone offsets** (Judilibre: introduction / visa / moyens / motivations / dispositif / moyens annexes / summary (sommaire)), reassembling fragments **by zone identity** because they can be **non-sequential** in the source text. **`rapprochements` and applied texts are NOT zones** — they are relationship metadata that populate graph edges (see D15). Regex/heuristic splitting is a **fallback only** (e.g. older administrative decisions) and is flagged (`chunking: heuristic`) when used.
- **Rationale (assessment):** regex-only splitting is brittle and can interleave legal reasoning with procedural arguments or annexes; official offsets keep chunks faithful and citations exact. This is invariant §2.1.4 in DESIGN.

## D11. Reranker delivery — **DECIDED: benchmark-gated ranking component**
- **Decision:** the reranker is a **benchmark-gated ranking component**, not a roadmap nice-to-have. A release may ship **without** it only if evals prove hybrid+authority meets the best-in-class bar; if reranking materially improves legal recall/nDCG/citation-exactness **within the latency budget**, it ships **before** the first best-in-class (Phase 1) release. The eval gate decides, after a Rust inference spike (model availability, tokenizer, ONNX/Candle, latency, packaging).
- **Pluggable provider (open-questions review P1):** `reranker.provider = disabled | local | http` — local cross-encoder via `ort`/Candle, or an **HTTP rerank endpoint** if quality justifies it and local packaging lags — so the quality bar is never blocked by Rust inference packaging. First candidate `bge-reranker-v2-m3`, not hardwired.
- **Adoption gate:** rerank fused top-K (50→8); require a *meaningful* gain over hybrid+authority on recall@k / nDCG / citation-exactness / stale-handling within the latency + token/call budget; a Phase-3 deferral records the eval result. (DESIGN §7.2, §14)
- Rationale (direction-lock P1): for a best-in-class engine, ranking quality is validated by evals — the reranker is neither assumed nor deferred by default.

## D12. Inline help is part of the contract — **DECIDED**
- **Decision:** because there is no MCP, the agent contract lives in the binary. Required surface: `jurisearch --help`, `jurisearch <command> --help`, **`jurisearch help agent`** (full contract in one call), **`jurisearch help schema --json`** (machine-readable schemas). The eval harness asserts completeness (DESIGN §10.4, §15).

## D13. Project licence enables `pg_search` — **DECIDED: AGPL-3.0 acceptable**
- **Decision:** AGPL-3.0 is acceptable for this project, so **`pg_search`/ParadeDB** (AGPL) is the **selected** lexical engine; packaging/runtime fit is a **validation gate**, not a licensing question. Native Postgres FTS and standalone Tantivy remain non-AGPL fallbacks only on hard failure.

## D14. `AgentPublic/legi` is comparison-only, never an ingestion input — **DECIDED** (superseded by D7)
- **Decision:** with official XML the sole authoritative source (D7), derived datasets (`AgentPublic/legi`, `legalkit`) are **non-authoritative** — comparison / regression / smoke-test fixtures only. They never seed canonical records, chunks, or embeddings for a real index, and their precomputed chunks/embeddings (LangChain 1024-char splits, stringified BGE-M3 vectors) must not influence the embedding-model choice. (This is stronger than the prior "reconstitute, don't import" rule — there is now no bootstrap path at all.)
- Rationale (direction-lock P2): a single source of truth keeps temporal/hierarchy/citation invariants clean. (DESIGN §4, §13.4)

## D15. Judilibre `rapprochements`/applied texts are relationships, not zones — **DECIDED**
- **Decision:** `zones` holds **text-offset ranges only**. Publisher **`related_decisions`** (rapprochements) and **`texts_applied`** are relationship metadata → graph edges (`rapprochements`, `applies_article`) with `edge_source: publisher|inferred`. Store **raw Judilibre taxonomy keys** (e.g. publication `["b","r"]`, keys b/r/l/c) alongside normalized labels.
- Rationale (review P1): the public spec separates text zones from applied-texts/rapprochements metadata; conflating them pollutes chunks. (DESIGN §5.1, §6, §8)

## D16. Citation-verification contract — **DECIDED**
- **Decision:** `cite` accepts internal IDs, LEGIARTI/LEGITEXT/LEGISCTA, ECLI, pourvoi, NOR, CETATEXT, and free text; returns one of `exact | normalized | ambiguous | stale_version | not_found | source_unavailable`; resolves **local-by-default**, official APIs only with `--online`; `--strict` makes anything but exact/normalized a failure (exit `2`); `--as-of`/dated citations select the version to verify. Documented JSON response schema. (New DESIGN §10.5; fixes the stale "§11 Grounding" pillar cross-ref.)

## D17. JSONL session wire protocol — **DECIDED**
- **Decision:** request `{"id","command","args"}`, response `{"id","ok",...,"result"|"error"}`. Order preserved (id echoed); stdout JSONL-only, diagnostics to stderr; `help`/`help schema` usable in-session; malformed line → JSONL error (non-fatal, `--fatal` opt-in); `exit` acknowledged then exit `0`. (DESIGN §11.1)

## D18. Embedded Postgres = managed process + packaging spike — **DECIDED (criteria)**
- **Decision:** "embedded Postgres" means a **managed local Postgres child process** (real binaries via `pg-embed`/`postgresql_embedded`), not an in-process library. The backend spike must clear explicit packaging criteria: bundled-vs-downloaded binaries, pinned `pgvector`/`pg_search` versions + install/migration, socket/ephemeral-port binding with no public exposure, single-writer locking, crash recovery/clean shutdown, index/schema upgrade story, cross-platform policy. No-Postgres fallback (Tantivy + local vector index + SQLite/Arrow) is the genuinely in-process path. (DESIGN §13.1, §13.3, §13.5)

## D19. Model-cache rule — **DECIDED**
- **Decision:** to keep "no network at query time" honest (`fastembed-rs` downloads on first use), local models are pre-fetched via `jurisearch model fetch`/`setup`; `status` reports missing models; `search`/`session` **fail with an actionable error** rather than download silently, unless `--allow-download`. (DESIGN §2, §11.2, §14)

## D20. Minor contract consistency — **DECIDED**
- **`kind`:** CLI flag `--kind code|decision|all`; result field `kind: article|decision`; documented mapping `code⇒article`, with `help schema --json` as arbiter.
- **Temporal sentinels:** normalize open-ended validity (missing `dateFin`, `2999-01-01`) to `valid_to: null`, preserve `valid_to_raw`; as-of semantics `valid_from <= as_of && (valid_to is null || as_of < valid_to)`.
- **`help schema --json` works without an index:** schemas compiled into the binary; index `schemas/` is a provenance copy. (DESIGN §5.1, §10.2, §10.4, §12)

## D21. Embedding model — **DECIDED: `bge-m3` locked as v1 (French-specialist bake-off skipped)**
- **Decision:** `BAAI/bge-m3` is the **locked v1 embedding model** (fingerprint `bge-m3 / 1024-d / pooling=cls / normalize=true`). The Phase-1 comparative embedding-model bake-off is **not run**: bge-m3 was validated locally (2026-06-22) as **statistically indistinguishable** from the two strongest French specialists — `Lajavaness/sentence-camembert-large` and `OrdalieTech/Solon-embeddings-large-0.1` — on a curated French-legal retrieval set (identical MRR@10 = 0.932; per-query sign test p = 1.000 against each). Evidence: `work/03-implementation/02-evidence/2026-06-22-bge-m3-vs-french-embeddings.md`; harness: `local-embed-tests/`.
- **Rationale:** no measurable French-legal retrieval gain from a specialist, and bge-m3 keeps multilingual reach + the learned-sparse / ColBERT upgrade path (the design's pre-stated near-tie preference, DESIGN §15). Supersedes the **embedding** portion of D4; D4's **rerank** model stays benchmark-gated (D11).
- **Caveat / retained capability:** the validation set is small and synthetic (directional, not release-gating). The re-embed + vector-index migration mechanism (W3) is **retained** for any future model change (e.g. Phase 3) — it is simply not exercised in Phase 1.

---

## Remaining validation gates (all product decisions are made)

No open *product/architecture* questions remain — only outcomes to validate against the spike (§13.3) and the eval set (§15). The **recommended answers** are recorded (open-questions review):
1. **Backend spike (D3/D18):** proceed with embedded Postgres + `pgvector` + `pg_search` — *validate, don't reselect*; it must clear every §13.3 criterion (packaging + quality), and fallbacks engage only on a hard failure (precedence in D3).
2. **Embedding model — DECIDED (D21):** `bge-m3` is locked as v1; the French-specialist bake-off is skipped after local validation (CamemBERT/Solon both tie). Re-embed/migration capability retained for any future change.
3. **Reranker (D11):** benchmark before Phase 1; ship it if the gain is material within budget, via a pluggable local/HTTP provider — else defer to Phase 3 with a recorded result.

(The **phase claim** is resolved, not open: Phase 1 = best-in-class LEGI, full juridic at Phase 2 — D8.)

All decisions settled across the 2026-06-20 reviews: **D1** (`jurisearch`), **D2** (Rust / Python-offline), **D3** (embedded Postgres + `pgvector` + `pg_search`), **D5** (endpoint embeddings incl. local `llama.cpp`), **D6** (CLI-only + JSONL), **D7** (official XML from day 1), **D11–D20**, plus the recommendations **D8/D9/D10**. The embedding model (D4) was later locked to `bge-m3` by **D21** (2026-06-22).
