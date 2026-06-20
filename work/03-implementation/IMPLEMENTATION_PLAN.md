# `jurisearch` — Implementation Plan

Date: 2026-06-20  
Status: implementation planning document  
Inputs: `work/02-conception/CONCEPTION.md`, `work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`, `work/reviews/2026-06-20-conception-readiness-review.md`, `work/reviews/2026-06-20-implementation-plan-review.md`  
Scope: execution plan only; no architecture re-decision

---

## 1. Planning Rules

This plan implements the locked conception. It must not reopen decisions already locked in `CONCEPTION.md §16` / `DECISIONS.md`:

- product name is `jurisearch`;
- runtime/search/indexing path is Rust;
- Python is offline-ingestion-only before canonical-record validation;
- interface is CLI-only with one-shot JSON and JSONL session mode;
- no MCP, no HTTP server, no `serve` daemon;
- authoritative ingestion starts from official XML / official APIs;
- authoritative index never uses derived HF datasets as input;
- selected backend is embedded Postgres + `pgvector` + `pg_search`;
- embeddings default to an OpenAI-compatible endpoint, including local loopback servers;
- Qdrant and external vector/search services are out of scope;
- remaining uncertainty is validation, not product indecision.

Fallbacks are invoked only on hard validation failure, in the locked precedence:

1. native Postgres FTS if `pg_search` packaging fails;
2. standalone Tantivy + local vector/metadata storage if embedded Postgres itself fails;
3. LanceDB only if the Postgres route fails both packaging and quality gates.

---

## 2. Target Workstreams

### W1 — Rust Workspace and Contracts

Owns project structure, shared schemas, CLI command surface, JSON outputs, errors, exit codes, and inline help.

Deliverables:

- Rust workspace with `jurisearch-core`, `jurisearch-cli`, and `jurisearch-ingest`.
- Stable source-namespaced ID types.
- Versioned request/response/error schemas.
- Compiled-in `help agent` and `help schema --json`.
- One-shot JSON discipline: JSON-only stdout, diagnostics on stderr.
- Stable exit codes: `0/2/3/4/5`.

### W2 — Evaluation Harness

Owns all quality gates. This workstream starts in Phase 0, not after features. It also owns the process for legally credible gold labels.

Deliverables:

- Golden legal retrieval tasks.
- Gold-label workflow with named legal-domain reviewers, review status, and held-out split.
- Minimum coverage categories: known-article lookup, conceptual statutory retrieval, historical `--as-of`, citation states, jurisprudence-by-facts, and statute→jurisprudence workflows.
- CLI-contract tests.
- Citation-state tests.
- Temporal edge tests.
- Token/tool-call budget checks.
- Ranking ablation framework: BM25-only, dense-only, hybrid, hybrid+authority, hybrid+rerank.
- Held-out split to avoid tuning to the test set.
- Curated vocabulary seed review process for `expand`.

### W3 — Embedded Storage Backend

Owns embedded Postgres lifecycle, schema, extensions, migrations, and fallback decision records.

Deliverables:

- Managed local Postgres child process.
- Pinned `pgvector` and `pg_search` extension installation.
- Schema for documents, chunks, vectors, graph edges, manifests, eval traces.
- Index/schema/extension migration mechanism.
- Embedding-fingerprint migration mechanism: manifest version bump, full re-embed, and vector index rebuild.
- Single-writer locking.
- Clean shutdown, crash recovery, no public exposure by default.
- Index artifact layout under `index/`.
- Cross-platform target policy recorded in Phase 0, even if v1 is Linux-only.

### W4 — Official Ingestion and Canonical Records

Owns official source parsing and canonical-record production.

Deliverables:

- Canonical JSONL/Parquet/Arrow schema.
- Official LEGI/DILA XML parser path from day one.
- Rust schema validation.
- Article-version temporal normalization.
- Structure-aware statutory chunking.
- Canonical-record retention policy: retained build artifact or reproducibly regenerated, but always manifest-traceable.
- Optional Python helpers only before canonical records.
- Regression fixtures from derived datasets as comparison-only artifacts.

