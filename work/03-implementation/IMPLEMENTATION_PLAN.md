# `jurisearch` — Implementation Plan

Date: 2026-06-20  
Updated: 2026-06-21 for ingestion reuse findings in `work/notes/2026-06-21-ingestion-reuse-impact-on-implementation-plan.md`; W8/0.8 auth revised to PISTE API-Key (`KeyId`) per `work/03-implementation/00-setup/PREREQUISITES.md §6`
Status: implementation planning document  
Inputs: `work/02-conception/CONCEPTION.md`, `work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`, `work/reviews/2026-06-20-conception-readiness-review.md`, `work/reviews/2026-06-20-implementation-plan-review.md`, `work/notes/2026-06-21-juridocs-ingestion-reuse.md`, `work/notes/2026-06-21-ingestion-reuse-impact-on-implementation-plan.md`
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

Owns the quality-gate harness, reporting, and legally credible retrieval evals. This workstream starts in Phase 0, not after features. Ingest-health gates are shared: W2 owns the harness/reporting, W3 owns schema/projection metrics, W4 owns ingestion replay inputs, and W7 owns operational runbooks.

Deliverables:

- Golden legal retrieval tasks.
- Gold-label workflow with named legal-domain reviewers, review status, and held-out split.
- Minimum coverage categories: known-article lookup, conceptual statutory retrieval, historical `--as-of`, citation states, jurisprudence-by-facts, and statute→jurisprudence workflows.
- CLI-contract tests.
- Citation-state tests.
- Temporal edge tests.
- Token/tool-call budget checks.
- Ranking ablation framework: BM25-only, dense-only, hybrid, hybrid+authority, hybrid+rerank.
- Reranker adoption gate linked to `work/03-implementation/02-evidence/2026-06-21-reranker-feasibility.md`, with BM25-only / dense-only / hybrid / hybrid+rerank measured before enabling rerank by default.
- Held-out split to avoid tuning to the test set.
- Curated vocabulary seed review process for `expand`.
- Ingest-health gate harness and reports: latest completed run per source, failed-member/error thresholds, projection/embedding coverage, and replay-snapshot diffs.

### W3 — Embedded Storage Backend

Owns embedded Postgres lifecycle, schema, extensions, migrations, and fallback decision records.

Deliverables:

- Managed local Postgres child process.
- Pinned `pgvector` and `pg_search` extension installation.
- Schema for documents, chunks, vectors, graph edges, manifests, eval traces, and operational ingest tables.
- `ingest_run`, `ingest_member`, and `ingest_error` tables with recovery-compatibility fields (`parser_version`, `schema_version`, `source_payload_hash`, `code_version`) stored as metadata, not as member identity keys.
- Projection/embedding coverage metrics used by ingest-health gates.
- Chunk provenance fields: `source_payload_hash`, `source_fields`, `chunk_builder_version`, and embedding fingerprint.
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
- Archive planning for official dumps: baseline/delta precedence, deterministic replay order, mixed-dataset rejection, and streaming `.tar.gz` member reads with configurable byte caps.
- Rust schema validation.
- Article-version temporal normalization.
- LEGI temporal ID/field contract: canonical article IDs use `legi:<LEGIARTI>@<valid_from>`, `version_group` groups article versions, and raw source end-date/sentinel values are preserved as `valid_to_raw` when normalized to `valid_to = null`.
- Structure-aware statutory chunking.
- Versioned canonical text-assembly contracts per document kind.
- Publisher link extraction from LEGI (`LIEN`, `LIEN_ART`, `LIEN_SECTION_TA`, `LIEN_TXT`, inline anchors) into canonical graph-edge records with `edge_source = publisher`.
- Ingest run/member/error accounting, resume-after-interruption, explicit unsupported-root counters, and optional quarantine of failed payloads.
- Derived-projection discipline: canonical/source writes complete before search/vector/graph projections, and query access is blocked or clearly marked until gates pass.
- Canonical-record retention policy: retained build artifact or reproducibly regenerated, but always manifest-traceable.
- Optional Python helpers only before canonical records.
- Regression fixtures from derived datasets as comparison-only artifacts.

### W5 — Retrieval and Ranking

Owns lexical search, dense retrieval, fusion, authority prior, query expansion, and reranking gate.

Deliverables:

- French legal BM25 through `pg_search`.
- Dense vector storage/search through `pgvector`.
- Embeddings endpoint client and fingerprint checks.
- Embedding input preflight against tokenizer or endpoint-specific token budget before document embeddings are written; over-budget build-time request text is truncated to the configured endpoint budget and reported without changing stored chunk text or chunk IDs.
- Build-time dense-projection **endpoint pool**: least-outstanding-requests dispatch across N bge-m3-compatible endpoints (extends `jurisearch ingest embed-chunks`), batched, resumable via projection-coverage. Measured 2026-06-22: LAN nodes are load-volatile, while OpenRouter `baai/bge-m3` is fingerprint-compatible (1024-d normalized, cos ~0.999972 vs local) and stable at C=16 (~292 texts/s), projecting the ~1.85 M-chunk LEGI corpus in ~1.8 h by itself. Query-time stays on a single local endpoint; OpenRouter is a build-time LEGI projection accelerator, with Phase 2 Judilibre egress to be reconsidered. See `work/03-implementation/00-setup/embeddings-endpoint.md`.
- `bge-m3` as the **locked v1 embedding model** (DECISIONS D21; validated vs CamemBERT/Solon 2026-06-22 — `work/03-implementation/02-evidence/2026-06-22-bge-m3-vs-french-embeddings.md`).
- Re-embedding / vector-index migration **capability retained** for any future model change (Phase 3); not exercised in Phase 1.
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
- `status`, including ingest-health, latest-run, coverage, and recovery-warning fields.
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
- Safe-mode ingest and rollback runbooks for projection/backfill/write-path failures.
- Structured diagnostics/tracing to stderr only, never mixed into JSON stdout.

### W8 — Official API Client

Owns official API access used by `cite --online`, Judilibre ingestion, and incremental sync.

Deliverables:

- **Dual PISTE Axway auth — both schemes required (tested 2026-06-21).** **API-Key (`KeyId` header)** for **Judilibre** (verified: prod `Juridia` → "JUDILIBRE 1.0.0", live `/search` → 200). **OAuth2 client-credentials (Bearer)** for **Légifrance**, which **rejects `KeyId`** (400-empty) but works via OAuth — **verified end-to-end** (token + `/search` → 200, prod + sandbox; `scope=openid`). See `work/03-implementation/00-setup/PREREQUISITES.md §6` for the recipe.
- OAuth2 token lifecycle/refresh — applies to the Légifrance (`cite --online`) path only; Judilibre's keyed path needs none.
- Per-app, per-API subscription + endpoint configuration. Subscriptions are per-app: prod `Juridia` → Judilibre + Légifrance; sandbox app → Légifrance only (Judilibre `/search` → 403 until subscribed).
- Rate-limit and backoff policy.
- Judilibre `/transactionalhistory` support for deltas.
- Upstream error mapping to stable `upstream/API` error vocabulary and exit code `5`.
- Secret handling integrated with W7 — API key/secret read from env/keyring and sent as `KeyId`.

### Workstream ↔ Phase Traceability

This matrix is authoritative for ownership. Phase tasks may run in parallel only when their dependencies are satisfied.

