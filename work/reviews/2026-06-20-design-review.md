# Design review: `jurisearch`

Review date: 2026-06-20

Reviewed files:

- `work/01-design/README.md`
- `work/01-design/DESIGN.md`
- `work/01-design/DECISIONS.md`
- `work/01-design/RESEARCH.md`

User constraints incorporated in this review:

- Rename the agent-facing binary from `juri` to `jurisearch`.
- Search/runtime should be Rust.
- Python is acceptable only where it materially improves offline ingestion.
- CLI only. No MCP, no HTTP transport.
- The CLI must provide complete inline help for agents.
- Prefer embedded local storage such as embedded Postgres/`pg-embed` with relevant extensions if it makes sense.
- AGPL-3.0 is acceptable for this project, so ParadeDB/`pg_search` can be considered as a default lexical-search candidate.
- Do not introduce Qdrant or another separate vector-search service unless a spike proves local embedded options fail.

## Executive verdict

The legal-retrieval concept is strong, but the proposed implementation direction needs a major revision before build starts.

The design correctly preserves the foundation's core legal ideas: structure-aware chunking, temporal correctness, hybrid retrieval, authority signals, citation verification, graph-backed related material, and multi-step agent search. Those should stay.

The design is misaligned with the product constraints in four important places:

1. It keeps `juri` as the working binary name instead of committing to the more agent-obvious `jurisearch`.
2. It recommends Python as the v1 core runtime, while Rust is a better fit for the search binary and CLI-only agent use.
3. It keeps MCP/HTTP/`serve` in the v1 surface, despite the CLI-only requirement.
4. It does not specify the complete inline help contract that replaces MCP tool descriptions.

## Findings

### P0: The transport model conflicts with the product direction

`DESIGN.md` currently scopes the tool as callable "as a subprocess and/or via MCP" and later defines CLI, `serve`, MCP, and HTTP adapters. `README.md` also says the tool exposes its contract over CLI and MCP.

This should be changed to CLI-only.

Recommended replacement:

- Primary interface: `jurisearch <command> ...`
- Machine interface: `--json` on every command.
- Long-running agent interface: `jurisearch session --jsonl` or `jurisearch batch --jsonl`.
- Removed from v1: MCP server, HTTP server, `jurisearch serve`.

Reasoning: removing MCP increases the importance of subprocess ergonomics. A JSONL session mode still satisfies "CLI only" while avoiding repeated model/index cold starts in multi-step agent loops.

### P0: The runtime recommendation should be Rust search first

The current design recommends "Python core + LanceDB" for v1 and defers Rust to v2. That is the wrong default for the search/runtime layer under these constraints.

Rust is viable for the search binary:

- Tantivy is a Rust full-text search library inspired by Lucene, with BM25, range queries, facets, configurable tokenizers, incremental indexing, mmap support, and tiny startup time.
- LanceDB has a Rust SDK and supports local vector search, metadata, SQL, full-text search, and persistent storage.
- `fastembed-rs` supports local ONNX inference and BGE-M3 joint dense/sparse/ColBERT embeddings.
- `ort` provides Rust bindings for ONNX Runtime when lower-level model control is needed.
- `clap` provides mature Rust CLI parsing and generated help.

Recommended revision:

- `jurisearch-core`: Rust retrieval core.
- `jurisearch-cli`: Rust CLI over the core.
- `jurisearch-ingest`: Rust index builder where practical.
- Optional `tools/ingest-python`: offline ingestion/conversion helpers only, never query-time.

Python can be useful for parsing DILA XML, using existing API clients, experimenting with Hugging Face datasets, or converting corpus dumps into canonical JSONL/Parquet. But the searchable artifact and all query paths should be built and read by Rust.

### P0: CLI-only creates a model cold-start risk that the design currently hides

The old design solved Python/model startup cost with `juri serve`. If `serve` is removed, repeated agent calls can reload the embedding model and reranker on every subprocess invocation.

Rust reduces binary startup, but it does not eliminate query-model load time. BGE-M3 or a cross-encoder reranker can still dominate latency if loaded per command.

Recommended design addition:

```text
jurisearch session --jsonl
```

Behavior:

- Starts one `jurisearch` process.
- Loads index readers and local models once.
- Reads one JSON command per line from stdin.
- Writes one JSON response per line to stdout.
- Supports the same logical commands as one-shot CLI: `search`, `fetch`, `cite`, `related`, `context`, `status`, `help`.
- Exits cleanly on `{ "command": "exit" }`.

This remains a CLI/subprocess interface, not MCP or HTTP.