### W5 — Retrieval and Ranking

Owns lexical search, dense retrieval, fusion, authority prior, query expansion, and reranking gate.

Deliverables:

- French legal BM25 through `pg_search`.
- Dense vector storage/search through `pgvector`.
- Embeddings endpoint client and fingerprint checks.
- `bge-m3` as provisional benchmark-default until Phase 1 chooses the final embedding model.
- Re-embedding and index migration coordination if the final embedding model differs from the provisional model.
- Custom Rust RRF.
- Authority weighting as ranking prior, not hard filter.
- Vocabulary expansion applied to lexical leg.
- Pluggable reranker provider: `disabled | local | http`.
- Reranker feasibility spike: model availability, tokenizer behaviour, ONNX/Candle compatibility, latency, packaging.

### W6 — Agent Commands

Owns user-visible behavior.

Deliverables:

- `search`
- `fetch`
- `cite`
- `related`
- `context`
- `expand`
- `status`
- `model fetch` / `setup`
- `session --jsonl`
- `batch --jsonl`
- `ingest` / `sync`
- `help agent`
- `help schema --json`

### W7 — Security, Compliance, and Operations

Owns privacy, licensing, provenance, local model cache, secrets, and operational reliability.

Deliverables:

- AGPL-3.0 project posture.
- AGPL-3.0 distribution/source-availability release checklist when bundling `pg_search`.
- Licence Ouverte attribution and manifest traceability.
- Pseudonymisation preservation.
- `JURISEARCH_` environment override policy.
- Secrets via env / OS keyring, never logs.
- Local model cache rule: fail rather than silent download unless explicitly allowed.
- Structured diagnostics/tracing to stderr only, never mixed into JSON stdout.

### W8 — Official API Client

Owns official API access used by `cite --online`, Judilibre ingestion, and incremental sync.

Deliverables:

- PISTE OAuth2 client-credentials flow.
- Token lifecycle and refresh.
- Sandbox vs production endpoint configuration.
- Rate-limit and backoff policy.
- Judilibre `/transactionalhistory` support for deltas.
- Upstream error mapping to stable `upstream/API` error vocabulary and exit code `5`.
- Secret handling integrated with W7.

### Workstream ↔ Phase Traceability

This matrix is authoritative for ownership. Phase tasks may run in parallel only when their dependencies are satisfied.

| Phase task | Owner | Depends on | Parallelization note |
|---|---|---|---|
| 0.1 Workspace skeleton | W1 | none | Starts first. |
| 0.2 Eval harness first cut | W2 | 0.1 schema stubs | Can run alongside 0.3/0.5 once fixture format exists. |
| 0.3 Embedded Postgres spike | W3 | 0.1 minimal workspace | Can run alongside 0.4/0.5. |
| 0.4 Embeddings endpoint contract | W5/W7 | 0.1 schema stubs | Can run alongside 0.3/0.5. |
| 0.5 Official LEGI XML ingestion spike | W4 | 0.1 schema stubs | Feeds 0.6. |
| 0.6 Baseline hybrid retrieval | W3/W5/W6 | 0.3 + 0.4 + 0.5 | Starts after backend, embeddings, and canonical subset exist. |
| 0.7 Reranker feasibility spike | W5 | 0.1 + 0.2 metric harness | Can run after eval harness skeleton. |
| 0.8 Official API client foundation | W8/W7 | 0.1 config/error stubs | Feeds 1.4, 2.1, and 2.5. |
| 1.1 Full LEGI canonicalization | W4 | 0.5 | Can run alongside 1.2. |
| 1.2 Statutory chunking/context | W4/W6 | 1.1 partial records | Feeds 1.3 and 1.4. |
| 1.3 Search pipeline hardening | W5/W6/W2 | 0.6 + 1.2 | Can iterate with eval data. |
| 1.4 Statutory citation verification | W6/W8 | 1.1 + 0.8 | `--online` depends on W8. |
| 1.5 JSONL session/batch | W1/W6 | 0.1 command registry | Can run alongside 1.1–1.4. |
| 1.6 Model cache/configuration | W7/W5 | 0.4 | Feeds 1.7. |
| 1.7 Phase 1 eval/migration gate | W2/W5/W3 | 1.1–1.6 + 0.7 | Final gate before Phase 1 claim. |
| 2.1 Judilibre ingestion | W4/W8 | 0.8 + Phase 1 schema/index | Feeds 2.3/2.4. |
| 2.2 Justice administrative ingestion | W4 | Phase 1 schema/index | Can run alongside 2.1. |
| 2.3 Graph layer | W3/W5/W6 | 2.1 + 2.2 relationship records | Feeds `related`. |
| 2.4 Decision search/fetch/context/cite | W5/W6/W8 | 2.1 + 2.2 + 2.3 | Feeds Phase 2 eval. |
| 2.5 Incremental sync | W8/W4/W3 | 0.8 + source-specific ingestion | Can run after ingestion paths exist. |
| 2.6 Phase 2 eval gate | W2/W5/W6 | 2.1–2.5 | Final gate before full juridic claim. |