| Phase task | Owner | Depends on | Parallelization note |
|---|---|---|---|
| 0.1 Workspace skeleton | W1 | none | Starts first. |
| 0.2 Eval harness first cut | W2 | 0.1 schema stubs | Can run alongside 0.3/0.5 once fixture format exists. |
| 0.3 Embedded Postgres spike | W3 | 0.1 minimal workspace | Can run alongside 0.4/0.5. |
| 0.4 Embeddings endpoint contract | W5/W7 | 0.1 schema stubs | Can run alongside 0.3/0.5. |
| 0.5 Official LEGI XML ingestion spike | W4 | 0.1 schema stubs | Feeds 0.6. |
| 0.5a Archive precedence + streaming module | W4 | 0.1 schema stubs | Ports proven baseline/delta ordering and streaming reader semantics before full-corpus work. |
| 0.6 Baseline hybrid retrieval | W3/W5/W6 | 0.3 + 0.4 + 0.5 | Starts after backend, embeddings, and canonical subset exist. |
| 0.7 Reranker feasibility spike | W5 | 0.1 + 0.2 metric harness | Can run after eval harness skeleton. |
| 0.8 Official API client foundation | W8/W7 | 0.1 config/error stubs | Feeds 1.4, 2.1, and 2.5. |
| 1.0 Ingest run/member/error accounting + resume/quarantine | W3/W4/W7/W2 | 0.3 + 0.5 + 0.5a | Gates 1.1; W2 supplies reporting hooks. |
| 1.1 Full LEGI canonicalization | W4 | 1.0 | Can run alongside 1.2 once operational accounting exists. |
| 1.2 Statutory chunking/context | W4/W6 | 1.1 partial records | Feeds 1.3 and 1.4. |
| 1.3 Search pipeline hardening | W5/W6/W2 | 0.6 + 1.2 | Can iterate with eval data. |
| 1.4 Statutory citation verification | W6/W8 | 1.1 + 0.8 | `--online` depends on W8. |
| 1.5 JSONL session/batch | W1/W6 | 0.1 command registry | Can run alongside 1.1–1.4. |
| 1.6 Model cache/configuration | W7/W5 | 0.4 | Feeds 1.7. |
| 1.7 Phase 1 eval/migration gate | W2/W5/W3 | 1.0 + 1.1–1.6 + 0.7 | Final gate before Phase 1 claim. |
| 2.1 Judilibre ingestion | W4/W8 | 0.8 + Phase 1 schema/index | Feeds 2.3/2.4. |
| 2.2 Justice administrative ingestion | W4 | Phase 1 schema/index | Can run alongside 2.1. |
| 2.2a Optional DILA bulk jurisprudence adapter | W4 | 2.1 + 2.2 stable | Explicit scope decision; coverage fallback only, not a zone-accurate replacement. |
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
- Define gold-label ownership and provenance: each legal retrieval fixture records `drafted_by` (may be an LLM), `verified_against` (official Légifrance/Judilibre API), a named legal-domain `reviewer` + review status + rationale, and a `tier` (`dev` | `release_gating`).
- Implement the gold-label workflow: **LLM-draft → verify against the official source (not model memory) → named-human sign-off**; only human-signed, source-verified labels are `release_gating`. Add an LLM adversarial/coverage pass that flags human↔model and label↔retrieval disagreements for re-review.
- Seed minimum fixture coverage: known-article lookup, conceptual statutory retrieval, historical `--as-of`, citation states, and one end-to-end `search → fetch → cite` loop.
- Add CLI-contract tests for stdout/stderr discipline, exit codes, help completeness, and JSON schema validity.
- Add citation-state fixture format for `exact`, `normalized`, `ambiguous`, `stale_version`, `not_found`, `source_unavailable`.
- Add temporal fixtures for `valid_to = null`, `valid_to_raw` sentinel preservation, 2016 reform boundaries, same-day version changes, and `MODIFIE` / `ABROGE` transitions.
- Define review process for the curated vocabulary seed lexicon used by `expand`.
- Define the ingest-health gate report envelope in JSON and Markdown, with placeholder categories for W3 projection metrics, W4 replay inputs, and W7 recovery/runbook checks.

Acceptance:

- Eval harness can run without a full corpus using fixtures.
- Gold labels carry provenance (`drafted_by`, `verified_against` the official API, named `reviewer`) and a `tier`; a label is `release_gating` only after **official-source verification AND human sign-off**. LLM-only labels are never release-gating; LLM-drafted + source-checked labels are allowed in the `dev` tier.
- CI/local test command reports retrieval, citation, temporal, and CLI-contract categories separately.
- Ingest-health reports can emit pending/empty categories before the full corpus exists, without treating unavailable W3/W4/W7 metrics as passed.
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

Current status (2026-06-21):

- Done: the disposable extension smoke is codified in `jurisearch-storage`. It starts a pgrx-managed local PostgreSQL, preloads `pg_search`, creates `vector` and `pg_search`, and verifies a pgvector nearest-neighbor query.
- Done: the first durable lifecycle slice is codified in `jurisearch-storage`: persistent index-root PGDATA, process-lifetime storage lock before touching PGDATA, conservative orphan reclaim under that lock, app database bootstrap, extension bootstrap, session advisory lock, clean stop, and restart/concurrent-owner smoke coverage.
- Done: the first migration/schema slice is codified in `jurisearch-storage`: versioned `schema_migrations`, extension creation through migration v1, minimal `documents`, `chunks`, `chunk_embeddings`, `graph_edges`, and `index_manifest` tables, and restart/idempotency smoke coverage with a 1024-d vector insert.
- Done: the first retrieval smoke is codified in `jurisearch-storage`: migration v2 adds a `pg_search` BM25 index on chunks, and the smoke verifies BM25 lexical candidate retrieval plus pgvector nearest-neighbor retrieval over synthetic chunks.
- Done: Phase 0 platform and offline-install policy is recorded in `00-setup/storage-backend-policy.md`: Linux x86_64, PostgreSQL 18 through a pgrx-managed or pgrx-like `pg_config` prefix, matching extension artifacts, and explicit offline pre-stage requirements.
- Done: target spike-corpus retrieval is codified in `jurisearch-storage` as an opt-in ignored test. `target_spike_corpus.rs` seeds 50k LEGI article fixtures + 10k Judilibre decision fixtures, bulk-builds an IVFFlat vector index for the fixture, and verifies stable hybrid JSON with BM25 + pgvector candidates. Local run on 2026-06-21 after adding inactive temporal near-neighbors: lexical 18.90 ms, dense 102.77 ms, full warm JSON 132.04 ms.
- Caveat: the target fixture is a structural plumbing floor, not a real capacity benchmark. It uses one chunk per fixture document and a deliberately simple two-vector distribution, so real varied embeddings and multi-chunk Judilibre decisions still need Phase 1/2 performance measurement. The spike also shells out through `psql`; the serving path should reuse a libpq client or pool.
- Remaining before 0.3 is complete: none from the current spike checklist; move next to 0.4 unless review finds a gap.

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
- Use `bge-m3` as the locked dense embedding model (DECISIONS D21) for Phase 0 and beyond.
- Require provider/model/dimension/normalization/pooling fingerprint.
- Fail hard on dimension/fingerprint mismatch.
- Add local `llama.cpp` profile documentation and checks.
- Add in-process backend placeholder behind explicit config.
- Keep Phase 0 dense vectors re-embeddable in principle (migration capability), though no Phase 1 model change is planned (`bge-m3` locked, D21).

Acceptance:

- Query embeddings work against a compatible endpoint.
- Phase 0 indexes record `bge-m3` as the locked model (D21); the manifest provisional flag is set accordingly.
- A wrong dimension fails with an actionable error.
- `127.0.0.1` endpoint is treated as configured endpoint, not as an in-process shortcut.
- In-process mode refuses missing local models unless `model fetch` or explicit download permission is used.

Current status (2026-06-21):

- Done: `jurisearch-embed` implements the OpenAI-compatible embeddings client, embedding fingerprint/manifest structs, hard fingerprint and dimension checks, local-loopback base-url classification, and the explicit in-process missing-model guard.
- Done: `jurisearch status` now reports the Phase 0 embedding manifest fields: provider, base URL, base-url class, model, dimension, normalization, pooling, provisional status, and re-embeddable status.
- Done: local `llama.cpp` compatibility is codified as an ignored live endpoint test. Local run on 2026-06-21: `cargo test -p jurisearch-embed --test live_endpoint -- --ignored --nocapture` passed against `http://127.0.0.1:8097/v1`, returning a 1024-d normalized `bge-m3` vector.
- Remaining before 0.4 is complete: none from the current endpoint-contract checklist; move next to 0.5 unless review finds a gap.

### 0.5 Official LEGI XML Ingestion Spike

Tasks:

- Parse a representative official LEGI XML subset.
- Preserve raw source IDs, hierarchy, status, dates, links, and source provenance.
- Emit canonical records.
- Validate canonical records in Rust.
- Add typed parser errors for XML, missing required fields, invalid dates, invalid IDs, and unsupported roots.
- Normalize open-ended validity to `valid_to = null` while preserving raw source value in `valid_to_raw`.
- Generate structural article chunks.

Acceptance:

- Canonical records index and search with no Python in the query/index path.
- Derived datasets are not accepted as authoritative ingestion input.
- Invalid canonical records fail validation with actionable diagnostics.
- Unsupported XML roots are explicitly classified and counted; they are not reported as successful inserts.

Current status (2026-06-21):