### P0: Complete inline help must become part of the contract

With no MCP, the agent cannot rely on MCP tool descriptions for discovery. The CLI help must be self-contained and complete.

Required help surface:

- `jurisearch --help`: concise overview, command list, global flags, examples, and pointer to agent help.
- `jurisearch <command> --help`: complete command-specific flags, defaults, accepted values, examples, JSON output shape, exit codes, and common errors.
- `jurisearch help agent`: complete agent-facing contract in one call.
- `jurisearch help schema --json`: machine-readable schemas for commands and responses.

`jurisearch help agent` should include:

- Command inventory.
- When to use `search` vs `fetch` vs `cite` vs `related`.
- All flags and accepted enum values.
- JSON request/response examples.
- Pagination/cursor rules.
- Temporal query rules.
- Citation verification workflow.
- Exit codes.
- Error object schema.
- Token-budget guidance.

This is not optional documentation. It is part of the agent API.

### P1: The backend choice needs a Rust-specific spike

The current LanceDB recommendation is plausible but too Python-centric. LanceDB's docs state Rust support for vector search, full-text search, SQL, and local embedded operation. However, SDK feature coverage differs: model-based rerankers are documented as Python-only, while Rust exposes hybrid reranking through generic/custom interfaces and built-in RRF.

An embedded Postgres path may be a better v1 fit than LanceDB if it keeps documents, metadata, graph edges, temporal filters, and vectors in one local relational store. The Rust ecosystem has `pg-embed` and `postgresql_embedded`-style crates for starting a local Postgres process from a Rust application. `pgvector` gives vector search inside Postgres. Since AGPL-3.0 is acceptable, `pg_search`/ParadeDB should be treated as a serious default candidate for BM25/full-text search if packaging works; native Postgres FTS remains a fallback for simplicity, and standalone Tantivy remains a fallback for maximum tokenizer control.

Recommended decision:

Run an early Rust spike before locking the backend:

1. Index 50k LEGI article versions and 10k Judilibre decisions.
2. Build BM25 candidates with French tokenization.
3. Build dense candidates with BGE-M3 query embeddings.
4. Fuse with RRF in Rust.
5. Apply temporal filters before fusion.
6. Apply authority scoring after fusion.
7. Return stable JSON under 500 ms warm for common queries.

Backend candidates:

- Preferred local relational path: embedded Postgres (`pg-embed` or `postgresql_embedded`) + `pgvector` for dense vectors + Postgres tables for documents, chunks, graph edges, temporal metadata, manifests, and eval traces.
- Lexical options on the Postgres path: prefer `pg_search`/ParadeDB for BM25 if extension packaging works; use native Postgres FTS for a simpler fallback; use standalone Tantivy if French tokenizer control requires leaving Postgres FTS.
- Lance path: LanceDB Rust for vectors/metadata, possibly FTS, with custom Rust RRF/authority reranking where SDK gaps exist.
- Conservative no-Postgres path: Tantivy for lexical search + a local Rust vector index + SQLite/Parquet/Arrow metadata.
- Do not use Qdrant in v1. It is a separate search service and conflicts with the local embedded CLI shape unless all embedded options fail under benchmark.

Recommended immediate spike:

- Build one prototype with embedded Postgres + `pgvector` + `pg_search`.
- Build one fallback prototype with embedded Postgres + `pgvector` + native FTS.
- Build one prototype with Tantivy + embedded Postgres + `pgvector`.
- Compare warm latency, packaging complexity, French tokenization quality, temporal-filter performance, and index artifact portability.
- Treat Qdrant as out of scope for this project unless both prototypes fail.

### P1: Reranking should be staged, not promised too early

The design currently assumes `bge-reranker-v2-m3` in v1. In Rust, this requires proving model availability, tokenizer behavior, ONNX/Candle compatibility, latency, and packaging.

Recommended v1 staging:

- MVP: BM25 + dense retrieval + RRF + authority scoring.
- v1.1: optional local reranker after a Rust inference spike.
- Always benchmark: BM25-only, dense-only, hybrid, hybrid + authority, hybrid + reranker.

The legal ranking behavior should be validated by evals, not by model choice alone.

### P1: Ingestion/runtime boundaries need to be explicit

If Python is allowed for ingestion, the design must prevent Python from leaking into the runtime.

Recommended boundary:

- Python may download, parse, inspect, and normalize official sources.
- Python may emit canonical JSONL/Parquet/Arrow files.
- Rust owns schema validation, index construction, manifest generation, and all query execution.
- Rust tests validate that canonical records can be indexed and searched without Python.