---

## 3. Phase 0 — Validation Foundation

Goal: prove the chosen stack and create the quality gate infrastructure before feature build-out.

Phase 0 is complete only when the selected backend, source path, embeddings contract, CLI contract skeleton, and eval harness are all validated enough to support Phase 1.

### 0.1 Repository and Workspace Skeleton

Tasks:

- Create the Rust workspace layout.
- Add CLI binary named `jurisearch`.
- Add shared schema/version modules.
- Add minimal command router with all planned commands present.
- Add JSON output envelope and error type.
- Add placeholder `help agent` and `help schema --json` generated from the same command/schema registry.

Acceptance:

- `jurisearch --help`, `jurisearch help agent`, and `jurisearch help schema --json` work with no index.
- Every command exists, even if initially returns `not_implemented`.
- JSON mode emits valid JSON only on stdout.

### 0.2 Evaluation Harness First Cut

Tasks:

- Define eval fixture format for queries, expected IDs/citations, allowed alternates, and temporal expectations.
- Define gold-label ownership: each legal retrieval fixture has a legal-domain author/reviewer, review status, and rationale for expected IDs/citations.
- Seed minimum fixture coverage: known-article lookup, conceptual statutory retrieval, historical `--as-of`, citation states, and one end-to-end `search → fetch → cite` loop.
- Add CLI-contract tests for stdout/stderr discipline, exit codes, help completeness, and JSON schema validity.
- Add citation-state fixture format for `exact`, `normalized`, `ambiguous`, `stale_version`, `not_found`, `source_unavailable`.
- Add temporal fixtures for `valid_to = null`, 2016 reform boundaries, and same-day version changes.
- Define review process for the curated vocabulary seed lexicon used by `expand`.

Acceptance:

- Eval harness can run without a full corpus using fixtures.
- Gold labels have author/reviewer metadata and cannot be marked release-gating until reviewed.
- CI/local test command reports retrieval, citation, temporal, and CLI-contract categories separately.
- Failure output points to the broken contract or fixture.

### 0.3 Embedded Postgres Spike

Tasks:

- Start/stop a managed local Postgres child process from Rust.
- Decide and document Phase 0 platform policy, e.g. v1 Linux-only with macOS/Windows packaging policy recorded, or explicit multi-platform targets.
- Decide bundled vs downloaded-and-cached Postgres binaries and record the offline-install story.
- Bind only to Unix socket or ephemeral loopback.
- Install/pin `pgvector` with exact extension/build compatibility.
- Install/pin `pg_search` with exact extension/build compatibility.
- Create minimal documents/chunks/vector schema.
- Index a small synthetic smoke corpus first, then the target spike corpus with lexical and dense fields.
- Scale the spike to the concrete target from `DESIGN §13.3`: ~50k LEGI article versions + ~10k Judilibre decisions or decision fixtures.
- Run BM25 + vector candidate retrieval.
- Implement single-writer lock against one index directory.
- Test crash recovery and clean shutdown.
- Record index/schema/extension migration mechanics for the embedded Postgres data directory.