- Done: the first official LEGI parser slice is codified in `jurisearch-ingest::legi`. It parses `ARTICLE` XML into a `CanonicalDocument` record, validates typed `LEGIARTI` IDs and required fields, normalizes open-ended `DATE_FIN` sentinels to `valid_to = null` while preserving `valid_to_raw`, preserves optional status plus nature/type, hierarchy context, source URL, archive/member provenance, and a SHA-256 source payload hash.
- Done: structured parser errors now cover XML errors, missing required fields, invalid dates, invalid IDs, and unsupported roots. Unsupported roots return an explicit `UnsupportedRoot` classification instead of pretending to ingest.
- Done: representative real LEGI archive smoke is codified as an ignored integration test. Local run on 2026-06-21 used `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz`, parsed 25 real `ARTICLE` members after visiting 27 XML members, preserved raw archive-member SHA-256 hashes, classified `TEXTELR`/`TEXTE_VERSION` as unsupported roots, and hit no non-predefined named-entity failures in that sample.
- Done: `DTD/jorf/jorf_article.dtd` does not include `META_ARTICLE/ETAT`, and a representative JORF-flavoured member inspected during the smoke showed an empty status element. Status is therefore optional; canonical versions record `etat=absent` when the source carries no status value, while the real archive smoke verifies status is preserved when present.
- Done: article parsing now emits canonical publisher graph-edge candidates from `LIEN`, `LIEN_ART`, `LIEN_SECTION_TA`, `LIEN_TXT`, and inline anchors with `edge_source = publisher`, raw DILA attributes, source-member provenance, and conservative `refers_to` relation. The ignored real-archive smoke asserts publisher edges are emitted from the sampled official members.
- Done: article parsing now emits one structural `article_body` chunk per article version with full hierarchy context, source-field provenance, raw source payload hash, `chunk_builder_version = legi_article_structural:v1`, and paragraph/list/line-break boundaries preserved in the chunk body. The ignored real-archive smoke asserts each sampled article emits the structural chunk.
- Follow-up for 1.2/materialization: storage projection must persist emitted chunk provenance (`contextualized_body`, `chunking`, `boundary`, and `hierarchy_path`) via dedicated columns or chunk-level JSON before canonical chunks are inserted into Postgres; the current minimal `chunks` table does not yet carry all of these fields.
- Done: Phase 0.5 root scope and DTD-required-field audit are recorded in `work/03-implementation/02-evidence/2026-06-21-legi-phase0-root-dtd-audit.md` using `/home/pierre/Apps/juridocs/opendata` and `/home/pierre/Apps/juridocs/DTD`. `SECTION_TA`, `TEXTELR`/`TEXTEKALI`, and `TEXTE_VERSION` are intentionally deferred from Phase 0.5 canonical output and are covered by explicit unsupported-root tests.
- Follow-up for 1.0 ingest accounting: unsupported-root handling must persist per-root counts (`SECTION_TA`, `TEXTELR`/`TEXTEKALI`, `TEXTE_VERSION`, etc.) in ingest-run/member/error reporting rather than only classifying them in parser tests.
- Remaining before 0.5 is complete: none. Phase 1 must add profile-specific field retention, `SECTION_TA`/text-structure hierarchy assembly, and text-level canonicalization before claiming full LEGI canonicalization.

### 0.5a Archive Precedence and Streaming Module

Scope qualifier: Phase 0 implements the planner/reader semantics and deterministic-ordering tests needed to de-risk full-corpus ingestion; production ingest orchestration remains Phase 1+ work.

Tasks:

- Implement `jurisearch-ingest::archive` with an official-source enum, starting with `legi`.
- Parse official archive filenames and reject unrecognized or mixed-source plans.
- Select the latest baseline archive and order deltas strictly after that baseline by timestamp.
- Stream `.tar.gz` XML members through a bounded reader rather than materializing the archive.
- Add configurable member byte caps and record the active cap in the ingest manifest.
- Add deterministic ordering tests for baseline/delta selection, same-day deltas, mixed sources, and missing baseline errors.

Acceptance:

- A dry-run archive plan reports selected baseline, ordered deltas, skipped files, and source enum.
- Streaming member reads preserve deterministic archive order and enforce byte caps.
- Archive planning is recorded as a manifest artifact and can be reused by full ingest and `sync`.

### 0.6 Baseline Hybrid Retrieval

Scope qualifier: this step is being landed in slices. The current slice proves storage projection, retrieval, CLI search/fetch wiring, dense rebuild finalization, and endpoint-driven chunk embedding over a small official LEGI-compatible subset.

Tasks:

- Done: `jurisearch-storage::projection` inserts parser-produced canonical LEGI documents into Postgres, including document metadata, full canonical JSON/provenance, structural chunks, chunk source-field JSON, publisher graph-edge candidates, and chunk embedding rows.
- Done: the storage projection records the provisional dense fingerprint (`bge-m3:1024:normalize:true`) on chunks and inserts pgvector embeddings through prepared statements, so canonical records can be reprojected and re-embedded without losing provenance.
- Done: `hybrid_candidates_json` now returns compact chunk/document IDs, citations, snippets, source URLs, validity blocks, RRF score details, and stable cursors while keeping the exact temporal prefilter and custom RRF over BM25 plus dense candidates.
- Done: `fetch_documents_json` returns full document text plus chunk bodies/provenance for selected document IDs.
- Done: ignored real-data storage smoke uses `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz` by default; local run on 2026-06-21 inserted 12 official LEGI articles, 12 chunks, and 53 publisher edges, then verified BM25/dense hybrid search, `--as-of` prefiltering before `valid_from`, and fetch full text.
- Done: direct CLI `search`/`fetch` and JSONL session `search`/`fetch` now route through the storage helpers using `--index-dir` or `JURISEARCH_INDEX_DIR`; missing or uninitialized indexes return JSON `index_unavailable` errors instead of creating empty indexes.
- Done: CLI `search` sanitizes user query text before handing it to ParadeDB's query parser, embeds the original query through the configured OpenAI-compatible Phase 0 endpoint, and uses the stored `bge-m3:1024:normalize:true` fingerprint convention for dense candidates.
- Done: CLI `fetch` is covered by an integration-style test that creates a durable index, drops it, then invokes the binary and JSONL session path against the existing root; missing IDs return `no_results`, and reserved `--as-of`/`--part` flags fail as `bad_input` until they are actually implemented.
- Done: ignored live CLI search smoke creates a tiny durable index with a live bge-m3 embedding, invokes `jurisearch search` against `JURISEARCH_INDEX_DIR`, and verifies the expected document plus dense rank.
- Done: `help schema --json` now advertises the implemented `SearchResponse.candidates` and `FetchResponse.documents` shapes instead of the old stub `results` shape.
- Done: storage dense rebuild finalization verifies full chunk embedding coverage for the expected fingerprint/model/dimension, rebuilds the ivfflat ANN index, and writes the embedding manifest with coverage and index parameters.
- Done: `jurisearch ingest embed-chunks` opens an existing durable index, loads canonical chunk embedding inputs, calls the configured OpenAI-compatible endpoint, inserts `chunk_embeddings`, and feeds the dense rebuild finalizer; `--limit` supports smoke runs and `--index-lists` controls the rebuilt ivfflat index.
- Done: live chunk embeddings use canonical `contextualized_body` recovered from `documents.canonical_json` when present, falling back to `chunks.body` until chunk provenance gets first-class storage columns.
- Done: ignored live CLI embed smoke creates a tiny durable index without dense rows, invokes `jurisearch ingest embed-chunks`, and verifies one endpoint-produced embedding plus the dense manifest/index coverage.
- Done: the embedding client preflights input size before network calls using configurable endpoint ceilings (`JURISEARCH_EMBED_MAX_INPUT_CHARS`, `JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS`, `JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN`); `status` and `ingest embed-chunks` output record the active budget.
- Done in Phase 1.2: tokenizer-backed embedding preflight can count a configured endpoint/model tokenizer before network calls, while conservative char/token estimates remain the fallback.

Acceptance:

- Met in storage layer: `search` returns compact IDs, citations, snippets, source URLs, validity blocks, scores, and cursors.
- Met in storage and CLI layer: `fetch` returns full text for selected IDs.
- Met in storage layer: historical `--as-of` queries do not leak future LEGI versions in the real-archive smoke.
- Met for Phase 0 slice: dense rows are re-insertable from canonical chunk IDs and fingerprints without losing provenance, CLI live search is smoke-tested, endpoint-driven chunk embedding is smoke-tested, and storage can finalize/rebuild the ANN index and manifest after a full re-embed.

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

Current status (2026-06-21):

- Done: feasibility evidence is recorded in `work/03-implementation/02-evidence/2026-06-21-reranker-feasibility.md`.
- Done: `bge-reranker-v2-m3` remains the first benchmark candidate, with `max_length=1024` as the initial setting because upstream notes 8192-token support but recommends 1024 from fine-tuning.
- Done: Phase 1 should implement a provider seam first (`disabled | http | local_onnx`), keep `disabled` as default, and use HTTP/TEI as the first shippable provider if legal eval proves a rerank gain.
- Done: local ONNX through Rust `ort` is feasible enough for a benchmark spike, but not selected as default before latency, tokenizer, runtime packaging, and model-cache policy tests.
- Done: local GPU acceleration is not a Phase 1 dependency; this workstation exposes AMD Radeon 8060S through ROCm, while the practical first measurements should be CPU and HTTP.
- Remaining for Phase 1 benchmark: run empirical `http` and `local_onnx` latency tests, verify tokenizer/pair-contract compatibility, pin runtime packaging/model-cache policy, and measure hybrid+rerank quality before any default adoption.