Artifact contract:

```text
index/
  manifest.json
  pg/               # embedded Postgres data directory, if Postgres path wins
  lexical/          # optional Tantivy index, if not using Postgres FTS/pg_search
  vectors/          # optional external vector index, if not using pgvector
  docs/             # canonical stored documents/chunks, if not stored in Postgres
  graph/            # edge table, if not stored in Postgres
  schemas/          # versioned JSON schemas
```

The manifest should record source dataset versions, build date, corpus coverage, model versions, tokenizer version, and schema version.

### P1: The command examples must be renamed and tightened

Every design example should use `jurisearch`, not `juri`.

Suggested v1 command set:

```text
jurisearch search "<query>" --json
jurisearch fetch <id> --json
jurisearch cite <id-or-citation> --json
jurisearch related <id> --json
jurisearch context <id> --json
jurisearch status --json
jurisearch help agent
jurisearch help schema --json
jurisearch session --jsonl
```

Admin commands can exist, but should not distract from the agent contract:

```text
jurisearch ingest canonical <path>
jurisearch ingest legi <path>
jurisearch sync
```

If ingestion remains partly Python, the CLI should still describe the supported import artifact rather than requiring agents to know Python tooling.

### P2: `jurisearch` is a better agent-facing name than `juri`

`juri` is short, but ambiguous. `jurisearch` is longer, but more self-describing to agents and humans scanning tool lists.

Recommended:

- Binary: `jurisearch`
- Crate/workspace: `jurisearch`
- Config: `~/.config/jurisearch/config.toml`
- Env prefix: `JURISEARCH_`
- Internal IDs should not use `juri:` as a namespace. Use source-based namespaces such as `legi:`, `judilibre:`, `ja:`, `ecli:`, or `jurisearch:` only for synthetic internal objects.

Do not keep `juri` as the primary command. An alias can be added later if typing cost becomes a real issue.

### P2: The design over-weights Anthropic/MCP guidance

The agent ergonomics guidance is useful, but the design should avoid sounding coupled to one agent ecosystem. The interface is a POSIX-style CLI contract.

Recommended wording:

- Replace "MCP tool descriptions" with "inline CLI help and JSON schemas".
- Replace "Claude Desktop/Code" specificity with "LLM harnesses that can spawn subprocesses".
- Keep the useful principles: concise default, pagination, stable IDs, natural citations, actionable errors.

### P2: Evaluation should include CLI behavior, not just retrieval quality

The eval section is directionally right, but CLI-only adds more acceptance criteria.

Add eval checks for:

- `jurisearch help agent` contains every command and schema.
- One-shot CLI returns valid JSON for success, no-results, and bad-input cases.
- JSONL session handles multiple calls without restarting.
- Search/fetch/cite loop stays within token budgets.
- Exit codes are stable.
- No command prints non-JSON on stdout when `--json` is set.
- Stderr is reserved for diagnostics only.

## What to keep

Keep these parts of the design:

- The retrieval-only boundary: not a chatbot, not legal advice, not answer synthesis.
- The two-step `search` then `fetch` contract.
- Temporal correctness as a hard invariant.
- Structure-aware code/article chunking.
- Judilibre zone-aware decision chunking.
- Citation verification as a first-class command.
- Authority signals as ranking inputs, not hard filters.
- Modest graph traversal that returns candidate related material, not legal conclusions.
- Coverage/status reporting with provenance.

## Proposed revised architecture

```text
Official sources / dumps
        |
        | optional Python helpers for difficult parsing/prototyping
        v
Canonical records (JSONL/Parquet/Arrow, versioned schema)
        |
        | Rust schema validator + index builder
        v
Local jurisearch index artifact
        |
        | Rust retrieval core
        v
jurisearch CLI
  - one-shot commands
  - JSON output
  - JSONL session mode
  - complete inline help
```

Runtime components:

- CLI parser/help: `clap`.
- Storage/query substrate: embedded Postgres if packaging and extension support are acceptable.
- Vector search: `pgvector` on embedded Postgres, or a local Rust vector index if Postgres packaging fails.
- Lexical search: `pg_search`/ParadeDB for the first embedded-Postgres spike; native Postgres FTS as the simple fallback; standalone Tantivy if BM25/tokenizer control requires leaving Postgres FTS.
- Query embeddings: `fastembed-rs` BGE-M3 or direct `ort`/Candle implementation after benchmark.
- Fusion: custom Rust RRF.
- Authority scoring: custom Rust layer.
- Metadata/graph: Postgres tables when using embedded Postgres; otherwise embedded local tables in the index artifact.