Acceptance:

- Meets all `DESIGN §13.3` packaging/lifecycle criteria or records a hard failure: binary acquisition/offline install, pinned extensions, private binding, single-writer lock, crash recovery, clean shutdown, migrations, and platform policy.
- No public network exposure by default.
- Warm query over the spike corpus returns stable JSON in < 500 ms for common queries.
- Platform policy is documented before Phase 1 begins.
- If `pg_search` alone fails packaging, native Postgres FTS fallback is evaluated without reopening the backend decision.
- If embedded Postgres itself fails, record the hard failure and move to the no-Postgres fallback path.

### 0.4 Embeddings Endpoint Contract

Tasks:

- Implement OpenAI-compatible `/v1/embeddings` client.
- Support hosted and local loopback base URLs.
- Use `bge-m3` as the provisional benchmark-default for Phase 0 dense work.
- Require provider/model/dimension/normalization/pooling fingerprint.
- Fail hard on dimension/fingerprint mismatch.
- Add local `llama.cpp` profile documentation and checks.
- Add in-process backend placeholder behind explicit config.
- Mark all Phase 0 dense vectors as re-embeddable if Phase 1 selects a different model.

Acceptance:

- Query embeddings work against a compatible endpoint.
- Phase 0 indexes record that `bge-m3` is provisional unless the Phase 1 eval confirms it.
- A wrong dimension fails with an actionable error.
- `127.0.0.1` endpoint is treated as configured endpoint, not as an in-process shortcut.
- In-process mode refuses missing local models unless `model fetch` or explicit download permission is used.

### 0.5 Official LEGI XML Ingestion Spike

Tasks:

- Parse a representative official LEGI XML subset.
- Preserve raw source IDs, hierarchy, status, dates, links, and source provenance.
- Emit canonical records.
- Validate canonical records in Rust.
- Normalize open-ended validity to `valid_to = null` while preserving raw source value.
- Generate structural article chunks.

Acceptance:

- Canonical records index and search with no Python in the query/index path.
- Derived datasets are not accepted as authoritative ingestion input.
- Invalid canonical records fail validation with actionable diagnostics.

### 0.6 Baseline Hybrid Retrieval

Tasks:

- Insert canonical LEGI subset into Postgres.
- Build BM25 fields through `pg_search`.
- Store dense vectors through `pgvector` using the provisional `bge-m3` fingerprint.
- Implement exact temporal prefilter.
- Implement custom RRF.
- Add basic `search` and `fetch` over the subset.

Acceptance:

- `search` returns compact IDs, citations, snippets, source URLs, validity blocks, scores, and cursors.
- `fetch` returns full text for selected IDs.
- Historical `--as-of` queries do not leak current law in fixtures.
- Dense index can be fully re-embedded and rebuilt from canonical records without losing provenance.

### 0.7 Reranker Feasibility Spike

Tasks:

- Test `bge-reranker-v2-m3` as the first candidate without hardwiring it as the only option.
- Verify tokenizer availability and compatibility.
- Spike local inference through `ort` and/or Candle.
- Measure top-K reranking latency on fused candidate sets.
- Validate packaging implications for local models.
- Validate HTTP rerank provider shape as fallback if local packaging lags.

Acceptance:

- Phase 1 has enough data to decide whether reranking can ship locally, over HTTP, or not at all.
- Reranker adoption remains eval-gated; the spike does not force adoption.
- If local inference is infeasible but quality gain is material, HTTP provider remains available.

### 0.8 Official API Client Foundation

Tasks:

- Implement PISTE OAuth2 client-credentials auth.
- Implement token lifecycle and refresh.
- Support sandbox and production endpoint configuration.
- Implement rate-limit handling, backoff, and retry policy.
- Map upstream/API failures to stable error objects and process exit code `5`.
- Add Judilibre `/transactionalhistory` client support for Phase 2 deltas.
- Integrate secret loading with W7 without logging credentials.

Acceptance:

- Sandbox auth and a representative official API call work.
- Rate-limit and upstream-error paths are testable without leaking secrets.
- `cite --online`, Judilibre ingestion, and `sync --since` have a shared client rather than bespoke API code.

Phase 0 exit gate:

- Backend stack passes acceptance or fallback path is formally triggered.
- Eval harness exists and gates CLI contract, temporal correctness, citation states, and ranking metrics.
- Official XML ingestion path produces valid searchable records.
- Embeddings fingerprint guard works.
- Official API client foundation handles sandbox auth, rate limits, and upstream error mapping.
- Reranker feasibility data is available for the Phase 1 adoption gate.
- Platform policy and backend migration mechanics are recorded.
- No locked product decision remains ambiguous.

---

## 4. Phase 1 — Production-Quality LEGI Search

Goal: ship best-in-class statutory search over official LEGI XML.

### 1.1 Full LEGI Canonicalization

Tasks:

- Expand parser coverage to full official LEGI code corpus.
- Normalize hierarchy into `hierarchy_path`.
- Build version groups across article versions.
- Preserve statuses: `VIGUEUR`, `MODIFIE`, `ABROGE`, `ABROGE_DIFF`.
- Record source dataset version, build date, coverage, schema version, parser version, and source files.
- Implement canonical-record retention policy: retain canonical records as a build artifact or document deterministic regeneration, with manifest traceability either way.

Acceptance:

- `status` reports LEGI coverage, freshness, source versions, and manifest.
- Rebuild from same inputs produces equivalent canonical records and index manifest.
- Canonical-record retention/regeneration policy is explicit and tested.

### 1.2 Statutory Chunking and Context

Tasks:

- Chunk article versions structurally.
- Split long articles only on legal substructure such as alinéas and enumerations.
- Repeat article header and hierarchy context where needed for embedding.
- Implement `context` for ancestry and sibling articles with `--as-of`.

Acceptance:

- No chunk crosses legal hierarchy boundaries.
- `context` reconstructs section neighbourhood at the requested date.
- Eval includes long-article and hierarchy-sensitive cases.

### 1.3 Search Pipeline Hardening

Tasks:

- Tune French legal analyzer for elision, accents, statutory references, and legal stopwords/boosters.
- Implement vocabulary expansion seed lexicon and `expand`, with legal-domain review/sourcing for seed terms.
- Implement temporal prefilter before fusion.
- Implement RRF and authority prior.
- Add pagination and truncation guidance.
- Add `--format concise|detailed`.

Acceptance:

- BM25-only, dense-only, hybrid, hybrid+authority ablations are measurable.
- Expansion is logged in `expanded_terms`.
- Vocabulary seed entries carry source/review metadata.
- Exact statutory references such as article numbers remain precise.
- Authority prior never overrides explicit filters.

### 1.4 Citation Verification for Statutes

Tasks:

- Implement `cite` for internal IDs, `LEGIARTI`, `LEGITEXT`, `LEGISCTA`, `NOR`, and free-text article citations.
- Implement local-by-default resolution.
- Implement optional `--online` confirmation through the shared official API client where configured.
- Implement `--strict`.
- Return all six citation states.

Acceptance:

- Fabricated, stale, ambiguous, and malformed citations are classified correctly.
- Dated citations resolve against the correct historical version.
- `--online` uses the shared OAuth2/rate-limited client and maps upstream errors to exit code `5`.
- `--strict` fails anything other than `exact` / `normalized`.

### 1.5 JSONL Session and Batch

Tasks:

- Implement request/response envelope with echoed `id`.
- Preserve input order.
- Keep stdout JSONL-only and diagnostics on stderr.
- Support `help` and `help schema` inside session.
- Implement non-fatal malformed-line errors.
- Implement explicit `exit` acknowledgement.
- Implement finite `batch --jsonl`.

Acceptance:

- Sequential multi-call agent loop runs without process restart.
- Malformed input does not kill the session unless fatal mode is explicit.
- Session and one-shot payloads share schemas.

### 1.6 Model Cache and Configuration

Tasks:

- Implement config loading from `~/.config/jurisearch/config.toml`.
- Implement `JURISEARCH_` environment overrides.
- Implement embedding provider config and secret handling.
- Implement `model fetch` / `setup` for in-process/local models.
- Implement `status` checks for missing local models and endpoint reachability.

Acceptance:

- Hosted and local loopback embedding endpoints both work.
- Missing in-process models fail with an actionable error.
- Secrets are never written into manifest or logs.

### 1.7 Phase 1 Evaluation and Migration Gate

Tasks:

- Complete LEGI eval set with realistic statutory research tasks.
- Benchmark embedding candidates: `bge-m3`, French specialists, and at least one strong hosted multilingual model.
- Decide final embedding model by post-fusion legal metrics.
- If the winner differs from provisional `bge-m3`, run the explicit embedding migration: manifest fingerprint change, full-corpus re-embed, vector index rebuild, and schema/version bump.
- Evaluate reranker adoption before release.
- Use the Phase 0 reranker feasibility spike to decide local vs HTTP vs disabled provider before the Phase 1 claim.
- Measure token/tool-call budget for `search → fetch → cite`.

Acceptance:

- Phase 1 may claim best-in-class LEGI/statutory search only if eval gates pass.
- The index fingerprint matches the selected final embedding model before release.
- Any re-embedding migration is reproducible from canonical records and recorded in the manifest.
- If reranker is not adopted, the eval result is recorded.
- Docs and `status` do not claim full French juridic coverage before Phase 2.

---

## 5. Phase 2 — Jurisprudence and Full French Juridic Search

Goal: add Judilibre and justice administrative so the product can claim best-in-class French juridic search across statutes and jurisprudence.

### 2.1 Judilibre Ingestion

Tasks:

- Ingest Judilibre decisions from official API/export sources.
- Use the shared official API client for OAuth2, sandbox/prod config, rate-limit/backoff, and upstream error mapping.
- Preserve ECLI, pourvoi, court, chamber, formation, date, publication, solution, taxonomy raw keys, full text, zones, texts applied, and related decisions.
- Reassemble non-sequential zone fragments by zone identity.
- Store `texts_applied` and `related_decisions` as relationships, not zones.
- Preserve source pseudonymisation and do not enrich records with re-identifying cross-source data.

Acceptance:

- Official zones are primary chunk boundaries.
- Regex/heuristic chunking is flagged when offsets are absent.
- `rapprochements` and applied texts produce graph edges, not chunks.
- Pseudonymisation tests assert no re-identification and no cross-source linking designed to defeat source pseudonymisation.

### 2.2 Justice Administrative Ingestion

Tasks:

- Ingest Conseil d'État, CAA, and TA open data.
- Preserve `CETATEXT`, ECLI, court, date, source URLs, and source provenance.
- Map available structure to canonical decision records.
- Implement fallback zone/part chunking with provenance.

Acceptance:

- Administrative decisions are searchable and citeable.
- Coverage and freshness are reported by `status`.

### 2.3 Graph Layer

Tasks:

- Store graph edges in Postgres.
- Implement `related` over bounded 1–2 hop traversal.
- Support `cites`, `interpreted-by`, `appeals`, `applies-article`, `rapprochements`.
- Preserve `edge_source = publisher | inferred`.
- Add citation parser for inferred links.

Acceptance:

- Graph returns ranked candidate neighbours with authority signals.
- Graph never asserts *jurisprudence constante*.
- Publisher-provided relationships are distinguishable from inferred links.

### 2.4 Decision Search, Fetch, Context, and Cite

Tasks:

- Extend search filters for court, chamber, formation, publication, decision date.
- Implement decision `fetch --part` for motivations, dispositif, moyens, visa, summary.
- Implement decision `context` over neighbouring zones.
- Extend `cite` to ECLI, pourvoi, and `CETATEXT`, with optional `--online` through the shared API client where available.
- Extend authority scoring for court tier, formation, publication level, and recency.

Acceptance:

- Agent can run statute-to-jurisprudence workflows.
- `cite` verifies decision identifiers and free-text decision citations.
- Authority remains a ranking signal, not a hard filter.

### 2.5 Incremental Sync

Tasks:

- Implement `sync --source legi|judilibre|ja --since`.
- Use official deltas/transaction histories through the shared official API client where available.
- Update manifests and coverage.
- Preserve deterministic rebuild path.

Acceptance:

- `status` reports exact corpus freshness after sync.
- Sync cannot silently mix incompatible schema/model/source versions.

### 2.6 Phase 2 Evaluation Gate

Tasks:

- Add jurisprudence retrieval tasks for Cassation and administrative courts.
- Add statute + jurisprudence agent workflows.
- Test zone-aware fetch and citation verification.
- Include legal-domain review for ECLI/pourvoi/CETATEXT gold labels and jurisprudence-by-facts expected decisions.
- Tune authority weights using eval, not hard-coded intuition.

Acceptance:

- Full French juridic search claim is allowed only after Phase 2 gates pass.
- Results remain source-backed, temporally correct, citeable, and token-efficient.

---

## 6. Phase 3+ — Ranking Depth and Corpus Expansion

Goal: improve ranking and broaden corpus coverage without weakening core invariants.

Potential work:

- Reranker tuning or distillation.
- Learned sparse retrieval using `bge-m3` sparse output or SPLADE-like models.
- ColBERT-style late interaction if evals justify cost.
- EU law, KALI, BOFIP, doctrine, and other designed-for corpora.
- More sophisticated agent-workflow benchmarks.

Acceptance:

- Every expansion preserves official provenance, temporal semantics, citation verification, and agent contract stability.
- New corpora are introduced only with source-specific canonicalization and eval coverage.

---

## 7. Cross-Cutting Test Strategy

### Unit Tests

- ID parsing and normalization.
- Temporal interval semantics.
- Citation parsing and state classification.
- Schema validation.
- RRF and authority scoring.
- Vocabulary expansion.
- Official API error mapping.
- Embedding fingerprint migration decisions.

### Integration Tests

- Embedded Postgres lifecycle.
- Extension installation and migrations.
- Embedding-model re-embed and index migration.
- Ingestion → canonical records → index → search.
- Embeddings fingerprint mismatch.
- Official API OAuth2 sandbox, token refresh, rate-limit/backoff, and upstream error handling.
- JSONL session protocol.
- CLI stdout/stderr discipline.
- Pseudonymisation preservation: no re-identification enrichment or cross-source linking that defeats source pseudonymisation.

### Golden Tests

- Search result JSON shape.
- Help text completeness.
- `help schema --json` schema validity.
- Citation verification states.
- Temporal historical queries.
- Legal gold labels with reviewer metadata.
- Jurisprudence-by-facts expected decisions.

### Performance Tests

- Warm query latency.
- Index build throughput.
- Embedding endpoint throughput.
- Reranker latency.
- Reranker local-vs-HTTP provider overhead.
- JSONL session amortization.

---

## 8. Release Gates

### Phase 0 Gate

- Backend validated or fallback formally triggered.
- Eval harness exists.
- Official LEGI XML subset indexes and searches.
- CLI contract skeleton works.
- Embedding endpoint contract works.
- Embedded Postgres spike hits < 500 ms warm JSON on the target spike corpus or records a hard failure.
- Official API client foundation works against sandbox and maps upstream failures.
- Reranker feasibility spike feeds Phase 1 adoption decision.
- Platform policy and migration mechanics are documented.