### 0.8 Official API Client Foundation

Tasks:

- Implement PISTE Axway **API-Key (`KeyId` header)** auth for **Judilibre** — verified (`/search` → 200).
- Implement **OAuth2 client-credentials (Bearer)** auth for **Légifrance** — verified end-to-end (`scope=openid` → Bearer → `/search` → 200, sandbox + prod). Token lifecycle/refresh applies to this path.
- Support sandbox and production endpoint + credential configuration.
- Implement rate-limit handling, backoff, and retry policy.
- Map upstream/API failures to stable error objects and process exit code `5`.
- Add Judilibre `/transactionalhistory` client support for Phase 2 deltas.
- Integrate secret loading with W7 (env/keyring) without logging credentials.

Acceptance:

- A representative **production** Judilibre call works (verified: `/search` → 200, `total` 109k+). Sandbox works once the sandbox app subscribes to Judilibre (currently "No subscribed APIs" → 403); until then dev/test uses prod or fixtures.
- Rate-limit and upstream-error paths are testable without leaking secrets.
- `cite --online`, Judilibre ingestion, and `sync --since` have a shared client rather than bespoke API code.

Current status (2026-06-21):

- Done: `jurisearch-official-api` provides the shared PISTE client foundation with production/sandbox base URLs, redacted config debug output, Judilibre `KeyId` auth, Légifrance OAuth2 client-credentials token acquisition/cache, Bearer search calls, and `/transactionalhistory` path support.
- Done: mock-server tests verify Judilibre sends `KeyId`, Légifrance posts the `scope=openid` client-credentials form and reuses Bearer tokens, missing credentials map to dependency errors, and HTTP 429 maps to stable upstream/rate-limit errors without leaking secrets.
- Remaining for later 0.8 slices: live opt-in smoke tests against configured PISTE credentials, explicit retry/backoff scheduling around 429/5xx, keyring-backed secret loading, CLI wiring for `cite --online` / `sync`, and Judilibre sandbox validation once the sandbox app is subscribed.

Phase 0 exit gate:

- Backend stack passes acceptance or fallback path is formally triggered.
- Eval harness exists and gates CLI contract, temporal correctness, citation states, and ranking metrics.
- Official XML ingestion path produces valid searchable records.
- Archive planning and streaming member reads are deterministic and manifest-recorded.
- Embeddings fingerprint guard works.
- Official API client foundation handles sandbox auth, rate limits, and upstream error mapping.
- Reranker feasibility data is available for the Phase 1 adoption gate.
- Platform policy and backend migration mechanics are recorded.
- No locked product decision remains ambiguous.

---

## 4. Phase 1 — Production-Quality LEGI Search

Goal: ship best-in-class statutory search over official LEGI XML.

### 1.0 Ingest Operational Accounting and Replay

Tasks:

- Add `ingest_run`, `ingest_member`, and `ingest_error` schema and repository APIs.
- Track per-member archive name, member path, source entity, date anchor, status, structured error, and recovery-compatibility metadata (`parser_version`, `schema_version`, `source_payload_hash`, `code_version`).
- Implement resume that skips only compatible `inserted` / `skipped` members and retries `failed` / unfinished `parsed` members.
- Block blind recovery when parser/schema/code/source-payload compatibility differs; require targeted reprocess.
- Add optional quarantine output for failed payloads with run/archive/member/error traceability.
- Add ingest-health metrics used by W2 reporting: failed-member percentage, error classes, projection coverage, embedding coverage, and replay snapshot status.
- Add safe-mode ingest flags that disable optimized write/backfill paths.

Acceptance:

- Interrupted ingest can resume without duplicate canonical records or skipped failed work.
- Parser/schema/code changes cannot silently preserve stale bad rows.
- Failed payloads can be traced to source archive/member and optionally quarantined.
- `status --json` can report latest ingest health, coverage, and recovery warnings from these tables.
- Query access is blocked or explicitly marked incomplete until required projections pass their gates.

Current status (2026-06-21):

- Done: storage schema migration `3` adds `ingest_run`, `ingest_member`, and `ingest_error` tables with run status, member archive/path/source/date/status fields, structured error fields, and recovery-compatibility metadata (`parser_version`, `schema_version`, `code_version`, `source_payload_hash`).
- Done: `jurisearch-storage::ingest_accounting` provides repository APIs for run start/finish, idempotent member recording, member status updates, structured error recording, compatibility-based resume decisions (`process` / `skip` / `retry` / `blocked_incompatible`), and ingest-health metrics covering latest-run member/error counts plus corpus-wide projection and embedding coverage.
- Done: storage integration tests cover compatible skip, failed/unfinished retry, incompatible replay blocking, structured error aggregation, and projection/embedding coverage metrics against managed Postgres.
- Done: `status --json` keeps the no-index pending payload but, when an initialized index is configured, reports live ingest health from storage, latest completed run, recovery warnings, and query readiness derived from projection/embedding coverage gates.
- Done: `jurisearch ingest legi-archives` streams the planned LEGI baseline/deltas into canonical storage for the current ARTICLE parser slice, records ingest run/member/error accounting, applies compatible resume decisions, counts parsed metadata roots and unsupported roots separately, records safe-mode as run metadata, and optionally quarantines failed XML payloads by run/archive/member path. Member-level failures are reported as `run_status: "failed"` with exit `0` when the command itself completed; systemic archive/storage errors still use JSON errors and non-zero exits. Metadata roots that are parsed but not yet projected into query documents are persisted as skipped member rows with their source ID/date anchor where available and reported in `parsed_metadata_roots`; truly unsupported roots remain in the JSON `unsupported_roots` summary. CLI contract tests cover a fresh-index ingest, metadata-root accounting, failed-member quarantine, accounting rows, compatible replay skip, failed-member retry, and compatibility-blocked replay.
- Done: retrieval commands now enforce ingest-health coverage outside `status`: `search` requires complete corpus projection and embedding coverage before contacting the embedding endpoint, while `fetch` requires complete corpus projection coverage.
- Done: ingest health now reports replay snapshot status plus deterministic component counts/signatures for documents, chunks, publisher graph edges, chunk embeddings, and index manifests; these signatures provide the basis for later replay-drift diffs.
- Done: embedding coverage is fingerprint-aware: a chunk counts as embedded only when its finalized chunk fingerprint is present and matches the corresponding `chunk_embeddings.embedding_fingerprint`, so stale embeddings cannot satisfy query readiness.
- Remaining for later 1.0 slices: add safe-mode behavior for future optimized write/backfill paths and expand full-corpus gate thresholds once broader LEGI canonicalization lands.

### 1.1 Full LEGI Canonicalization

Tasks:

- Expand parser coverage to full official LEGI code corpus.
- Normalize hierarchy into `hierarchy_path`.
- Build version groups across article versions.
- Preserve statuses: `VIGUEUR`, `MODIFIE`, `ABROGE`, `ABROGE_DIFF`.
- Implement the LEGI temporal identity contract: `legi:<LEGIARTI>@<valid_from>` for article-version IDs, stable `version_group`, and preserved `valid_to_raw`.
- Extract publisher-provided LEGI links and inline references into canonical `GraphEdge` records with `edge_source = publisher`.
- Define and version canonical text-assembly contracts per statutory document kind.
- Use the `juridocs` DTD matrix as a checklist only; re-verify required fields against the current official DTDs before making parser validation authoritative.
- Record per-record source payload hashes and source field lists.
- Record source dataset version, build date, coverage, schema version, parser version, and source files.
- Implement canonical-record retention policy: retain canonical records as a build artifact or document deterministic regeneration, with manifest traceability either way.

Acceptance:

- `status` reports LEGI coverage, freshness, source versions, and manifest.
- Rebuild from same inputs produces equivalent canonical records and index manifest.
- Canonical-record retention/regeneration policy is explicit and tested.
- Publisher graph edges rebuild from canonical records without re-ingesting LEGI XML.
- Canonical text payload hashes are stable across equivalent rebuilds.
- Temporal article IDs, `version_group`, and `valid_to_raw` are stable and covered by fixtures for current, modified, abrogated, sentinel, and same-day-version cases.

Current status (2026-06-21):