## Concrete changes requested for the design docs

1. Rename `juri` to `jurisearch` everywhere.
2. Replace "Python core + LanceDB" with "Rust search/runtime core; Python allowed only for offline ingestion helpers".
3. Delete MCP, HTTP, FastMCP, and `serve` from v1.
4. Add `jurisearch session --jsonl` for warm multi-call agent use.
5. Add a "Complete inline help" section under the agent contract.
6. Add `jurisearch help agent` and `jurisearch help schema --json`.
7. Rework tech stack around Rust libraries and a Rust backend spike.
8. Change the reranker from guaranteed v1 to benchmark-gated staged delivery.
9. Define canonical ingestion artifacts and runtime index artifact layout.
10. Update evals to test CLI JSON behavior, help completeness, session mode, and exit codes.
11. Add an embedded Postgres backend option with `pgvector`, `pg_search` as the preferred lexical candidate, native FTS/Tantivy fallbacks, and Qdrant explicitly out of scope for v1.

## Research basis

French legal data:

- Legifrance API is available via PISTE after registration and uses the data from the Legifrance site: https://www.data.gouv.fr/dataservices/legifrance
- Legifrance stable API opened on 2023-04-04; use is under Licence Ouverte 2.0, PISTE terms, API terms, and quotas: https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api
- Judilibre is a free open database from the Cour de cassation, enriched and pseudonymised; it exposes publication level, summaries, texts applied, related jurisprudence, and attacked decisions: https://www.data.gouv.fr/datasets/api-judilibre
- Judilibre `GET /decision` returns full structured content including metadata, ECLI, publication, solution, date, full text, zones, applied texts, and jurisprudence rapprochements: https://github.com/Cour-de-cassation/judilibre-search/blob/dev/public/JUDILIBRE-public.json
- Conseil d'Etat says the full administrative-justice decisions corpus is available on the open-data platform, with Conseil d'Etat decisions from 2021-09-30 and next-day publication Tue-Sat: https://www.conseil-etat.fr/decisions-de-justice/donnees-ouvertes-open-data
- ECLI exists to identify, access, search, cite, and link EU and national judicial decisions: https://eur-lex.europa.eu/content/help/eurlex-content/ecli.html

Rust/search tooling:

- Tantivy is a Rust full-text search library inspired by Lucene, with BM25, tokenizers, incremental indexing, mmap, facets, range queries, and tiny startup time: https://github.com/quickwit-oss/tantivy
- LanceDB Rust SDK supports local persistent vector search, metadata, full-text search, SQL, and native Rust usage: https://docs.rs/lancedb
- LanceDB docs note hybrid search and RRF, but model-based reranker support differs by SDK; Python has provider rerankers while Rust exposes built-in/generic hybrid reranking interfaces: https://docs.lancedb.com/reranking
- `pg-embed` can run a local PostgreSQL database from a Rust application or test on Linux, macOS, and Windows: https://docs.rs/pg-embed
- `postgresql_embedded` provides an embedded-like Postgres experience for Rust by downloading or bundling PostgreSQL and running it as a separate local process: https://github.com/theseus-rs/postgresql-embedded
- `pgvector` stores vectors in Postgres and supports exact/approximate nearest-neighbor search, multiple vector types, and multiple distance metrics: https://github.com/pgvector/pgvector
- ParadeDB/`pg_search` brings Tantivy/BM25-style full-text search into Postgres. AGPL-3.0 is acceptable for this project, so the remaining question is packaging/runtime fit, not licensing: https://github.com/paradedb/paradedb
- `fastembed-rs` supports local ONNX inference and BGE-M3 joint embeddings: https://github.com/Anush008/fastembed-rs
- `ort` is a Rust binding for ONNX Runtime: https://docs.rs/ort
- Candle provides native Rust ML inference with CPU/CUDA/WASM backends: https://huggingface.github.io/candle/
- `clap` supports Rust CLI parsing, subcommands, value enums, command generation, and configurable help output: https://docs.rs/clap

## Bottom line

The design should not be implemented as written. The legal model is good; the system shape should be revised to:

- `jurisearch` as the command.
- Rust for search/runtime.
- Optional Python only before index build or as offline ingestion tooling.
- Embedded local storage first, preferably embedded Postgres + `pgvector` + `pg_search` if the spike validates packaging and latency.
- No Qdrant or other separate vector/search service in v1.
- CLI-only, with JSON and JSONL session mode.
- Complete inline help as the replacement for MCP discoverability.