### Phase 1 Gate

- Full LEGI statutory corpus path works from official XML.
- Temporal correctness eval passes.
- Citation verification eval passes.
- Hybrid retrieval beats BM25-only and dense-only baselines on legal tasks.
- Final embedding model is selected; if it differs from provisional `bge-m3`, re-embedding/index migration is complete.
- Reranker adoption/deferral is backed by the feasibility spike and eval result.
- CLI contract eval passes.
- `status` accurately reports coverage and freshness.
- Phase 1 claims only LEGI/statutory best-in-class.

### Phase 2 Gate

- Judilibre and justice administrative ingestion work from official sources.
- Decision zone chunking and graph relationships pass eval.
- Pseudonymisation preservation tests pass.
- Jurisprudence citation verification passes.
- Statute + jurisprudence agent workflows pass budget and quality gates.
- Full French juridic best-in-class claim becomes allowed only here.

---

## 9. Risk Register

| Risk | Impact | Mitigation |
|---|---:|---|
| `pg_search` packaging fails with embedded Postgres | High | Use locked fallback: native Postgres FTS first; do not jump to Qdrant. |
| Embedded Postgres process management is too heavy | High | Trigger no-Postgres fallback only after lifecycle/packaging hard failure. |
| Embedding endpoint model mismatch | High | Hard fingerprint/dimension checks; actionable errors. |
| Embedding-model winner differs from provisional `bge-m3` | High | Budget full re-embed + manifest/index migration into Phase 1. |
| Official XML edge cases are more complex than fixtures | High | Phase 0 parser spike on real representative XML; canonical validation. |
| Official API coverage-date drift or rate limits block verification/sync | High | Bulk dumps for full builds; APIs only for deltas/verification; backoff, sandbox/prod config, and freshness reporting. |
| Eval set too small or generic | High | Build production-grade legal evals from Phase 0; legal-domain review, minimum category coverage, held-out split. |
| Reranker local packaging lags | Medium | Keep HTTP rerank provider; quality gate decides adoption. |
| Python helper leaks into runtime | Medium | Rust test proves canonical records index/search without Python. |
| Agent contract drifts from help/schema | Medium | Generate help/schema from the same registry where possible; eval completeness. |
| Derived datasets accidentally influence production index | Medium | Validator rejects non-authoritative source labels for production indexes. |
| AGPL-3.0 distribution obligations are missed | Medium | Release checklist ensures source availability for the combined distributed work when bundling `pg_search`. |

---

## 10. Immediate Next Work

Recommended first execution batch:

1. Scaffold the Rust workspace and CLI command registry.
2. Define schema/error/exit-code contracts and compile `help schema --json`.
3. Build the eval harness shell with CLI-contract tests.
4. Record phase/workstream owners and dependency gates in issue tracking from the matrix above.
5. Spike embedded Postgres + `pgvector` + `pg_search` against the concrete packaging and latency checklist.
6. Spike official LEGI XML → canonical record → Rust validation.
7. Implement endpoint embeddings fingerprint checks with provisional `bge-m3` and migration metadata.
8. Implement the shared official API client foundation.
9. Run the reranker feasibility spike.
10. Build the minimal LEGI subset search path: BM25 + dense + RRF + `search`/`fetch`.

This order keeps the hard validation gates ahead of feature work and prevents the implementation from drifting into a toy path.

---

## 11. References

- `work/02-conception/CONCEPTION.md`
- `work/01-design/DESIGN.md`
- `work/01-design/DECISIONS.md`
- `work/01-design/RESEARCH.md`
- `work/reviews/2026-06-20-conception-readiness-review.md`
- `work/reviews/2026-06-20-implementation-plan-review.md`
- `work/00-foundation/search.md`
- `work/00-foundation/assessment.md`