- Done: the LEGI parser now recognizes `TEXTE_VERSION`, `SECTION_TA`, and `TEXTELR` as DTD-backed metadata roots instead of unsupported roots. The parsed metadata records validate LEGI text/section IDs, normalize sentinel end dates, preserve source provenance/payload hashes, extract section hierarchy context, and derive TEXTELR date anchors from embedded link dates. Archive ingest records these roots separately from unsupported roots while leaving ARTICLE documents as the only query-projected canonical documents in this slice.
- Done: focused unit tests cover text-version, section, and text-structure metadata parsing; the ignored real-data smoke over `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz` parsed ARTICLE plus `TEXTE_VERSION` / `SECTION_TA` / `TEXTELR` roots in a bounded sample.
- Done: storage schema migration `4` adds `legi_metadata_roots` for parsed `TEXTE_VERSION`, `SECTION_TA`, and `TEXTELR` records. The table stores stable metadata keys, root/source/date lookup columns, parent text linkage for sections, payload hashes, archive/member provenance, canonical version, and lossless canonical JSON. `ingest legi-archives` persists these metadata rows before marking the member as skipped, reports `persisted_metadata_members`, and bumps the archive-member canonical compatibility version so existing skipped metadata rows are not silently reused without the new projection.
- Operator note: indexes populated before the `legi_article_metadata_parser:v3` / `canonical_record:v3` compatibility bump should be reprocessed intentionally; a blind resume will surface pre-bump members as compatibility mismatches before metadata backfill and `TEXTELR.structure_links` preservation occur.
- Done: archive ingest now runs a post-member hierarchy backfill that joins article publisher `LIEN_SECTION_TA` edges to persisted `SECTION_TA` metadata, enriches article canonical JSON/chunk contextualized text when the metadata path is strictly richer than the parsed article path, and invalidates stale chunk embeddings for changed documents.
- Done: hierarchy backfill now chooses the matching `SECTION_TA` version by the publisher edge `debut` date when present, then by article validity, and only falls back to the latest section row when source dates are incomplete or no section window contains the available anchor. A regression fixture stores two versions of the same section UID and verifies an 1804 article edge does not pick the 2020 section title, while an edge on the 2020 boundary uses the newer section.
- Done: archive ingest now scopes hierarchy backfill to article document IDs and `SECTION_TA` source UIDs touched by the current run, skips the backfill entirely when a resume touches neither, and reports the scoped document/section counts in command output and the stored manifest. The storage API keeps an explicit full-backfill entry point for maintenance and tests.
- Done: `jurisearch ingest backfill-legi-hierarchy` runs the explicit full LEGI hierarchy rebuild over an existing index, reports updated-document and invalidated-embedding counters, emits a re-embed hint when embeddings were invalidated, and leaves ingest-run/member accounting untouched. This is the operator recovery path for interrupted runs after member persistence but before end-of-run scoped backfill.
- Done: `ingest legi-archives` now writes a LEGI source manifest to `ingest_run.manifest` and command output, including run status/completeness, archive plan, latest archive timestamp/source version, parser/schema/code versions, member coverage counters, metadata root counters, hierarchy-backfill counters, and skipped archive summary. `status` exposes the latest run manifest through `ingest_health.latest_manifest`.
- Done: focused article parser fixtures now pin LEGI status and temporal normalization for `VIGUEUR`, `MODIFIE`, `ABROGE`, `ABROGE_DIFF`, `TRANSFERE`, non-sentinel end dates, the dominant LEGI open-ended sentinel (`2999-01-01`), and the related Légifrance-family open-ended sentinel (`2999-12-31`).
- Done: ignored real-data coverage now scans the local official LEGI baseline (`/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz`) until it observes status and temporal-end evidence. The verified run on 2026-06-21 visited 10,322 XML members, parsed 8,183 ARTICLE members, observed `VIGUEUR`, `MODIFIE`, `ABROGE`, `ANNULE`, `MODIFIE_MORT_NE`, `PERIME`, and `TRANSFERE`, found finite `valid_to` examples for the non-open-ended statuses, and confirmed the `2999-01-01` sentinel normalization. It records, rather than fails on, current parser gaps in the early `TNC_non_vigueur` archive branch and guards that this real-data parse-error rate stays below 1%. Multi-version and same-day-version corpus evidence remains future work because current canonical `version_group` is still the per-version `LEGIARTI` id rather than a shared article chronicle key.
- Done: ARTICLE canonicalization now tolerates official legacy records that omit `META_ARTICLE/NUM` or `META_ARTICLE/TYPE`, preserving absent type metadata as `type=absent` and using the `LEGIARTI` id as the deterministic title fallback when `NUM` is absent. The same real-data scan now leaves only one body-less `BLOC_TEXTUEL/CONTENU` parse gap in the observed window.
- Done: official ARTICLE records with no textual `BLOC_TEXTUEL/CONTENU` are intentionally not projected as canonical searchable documents. The parser still rejects them as missing required canonical body content, while `ingest legi-archives` classifies that exact ARTICLE parse error as a skipped no-text member, reports `skipped_no_text_articles`, avoids quarantine/error rows, and allows the run to complete when no other member fails.
- Done: `TEXTELR` canonical metadata now preserves ordered `structure_links` for `LIEN_TXT`, `LIEN_SECTION_TA`, and `LIEN_ART`, including target UID, raw `debut`/`fin`, optional `niv`, link text, and raw DILA attributes. Link target extraction prefers DILA identity attributes before broader references, and the parser/schema compatibility bump forces intentional replay before consumers depend on the new structure links. This ports the reusable `juridocs` text-structure insight into the persisted metadata layer without adding a new table yet.
- Done: hierarchy backfill now consumes persisted `TEXTELR.structure_links` as a fallback candidate source. For ARTICLE documents without direct publisher `LIEN_SECTION_TA` edges, it pairs each `LIEN_ART` with the nearest preceding `LIEN_SECTION_TA` in the same flat TEXTELR structure, joins that section to persisted `SECTION_TA` metadata, and applies the existing date-aware section-version selection/enrichment rules. Scoped ingest now tracks touched TEXTELR text IDs so TEXTELR-only replays can trigger the same backfill path; storage schema migration `5` adds `documents_source_uid_idx` to keep source-UID article joins indexable.
- Done: TEXTELR fallback hierarchy enrichment now carries the ordered preceding `LIEN_SECTION_TA` links for each fallback `LIEN_ART` and assembles a section stack from their `niv` levels and link text. It uses that stack only when it is richer than persisted `SECTION_TA.hierarchy_path`, excludes articles that already have direct publisher section edges, and keeps unresolved/no-preceding-section articles untouched. The direct-edge exclusion is intentional: publisher `LIEN_SECTION_TA` edges remain authoritative for those articles even when a TEXTELR stack could add deeper ancestry.
- Done: TEXTELR hierarchy backfill performance evidence is recorded in `work/03-implementation/02-evidence/2026-06-21-textelr-backfill-explain.md`. The run loaded official members from the expanded LEGI 20250713 dataset into temporary indexes, confirmed current Code civil's `TEXTELR` is too shallow to stress fallback article pairing, and captured production-shaped `EXPLAIN (ANALYZE, BUFFERS)` on fallback-heavy official `LEGITEXT000006075080` (220 `LIEN_ART`, 1 `LIEN_SECTION_TA`, 223 structure links). The measured full and text-source scoped candidate plans both returned 33 rows in about 90 ms and used `documents_source_uid_idx`; the repeated JSONB lateral scans remain visible but do not require immediate batching in this slice.
- Remaining for later 1.1 slices: re-check the TEXTELR candidate query on a true corpus-scale full backfill and add maintenance batching or structure-link materialization if execution time grows with large or numerous TEXTELR structures. That re-check must include many `TEXTELR` rows in one index, at least one text with deeper section stacks, and the direct publisher branch's `graph_edges` payload filter.

### 1.2 Statutory Chunking and Context

Tasks:

- Chunk article versions structurally.
- Split long articles only on legal substructure such as alinéas and enumerations.
- Use French sentence splitting and legal-abbreviation repair only as a long-article sub-splitting aid, not as the primary chunk boundary model.
- Preflight embedding inputs against a tokenizer or endpoint-specific token budget; keep conservative char-based guardrails as fallback only.
- Record chunk-origin provenance: `structural`, `zone`, `heuristic`, or `hard_split` as applicable.
- Record per-chunk `source_payload_hash`, `source_fields`, `chunk_builder_version`, and embedding fingerprint.
- Repeat article header and hierarchy context where needed for embedding.
- Implement `context` for ancestry and sibling articles with `--as-of`.

Acceptance:

- No chunk crosses legal hierarchy boundaries.
- Embedding inputs cannot exceed the configured model/endpoint budget without an actionable ingest error.
- Every chunk is traceable to its source payload, builder version, and chunk-origin provenance.
- `context` reconstructs section neighbourhood at the requested date.
- A structural-survival test proves `Code -> Livre -> Titre -> Chapitre -> Section -> Article` remains intact after ingestion, chunking, and context reconstruction.
- Eval includes long-article and hierarchy-sensitive cases.

Current status (2026-06-21):

- Done: storage schema migration `6` materializes emitted chunk provenance on `chunks` with `contextualized_body`, `chunking`, `boundary`, and `hierarchy_path`. Canonical LEGI document insertion writes these fields directly from `CanonicalChunk`, hierarchy backfill refreshes chunk context/provenance columns when it enriches document hierarchy, `ingest embed-chunks` reads stored `chunks.contextualized_body` instead of reparsing document JSON, and `fetch` returns the stored chunk provenance fields.
- Operator note: migration `6` rewrites existing `chunks` rows once to materialize provenance from `documents.canonical_json`; on a corpus-scale populated index this startup migration may take time and hold write locks, but it preserves existing embedding fingerprints because it copies the text the previous embedding path already used.
- Done: `context` is implemented for LEGI article ancestry and same-section sibling articles. The storage query reconstructs compact target, ancestry, and sibling summaries from stored `chunks.hierarchy_path`/canonical hierarchy, applies the requested `--as-of` validity window, defaults sibling reconstruction to the target version's `valid_from` when no date is supplied, caps returned sibling summaries with explicit `sibling_count`/`sibling_limit`/`sibling_truncated` fields, and is wired through both the CLI command and JSONL session protocol. Focused storage and CLI tests cover ancestry, sibling filtering, future-version exclusion, not-valid-at-date no-results, empty-hierarchy sibling suppression, compact sibling truncation, and invalid `--as-of` handling.
- Done: a structural-survival integration test proves `Code -> Livre -> Titre -> Chapitre -> Section -> Article` survives synthetic LEGI ARTICLE parsing, structural chunk construction, storage projection, and `context` reconstruction, including deep same-section sibling inclusion and neighbouring-section exclusion.
- Done: schema migration `7` materializes `documents.hierarchy_path` and adds `documents_context_hierarchy_idx` over `source`, `kind`, and `md5(hierarchy_path::text)`; canonical projection and hierarchy backfill keep the document-level path synchronized, and `context --siblings` now uses the indexed document path plus exact path equality instead of scanning chunk hierarchy per candidate document.
- Operator note: migration `7` rewrites existing `documents` rows once to materialize hierarchy paths and builds the context hierarchy index; on a corpus-scale populated index this startup migration may take time and hold write locks, but it removes the interactive `context --siblings` per-document chunk scan.
- Done at the fixture-definition level: hierarchy-sensitive eval coverage is now represented in `jurisearch-core`: `LegalRetrievalFixture` has an explicit `tier` plus optional machine-checkable `HierarchyExpectation` fields for context target, `as_of`, expected ancestry titles, required siblings, and forbidden siblings. The Phase 1 dev seed fixtures use official LEGI Freemium archive evidence for a same-section case and a temporal article-version/sibling case; they are source-checked but non-gating until a named legal reviewer promotes them. Follow-up for the W2 eval harness: execute these fixtures against `context` and assert ancestry / required / forbidden sibling expectations.
- Done: BM25 now indexes and searches `chunks.contextualized_body` instead of raw `chunks.body`, matching the text used for dense embeddings and allowing hierarchy/header terms to participate in lexical retrieval. Schema migration `8` backfills empty contextualized bodies from raw bodies, enforces a non-null/non-blank contextualized-body invariant, rebuilds `chunks_bm25_idx` on `contextualized_body`, and `hybrid_candidates_json` uses the context-enriched field for the lexical leg.
- Operator note: migration `8` drops and rebuilds the `pg_search` BM25 index; on a corpus-scale populated index this can take time and temporarily removes the lexical index while the migration runs.
- Follow-up for W5/W2 ranking evidence: run a before/after BM25 ranking check on the target-spike corpus or executable hierarchy eval fixtures before treating the longer header-prefixed lexical field as quality-neutral at scale.
- Done: long ARTICLE bodies now split conservatively on structural alinéa boundaries when the context-enriched article exceeds the Phase 1 chunk-size guardrail. Normal articles still emit one `article` chunk; split chunks keep `chunking = structural`, use `boundary = alinea|alinea_range`, carry source-field alinéa ranges, repeat hierarchy/article context in each `contextualized_body`, and bump the chunk-builder version to `legi_article_structural:v2`. The LEGI parser compatibility version is bumped to `legi_article_metadata_parser:v4` and ARTICLE canonical records to `legi_article:v2` so archive resume/replay does not silently retain old v1 single-chunk projections. Single oversized alinéas are not hard-split; tokenizer/preflight remains responsible for rejecting those until tokenizer-grade splitting lands.
- Done: embedding preflight can now use a configured Hugging Face `tokenizer.json` (`JURISEARCH_EMBED_TOKENIZER_JSON`) to count endpoint/model tokens before any embedding request is sent. The existing conservative character and estimated-token budgets remain the default fallback and continue to be reported, while status and `ingest embed-chunks` also expose the active token-count method and tokenizer path. Oversized tokenizer-counted inputs fail as bad input with the offending chunk ID in batch embedding.
- Remaining for later 1.2 slices: optional tokenizer-aware re-splitting of single oversized alinéas instead of fail-fast preflight only.

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

Current status (2026-06-21):

- Done: the chunk BM25 index now uses an explicit pg_search French legal tokenizer config for `chunks.contextualized_body`: default tokenization with accent folding, French stemming, and French stopword removal. Elision is handled by the tokenizer splitting apostrophes plus French stopword removal of elision particles; there is no dedicated elision filter. Schema migration `9` drops/rebuilds `chunks_bm25_idx` with `text_fields` tokenizer metadata and records manifest schema version `9`.
- Operator note: migration `9` drops and rebuilds the `pg_search` BM25 index; on a corpus-scale populated index this can take time and temporarily removes the lexical index while the migration runs.
- Done: retrieval smoke covers accent-insensitive and stemmed French legal lexical matching (`responsabilité`/`réparations`/`créancier`/`procédure`/`arrêté` vs unaccented queries), elision matching (`l'auteur` vs `auteur`), and exact statutory reference matching with reciprocal `Article 1240` and `Article 1241` assertions.
- Already present from earlier retrieval slices: temporal validity predicates are applied independently to lexical and dense candidate pools before RRF fusion.
- Done: retrieval-mode ablation controls are now first-class. `jurisearch search --mode hybrid|bm25|dense` and session JSON `mode` default to `hybrid`; `help schema --json` advertises the same enum; search responses include `retrieval_mode`. BM25-only search requires only projection coverage and skips query embedding/embedding coverage, while dense and hybrid keep the embedding gate and endpoint call.
- Done: `hybrid_candidates_json` supports `RetrievalMode::Hybrid`, `RetrievalMode::Bm25`, and `RetrievalMode::Dense` without passing fake vectors through BM25-only queries. Storage smoke asserts hybrid rank fusion plus BM25-only `dense_rank: null` and dense-only `lexical_rank: null`.
- Done: `jurisearch expand` is implemented for one-shot CLI and JSONL session mode over a deterministic `legal-vocabulary-seed:v1` lexicon. Each emitted `expanded_terms` entry carries `source_seed_id`, source citation, review status, reviewer placeholder, and rationale; `help schema --json` now exposes `ExpandRequest`/`ExpandResponse`, and CLI contract tests cover one-shot and session expansion without requiring an index.
- Done: `search` responses now log the same curated expansions as `expanded_terms` plus `expansion_seed_version`, without feeding those terms into ranking yet. This gives W2/W5 an auditable expansion trail for ranking experiments while preserving current BM25/dense behavior.
- Done: `search` responses include explicit pagination/truncation guidance metadata: requested `top_k`, returned candidate count, `possibly_truncated`, cursor support status, cursor note, and guidance.
- Done: `jurisearch search --format concise|detailed` and session JSON `format` now default to `concise`. Concise preserves the current candidate payload plus standard metadata; detailed adds query/retrieval diagnostics (`query_input`, lexical query text, mode, dense/lexical use, pool limits, embedding fingerprint, and kind filter) without changing ranking.
- Done: cursor pagination is implemented for `search`: `--cursor` / session JSON `cursor` accepts the stable candidate cursor format, storage filters the ranked candidate stream after the cursor, CLI over-fetches one row to emit `next_cursor`, and pagination metadata now reports `cursor_supported: true` with `after_cursor`, `next_cursor`, returned count, and guidance. Cursor pagination walks the ranked relevance pool (`top_k`-derived lexical/dense limits), not an exhaustive corpus scan.
- Follow-up for W5/W2 ranking evidence: include the French analyzer change in the planned before/after BM25 ranking check, and run a legal-vocabulary pass over the French stopword behavior before treating the analyzer as quality-neutral at scale.
- Remaining: feed `expanded_terms` into explicit ranking experiments, hybrid+authority ablation reporting once the authority prior exists, legal term/field boosters, and authority prior.

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

Current status (2026-06-21):

- Done: local-first `jurisearch cite` is implemented for one-shot CLI and JSONL session mode. It resolves internal `legi:<source_uid>@<date>` document IDs, `LEGIARTI` article IDs, `LEGITEXT`/`LEGISCTA` metadata roots, canonical-shape NOR identifiers from TEXTELR metadata, and free-text `article <num>` citations against the local index, including `L.`/`R.`/`D.` prefixed article numbers and a small set of common code-name hints.
- Done: cite responses return explicit citation states (`exact`, `normalized`, `ambiguous`, `stale_version`, `not_found`, `source_unavailable`), effective/requested `as_of`, match counts, local candidates with validity metadata, and `valid_on_as_of` annotations. `--strict` exits with code `2` unless the state is `exact` or `normalized`.
- Done: CLI contract coverage exercises exact LEGIARTI lookup, free-text normalization over both hand-written and LEGI-ingested rows, ambiguous article numbers, prefixed article numbers, historical `--as-of`, stale versions, malformed citations, strict failure, NOR metadata lookup, and session JSONL cite.
- Done: `cite --online` now uses the shared Légifrance API client path: OAuth2 client-credentials token acquisition, Bearer `/dila/legifrance/lf-engine-app/search` probe, `online.checked=true` on success, and upstream API failures mapped through the shared error object to exit code `5`. The current contract is hard-fail: when `--online` is requested, upstream availability gates even locally resolvable citations. Malformed citations are classified locally and are not sent upstream.
- Remaining: enrich `--online` from reachability/probe semantics to source-of-truth confirmation once the exact Légifrance search request envelope and response fields for citation IDs/versions are pinned by fixtures or live opt-in smoke tests. The current probe body is a placeholder (`query` plus `pageSize`) used to exercise shared auth/error plumbing, not the final production search schema.

### 1.5 JSONL Session and Batch

Tasks:

- Implement request/response envelope with echoed `id`.
- Preserve input order.
- Keep the active JSONL stream stdout-only with structured per-request errors in-stream; reserve stderr for non-stream diagnostics.
- Support `help` and `help schema` inside session.
- Implement non-fatal malformed-line errors.
- Implement explicit `exit` acknowledgement.
- Implement finite `batch --jsonl`.

Acceptance:

- Sequential multi-call agent loop runs without process restart.
- Malformed input does not kill the session unless fatal mode is explicit.
- Session and one-shot payloads share schemas.

Current status (2026-06-21):

- Done: `session --jsonl` and `batch --jsonl` share the request/response envelope with echoed `id`, preserve input order, keep the active stdout stream JSONL-only, emit per-request errors as structured JSONL objects, support `help`/`help schema`, and acknowledge `exit`. Stderr remains empty in contract tests for normal and error JSONL responses.
- Done: malformed JSONL lines are non-fatal by default and produce per-line `bad_input` responses; `--fatal` stops only after malformed JSON, not after well-formed command-level error responses. `batch --jsonl` is finite over stdin EOF, and both `session` and `batch` reject missing `--jsonl` with exit code `2`.
- Done: CLI contract tests cover warm session ordering/help/exit, curated expansion in session mode, finite batch EOF behavior, non-fatal malformed lines, fatal malformed-line stopping, and required `--jsonl`.

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

Current status (2026-06-21):

- Done: CLI embedding configuration now loads from a TOML runtime config at `JURISEARCH_CONFIG`, `$XDG_CONFIG_HOME/jurisearch/config.toml`, or `~/.config/jurisearch/config.toml`; `JURISEARCH_CONFIG=none`, `0`, or an empty value disables file loading for deterministic tests and scripted runs.
- Done: file config covers embedding provider, base URL, API key, model, dimension, normalization, pooling, token budgets, tokenizer path, provisional status, and re-embeddable status. `JURISEARCH_EMBED_*` environment variables are applied after the file config, so env values remain the final override layer. Provider spellings accept the same aliases in env and file config, `base_url` implies the OpenAI-compatible provider, and `0` token budgets mean unbounded in both layers.
- Done: `jurisearch status` reports config diagnostics (`config_path`, `config_loaded`, `config_error`) and active non-secret embedding settings, while API keys from either config files or env are redacted from stdout/stderr contract coverage. TOML parse diagnostics are source-free line/column errors, and unknown embedding config keys are rejected without echoing source snippets.
- Done: in-process model-cache checks use `JURISEARCH_MODEL_DIR`, then `$XDG_CACHE_HOME/jurisearch/models`, then `~/.cache/jurisearch/models`, and require an explicit cache bundle under `embeddings/<model-key>/` with `model.onnx` and `tokenizer.json`. `status` reports missing/ready cache state, `setup` reports readiness, `model fetch` confirms pre-staged cache or refuses missing models without `--allow-download`, and JSONL session supports both `setup` and `model fetch`.
- Done: `status` reports embedding endpoint reachability for local loopback OpenAI-compatible endpoints via a bounded TCP probe; hosted endpoints are classified but not probed to avoid unsolicited external network calls.
- Done: dense query and `ingest embed-chunks` paths call the existing in-process readiness guard before constructing the embedding client, so configured in-process mode with a missing local model fails with an actionable `model fetch` hint instead of attempting an implicit download.
- Exit-code note: `model fetch` without `--allow-download` treats a missing cache as a user/actionability error (`bad_input`, exit `2`); query/embed paths treat the same missing cache as an unmet local dependency (`dependency_unavailable`, exit `4`) because retrieval cannot proceed.
- Remaining: package a real in-process embedding backend/download implementation behind `model fetch --allow-download`; until then, `--allow-download` returns a dependency error instructing operators to pre-stage the required bundle. The real download backend must validate cache contents with size/hash checks before reporting a model as ready.

### 1.7 Phase 1 Evaluation and Migration Gate

Tasks:

- Complete LEGI eval set with realistic statutory research tasks.
- Run ingest-health gates: latest completed LEGI run, failed-member/error thresholds, projection/embedding coverage, and replay snapshot diffs over canonical records, chunks, publisher graph edges, embeddings, and manifest fields.
- Embedding model is **locked to `bge-m3`** (DECISIONS D21) — the comparative French-specialist bake-off is skipped (bge-m3 validated as statistically indistinguishable from CamemBERT and Solon on French-legal retrieval, 2026-06-22 evidence note). Still measure absolute retrieval quality on bge-m3 via the ablations / temporal / citation evals below.
- No Phase 1 re-embed/migration is planned; the migration path (manifest fingerprint change, chunk target-fingerprint bump, full re-embed, vector-index rebuild) remains available if a future model change is ever decided.
- Evaluate reranker adoption before release.
- Use the Phase 0 reranker feasibility spike to decide local vs HTTP vs disabled provider before the Phase 1 claim.
- Measure token/tool-call budget for `search → fetch → cite`.

Acceptance:

- Phase 1 may claim best-in-class LEGI/statutory search only if eval gates pass.
- Phase 1 may be queried as complete only if ingest-health and projection gates pass.
- The index fingerprint matches the locked `bge-m3` model before release.
- Any re-embedding migration is reproducible from canonical records and recorded in the manifest.
- If reranker is not adopted, the eval result is recorded.
- Docs and `status` do not claim full French juridic coverage before Phase 2.

Current status (2026-06-22):

- Done: `jurisearch status` now includes a fail-closed `phase1_gate` object for the LEGI/statutory Phase 1 claim. It reports `claim_allowed=false` until index query readiness, latest completed ingest run, zero failed members, projection/embedding coverage, replay snapshot availability, release-gating eval fixtures, final embedding-model selection, and reranker adoption/deferral are all satisfied.
- Done: the built-in Phase 1 eval fixture summary is machine-readable in `phase1_gate.eval_fixtures`; the current built-in hierarchy fixtures are source-verified development fixtures, not release-gating fixtures, so the gate correctly remains pending.
- Done: the first Phase 1 LEGI release-candidate fixture set is source-checked against the real DILA Freemium LEGI archive at `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz`. The candidates cover known-article lookup, conceptual statutory retrieval, temporal retrieval, and citation-rich statutory retrieval, carry structured `as_of` dates for future fixture execution, and `phase1_gate.eval_fixtures.release_candidates` reports them separately from `release_gating`.
- Done: `jurisearch eval phase1` can list the built-in release candidates without an index and can execute selected fixtures through the existing search path, reporting per-fixture expected-ID ranks, pass/fail status, retrieval mode, and candidate diagnostics. Session JSONL supports `eval phase1` with the same payload contract. Current execution checks expected-ID rank; hierarchy assertions from dev fixtures remain a follow-up harness extension.
- Done: the full LEGI dense projection completed against OpenRouter `baai/bge-m3` on 2026-06-22 with full coverage (`1,852,745` chunks and embeddings), rebuilt `chunk_embeddings_embedding_ivfflat_idx`, reported zero endpoint failures, and saved completion evidence in `work/03-implementation/02-evidence/2026-06-22-openrouter-dense-projection-run.log`.
- Done: the completed index was backed up to `/mnt/models/jurisearch-backup/phase1-freemium-20250713-20260622T135908+0200` with a `phase1-freemium-20250713-latest` symlink.
- Done: the D21 final embedding-model gate now passes from the stored dense embedding manifest when it matches the locked v1 fingerprint (`bge-m3:1024:normalize:true`, model `bge-m3`, 1024 dimensions, normalized embeddings) even while the manifest remains `provisional=true` / `reembeddable=true` for future migration capability.
- Done: executable Phase 1 eval fixtures were run against the completed LEGI index at top 20. Release-gating fixture evidence: BM25 passed 4/4, dense passed 2/4, and hybrid passed 4/4. Hybrid plus dev fixtures passed 5/6; the failing dev-only case is `legi-hierarchy-temporal-sibling-2000`. Evidence is recorded in `work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md`.
- Remaining: promote release-gating fixtures only after named human legal-domain review.
- Done: `jurisearch status` is cheap by default for replay evidence. Default status now reads cached replay signatures from `index_manifest['replay_snapshot']`, while `status --deep` explicitly recomputes and refreshes full replay signatures on demand. Successful LEGI archive ingestion, hierarchy backfill, and dense embedding finalization refresh the cache at command boundaries. Full-index evidence on `/home/pierre/Work/jurisearch/index/phase1-freemium-20250713` showed uncached default status in 3.00s with `replay_snapshot_source=missing`, explicit deep refresh in 589.34s over 1,736,165 documents, 1,852,745 chunks, 12,949,444 publisher edges, and 1,852,745 embeddings, then cached default status in 2.87s with the same signature (`430af44453662d6107a46e7baedde246`). Evidence is recorded in `work/03-implementation/02-evidence/2026-06-22-status-cache-optimization-summary.md`.
- Done: the Phase 1 reranker decision is explicitly deferred/disabled by default and wired into `status.phase1_gate.reranker_decision`. The decision is backed by the feasibility spike plus real-index eval evidence: the current candidate fixture set cannot measure a material rerank gain, no reranker provider is packaged, and cross-encoder latency/packaging remain unmeasured. Evidence is recorded in `work/03-implementation/02-evidence/2026-06-22-reranker-deferral-decision.md`.
- Done: the current Phase 1 release-candidate fixture set was assessed and is not strong enough to prove hybrid over BM25 because BM25 and hybrid both pass 4/4 at top 20. It remains useful source-checked smoke coverage but needs named human review plus more discriminating legal-ranking cases before promotion. Evidence is recorded in `work/03-implementation/02-evidence/2026-06-22-phase1-fixture-strength-decision.md`.

---

## 5. Phase 2 — Jurisprudence and Full French Juridic Search

Goal: add Judilibre and justice administrative so the product can claim best-in-class French juridic search across statutes and jurisprudence.

Phase 2 scope note: DILA bulk jurisprudence XML (`cass`, `inca`, `capp`, `jade`; roots `TEXTE_JURI_JUDI` / `TEXTE_JURI_ADMIN`) is an explicit optional adapter, not a default equal source path. Judilibre and justice-administrative ingestion remain the required Phase 2 path. If DILA bulk is accepted, it is a flagged coverage fallback after 2.1 and 2.2 are stable; decisions without official publisher zones use heuristic/fallback chunking provenance and do not satisfy the official-zone chunking gate by themselves.

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

### 2.2a Optional DILA Bulk Jurisprudence Adapter

Tasks, only if explicitly accepted:

- Parse DILA bulk jurisprudence archives for `cass`, `inca`, `capp`, and `jade`.
- Support `TEXTE_JURI_JUDI` and `TEXTE_JURI_ADMIN` roots.
- Preserve `JURITEXT`, `CETATEXT`, ECLI, court/source metadata, decision date, title, solution, text, summaries, links, archive/member provenance, and correction/replay semantics.
- Map records into the same canonical decision schema used by Judilibre and justice-administrative ingestion.
- Mark chunking provenance as heuristic/fallback when official zones are absent.

Acceptance, only if accepted:

- DILA bulk records are clearly distinguishable in `status`, manifest, and provenance.
- DILA bulk records cannot be mistaken for zone-accurate Judilibre records.
- Replay of the same bulk archives is deterministic and count/signature-stable.

### 2.3 Graph Layer

Tasks:

- Store canonical graph edges in Postgres.
- Materialize publisher-provided LEGI edges emitted during Phase 1 without re-ingesting LEGI XML.
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
- Reuse archive precedence and replay-order rules for source families that sync from bulk/delta archives.
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
- Archive filename parsing and baseline/delta ordering.
- Temporal interval semantics.
- Citation parsing and state classification.
- Schema validation.
- Parser error classification and DTD-required field validation.
- RRF and authority scoring.
- Vocabulary expansion.
- Official API error mapping.
- Embedding fingerprint migration decisions.
- Chunk builder provenance and token-budget preflight.

### Integration Tests

- Embedded Postgres lifecycle.
- Extension installation and migrations.
- Embedding-model re-embed and index migration.
- Ingestion → canonical records → index → search.
- Streaming archive reads with member byte caps.
- Ingest run/member/error accounting, resume-after-interruption, quarantine, and recovery-compatibility gates.
- Publisher link extraction into canonical graph edges.
- Derived projection gating before query access.
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
- Replay snapshots over canonical records, chunks, graph edges, embeddings, and manifest fields.
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
- Archive precedence and streaming member reads are validated.
- CLI contract skeleton works.
- Embedding endpoint contract works.
- Embedded Postgres spike hits < 500 ms warm JSON on the target spike corpus or records a hard failure.
- Official API client foundation works against sandbox and maps upstream failures.
- Reranker feasibility spike feeds Phase 1 adoption decision.
- Platform policy and migration mechanics are documented.

### Phase 1 Gate

- Full LEGI statutory corpus path works from official XML.
- Ingest run/member/error accounting, resume, and replay gates pass.
- Projection/embedding coverage gates pass before the index is marked query-ready.
- Temporal correctness eval passes.
- Citation verification eval passes.
- Hybrid retrieval beats BM25-only and dense-only baselines on legal tasks.
- Embedding model is locked to `bge-m3` (D21); no Phase 1 re-embedding/migration required.
- Reranker adoption/deferral is backed by the feasibility spike and eval result.
- CLI contract eval passes.
- `status` accurately reports coverage and freshness.
- `status` reports ingest health, latest completed source run, projection completeness, and recovery warnings.
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
| Embedding-model choice (resolved) | Low | `bge-m3` locked as v1 (D21) after CamemBERT/Solon validation; re-embed/migration capability retained for any future change. |
| Official XML edge cases are more complex than fixtures | High | Phase 0 parser spike on real representative XML; canonical validation. |
| Full LEGI baseline volume/throughput exceeds the subset parser path | High | Streaming member reads, configurable member caps, and deterministic archive ordering from Phase 0. |
| Resume/recovery after parser/schema changes preserves stale bad rows | High | Record parser/schema/code/source-payload compatibility metadata and require targeted reprocess on mismatch. |
| Official API coverage-date drift or rate limits block verification/sync | High | Bulk dumps for full builds; APIs only for deltas/verification; backoff, sandbox/prod config, and freshness reporting. |
| Eval set too small or generic | High | Build production-grade legal evals from Phase 0; legal-domain review, minimum category coverage, held-out split. |
| Reranker local packaging lags | Medium | Keep HTTP rerank provider; quality gate decides adoption. |
| Embedding/projection failure masks successful canonical ingestion | Medium | Separate canonical-ingestion states from embedding/index-projection states; retry/backfill derived work. |
| Phase 1 ingestion omits publisher links and forces Phase 2 LEGI re-ingestion | Medium | Capture publisher links as canonical `GraphEdge` records during 1.1. |
| Char-based chunk sizing overflows the embedding endpoint | Medium | Tokenizer or endpoint-specific preflight before embedding; conservative char guardrails only as fallback. |
| DILA bulk jurisprudence is mistaken for zone-accurate Judilibre data | Medium | Keep DILA bulk as optional flagged fallback; Judilibre and justice-administrative sources remain primary for official zones. |
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
4. Add archive precedence/streaming module with deterministic ordering tests.
5. Record phase/workstream owners and dependency gates in issue tracking from the matrix above.
6. Spike embedded Postgres + `pgvector` + `pg_search` against the concrete packaging and latency checklist.
7. Spike official LEGI XML → canonical record → Rust validation.
8. Add ingest run/member/error accounting, resume, and quarantine before full-corpus ingestion.
9. Implement endpoint embeddings fingerprint checks with locked `bge-m3` (D21), token preflight, and migration metadata.
10. Implement the shared official API client foundation.
11. Run the reranker feasibility spike.
12. Build the minimal LEGI subset search path: BM25 + dense + RRF + `search`/`fetch`.

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
- `work/notes/2026-06-21-juridocs-ingestion-reuse.md`
- `work/notes/2026-06-21-ingestion-reuse-impact-on-implementation-plan.md`
