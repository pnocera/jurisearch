# `jurisearch` — Implementation Prerequisites

Date: 2026-06-21
Status: setup analysis — what must exist before the §10 first execution batch can start
Sources: `work/03-implementation/IMPLEMENTATION_PLAN.md`, `work/01-design/DESIGN.md` (§11, §13, §14, §16), `work/01-design/RESEARCH.md`, `work/01-design/DECISIONS.md`, reusable reference repo `/home/pierre/Apps/juridocs`
Scope: prerequisites only — toolchain, services, data, credentials, people, and compliance posture. No architecture re-decision; the stack is locked (`DECISIONS.md`).

---

## 0. TL;DR — the four things that actually gate kickoff

Most prerequisites are routine. Four are not, and they determine whether Phase 0 can even begin:

1. **Embedded Postgres + `pgvector` + `pg_search` packaging (now scoped — was the top risk).** This is the locked backend (D3). The big update since the first draft: **two in-house repos supply both halves.** `/home/pierre/Work/gciauto2` (`gci_db`) is a working `postgresql_embedded 0.20.2` integration that already proves most 0.3 lifecycle/packaging criteria — loopback binding, crash recovery, clean shutdown, single-writer lock — and `/home/pierre/Work/paradedb` is our `pg_search` fork with a standard `cargo pgrx package` recipe (pg15–18). What remains for 0.3 is a **well-scoped ABI/drop-in spike** (build the fork's extension for the embedded PG major, copy artifacts into the data dir, `CREATE EXTENSION`), not a from-scratch problem — see §2. Keep the native-FTS fallback ready, but it is now genuinely a fallback. Note `juridocs` does **not** help here: it runs an *external* Postgres with native FTS (see §8).
2. **PISTE credentials — ✅ acquired; prod Judilibre CONFIRMED; auth is split (live-verified 2026-06-21).** Prod Judilibre `/search` → HTTP 200 (`total: 109517`). **The two PISTE APIs use different auth:** **Judilibre accepts the `KeyId` API-Key header** (what we hold), but **Légifrance rejects `KeyId`** (returns 400 regardless) and uses the **OAuth2 Bearer** path via the app's separate **OAuth Client ID + secret** — now in `~/.zshrc` and **verified end-to-end** (OAuth2 token → Bearer → Légifrance `/search` → 200, prod + sandbox). The W8 client implements **both** schemes (see §6). Values migrated to `~/.zshrc` (`0600`); `secrets.txt` deleted. Caveat: the sandbox app needs the **Judilibre** subscription added (currently 403).
3. **OpenAI-compatible embeddings endpoint serving `bge-m3` — ✅ stood up & verified.** Local `llama.cpp` on `127.0.0.1:8097`, 1024-d, normalized, `pooling=cls` — semantic discrimination confirmed on French legal text. Recipe + fingerprint in `embeddings-endpoint.md`. Unblocks 0.4/0.6 dense work.
4. **A legal-domain reviewer for gold labels (long-lead, human).** Every "best-in-class" gate depends on legally-credible gold article IDs/ECLIs (W2, 0.2). Securing the reviewer is slower than writing code — identify them before Phase 0, not at the Phase 1 gate.

Everything else (Rust toolchain, official XML dumps, the `juridocs` parser reference) is acquirable in hours and is on no critical path.

---

## 1. Build toolchain & developer environment

| Prerequisite | Concrete target | Notes / source |
|---|---|---|
| **Rust toolchain** | Pin **≥ 1.96** via `rust-toolchain.toml` | Floor is set by the backend reuse, not `juridocs` (1.80): `postgresql_embedded 0.20.2` needs ≥1.92 and the `paradedb` `pg_search` fork pins **1.96.0**. Backs all of W1–W8. |
| **C/C++ build toolchain** | `cc`, `make`, `pkg-config`, headers | Needed to build `pgvector` (C) and the `pgrx`-built `pg_search` against the embedded Postgres. |
| **`cargo-pgrx` + `pgrx` 0.18.1** | `cargo install --locked cargo-pgrx --version 0.18.1`; Postgres server headers for the target major | Required to build the `paradedb` fork's `pg_search` against the embedded PG — the fork's `make package` wraps `cargo pgrx package --pg-config <X>`. See §2. |
| **ONNX Runtime / `ort` prerequisites** | system libs or `ort` download-binaries feature | For in-process embeddings (`fastembed-rs`, optional) and the local reranker path (0.7). `juridocs` uses `fastembed 5.11` with `ort-download-binaries`. |
| **`psql` client + libpq** | any recent | Spike inspection, migrations, debugging the child Postgres. |
| **Git** | any | Plus read access to `/home/pierre/Apps/juridocs` for the reuse reference. |
| **Optional: Python 3.x** | only if Python ingestion helpers are used | The design keeps Python **offline-ingestion-only, before canonical records** (D-rust-only). The Rust XML parser is the primary path (W4 "from day one"), so Python is genuinely optional, not required. |

**Key crate baselines to reuse from `juridocs`** (proven versions, adapt as needed): `tokio 1.49`, `sqlx 0.8.6` (postgres/uuid/time/macros), `pgvector 0.4.1`, `clap 4.5`, `quick-xml 0.39.2`, `fastembed 5.11`. New to jurisearch (not in `juridocs`): the embedded-Postgres crate (`postgresql_embedded` / `pg-embed`), a `pg_search` integration, an OpenAI-compatible embeddings client, and `ort`/Candle for the reranker.

---

## 2. Storage backend — embedded Postgres + `pgvector` + `pg_search`  ⚠ critical path

This is locked (D3). It *was* the highest-risk prerequisite; with the two in-house repos (`gciauto2` + the `paradedb` fork) it is now a **scoped spike**, not an open problem. "Embedded Postgres" = a managed local Postgres **child process** (`postgresql_embedded`/`pg-embed`), not an in-process library (DESIGN §13.1).

**What must be acquired / proven before 0.3 can pass:**

| Item | Requirement | Risk |
|---|---|---|
| Postgres binaries | Bundled vs downloaded-and-cached; **offline-install story** documented | The `postgresql_embedded` distributions (Zonky/theseus-rs) are **vanilla** — they do not ship third-party extensions. |
| `pgvector` (server extension `vector`) | Pinned build compatible with the embedded Postgres major; installed into the data dir | Lower risk — widely available; `juridocs` already runs `CREATE EXTENSION vector` on PG 18. |
| `pg_search` (ParadeDB) | Build the **forked** `pg_search` for the same PG major as the embedded binaries; drop artifacts into the data dir's `lib`/`share` | **Now scoped, not open.** We own the fork (`/home/pierre/Work/paradedb`, `pgrx 0.18.1`, supports pg15–18) with a standard `cargo pgrx package --pg-config <X>` recipe. Residual unknown = ABI-matching the pgrx build to the theseus/Zonky embedded binaries (recipe below). Must be added to `shared_preload_libraries`. |
| Private binding | Unix socket or ephemeral loopback only; no public exposure by default | Spike acceptance (DESIGN §13.3). |
| Single-writer lock | One writer across concurrent `jurisearch` processes on one index dir | New mechanism. |
| Lifecycle | Startup, crash recovery, clean shutdown, no orphaned processes / stale locks | New mechanism. |
| Migration mechanics | Index/schema/extension migration across `jurisearch` versions | Needed before 1.0 operational tables land. |
| Platform policy | Documented (v1 Linux-only acceptable) | Extension packaging is the most platform-sensitive part. |

**In-house head start — the two repos supply both halves of 0.3.**
- **`/home/pierre/Work/gciauto2`** (`crates/gci_db/src/embedded.rs`, `advisory.rs`) is a production-grade `postgresql_embedded 0.20.2` integration that **already proves most of the 0.3 packaging/lifecycle criteria**: child Postgres on `127.0.0.1` + ephemeral port (private binding), durable data dir, version pinning via `VersionReq`, clean `pg_ctl stop -m fast` shutdown, crash-recovery/orphan-reclaim (`reclaim_data_dir` + provably-dead `postmaster.pid` clearing), single-writer locking (a PG advisory lock keyed on the data-dir path), and `0o700` PGDATA. **Near-verbatim reusable for W3.** That source repo pins PG **17**; jurisearch Phase 0 is now locked to PG **18**, so reuse the lifecycle mechanics while building the runtime and extensions against the jurisearch PG18 prefix.
- **`/home/pierre/Work/paradedb`** is our `pg_search` fork with the packaging recipe: `make package` → `cargo pgrx package --package pg_search --pg-config <PG_CONFIG>` produces the installable `.so` + `.control` + `.sql`.
- **Combined recipe to prove in 0.3:** (1) bring up managed Postgres using the gciauto2 lifecycle pattern, pinned to the jurisearch PG18 runtime; (2) build the fork's `pg_search` and `pgvector` for that **same major/prefix** via `cargo pgrx package`; (3) copy the artifacts into the runtime's `pkglibdir`/`sharedir`; (4) add `pg_search` to `shared_preload_libraries` in the conf map, then `CREATE EXTENSION pg_search; CREATE EXTENSION vector;`.
- **Residual risk (the only real unknown left):** ABI match between the pgrx build and the theseus-rs/Zonky embedded binaries. If those bundles ship `pg_config` + server headers, build directly against them (exact match); if not (likely — they are runtime bundles), build against an official PG of the same major+platform and copy artifacts in, which works when major/platform/compiler/critical configure flags align. This is a 1–2 day spike, not an open-ended risk.

**Fallback readiness (must be a live plan, per locked precedence):**
1. **native Postgres FTS** if `pg_search` packaging fails — note `juridocs` already proves this path works (French `to_tsvector` + `unaccent` + `pg_trgm`), so it is a *de-risked* fallback;
2. standalone **Tantivy** + local Rust vector index + SQLite/Arrow if embedded Postgres itself fails;
3. **LanceDB** only if the Postgres route fails both packaging and quality.

**Recommendation:** time-box the `pg_search`-on-embedded-Postgres spike early. If it stalls, the native-FTS fallback (already validated in `juridocs`) keeps Phase 0 moving without reopening the backend decision.

**Phase 0 policy recorded:** see [`storage-backend-policy.md`](./storage-backend-policy.md). The accepted Phase 0 path is Linux x86_64, PostgreSQL 18 through a pgrx-managed or pgrx-like `pg_config` prefix, and pre-staged matching `pg_search`/`pgvector` artifacts for offline installs.

---

## 3. Embeddings infrastructure  — ✅ endpoint verified; bge-m3 locked

Locked: OpenAI-compatible `/v1/embeddings` endpoint, hosted or local loopback (D5). Document and query embeddings must share one fingerprint; mismatch is a hard error (DESIGN §11.2). **Verified: `bge-m3` via local `llama.cpp`, now LOCKED as v1 (D21) after CamemBERT/Solon validation — launch command, test results, fingerprint, and the measured 3-node build-time throughput pool in [`embeddings-endpoint.md`](./embeddings-endpoint.md).**

| Prerequisite | Target | Notes |
|---|---|---|
| **An embeddings endpoint** | `llama.cpp` server on `127.0.0.1` **or** a hosted OpenAI-compatible API | Required for 0.4 and 0.6. A local endpoint is still treated as a "remote provider." |
| **`bge-m3` model** | **locked v1** (D21) — 1024-dim, CLS pooling, normalize | RESEARCH §2; config in DESIGN §14. `llama.cpp` requires pooling ≠ `none` and a dedicated embedding model (not a chat model). |
| ~~Phase 1 candidate models~~ — done | `sentence-camembert-large`, `Solon` validated 2026-06-22 | Both statistically tied bge-m3 → bge-m3 locked (D21); bake-off retired. |
| **Model cache (in-process mode only)** | `JURISEARCH_MODEL_DIR` (default `~/.cache/jurisearch/models`) | Only if `provider = in_process` (`fastembed-rs`); downloads off by default, `model fetch` to populate. |
| **GPU/CPU + build-time pool** | 3 fingerprint-identical bge-m3 nodes (localhost + `192.168.1.57` + `.27`) | Measured 2026-06-22: localhost ~58 t/s (slow/contended), remotes ~146–194 t/s, pooled ~288 t/s → the ~1.85 M-chunk dense projection drops ~8.9 h → ~1.8 h (~5×). Build-time only; retrieval uses localhost. See `embeddings-endpoint.md`. |

**Fingerprint contract to fix up front:** provider, base-url class, model, dimension, normalization, pooling — pinned in the manifest. Decide the Phase 0 endpoint (local vs hosted) before 0.4.

---

## 4. Reranker stack (Phase 0.7 spike — not on the early critical path)

Benchmark-gated and pluggable (`disabled | local | http`); adoption is eval-gated, never assumed (DESIGN §7.2).

- **Candidate model:** `bge-reranker-v2-m3` (multilingual cross-encoder, ~0.6B, 512-token pairs).
- **Local inference:** `ort` (ONNX Runtime bindings) and/or Candle; verify tokenizer availability and ONNX/Candle compatibility, latency, packaging.
- **HTTP fallback:** a rerank endpoint (e.g. hosted Cohere/Jina) if local packaging lags.
- **Prerequisite to run 0.7:** the eval metric harness (0.2) must exist first (it feeds the adoption decision).

---

## 5. Official data sources & corpus access  ⚠ long-lead for bulk download/storage

All sources are official and free-reuse (Licence Ouverte / DILA décret 24 June 2014). The authoritative index is **official-XML-only** (D7/D14) — derived HF datasets are comparison/smoke-test fixtures only, never ingestion inputs.

| Source | What to acquire | For tasks | Notes |
|---|---|---|---|
| **LEGI bulk XML** | "Codes, lois et règlements consolidés" dumps from data.gouv.fr / `echanges.dila.gouv.fr` (`.tar.gz` archives) | 0.5, 0.5a, 1.1 | Temporal fields `dateDebut`/`dateFin`; statuses `VIGUEUR`/`MODIFIE`/`ABROGE`/`ABROGE_DIFF`; IDs `LEGIARTI`/`LEGITEXT`/`LEGISCTA`. Need a **representative subset first** (0.5), then full baseline + deltas (1.1). |
| **Official DTDs** | LEGI/DILA DTD files matching the current dumps | 0.5, 1.1 | Re-verify required fields against **current** DTDs; the `juridocs` DTD matrix is a checklist only. |
| **Judilibre** | API access via PISTE (see §6); `/decision`, `/search`, `/export`, `/taxonomy`, `/transactionalhistory` | 2.1, 2.5 | Official **zone offsets** (primary chunk boundaries) + transactional history for deltas. Pseudonymised at source. |
| **Justice administrative** | `opendata.justice-administrative.fr` XML (Conseil d'État, 9 CAA, 42 TA) | 2.2 | IDs `CETATEXT` + ECLI. |
| **DILA bulk jurisprudence** (optional) | `cass`/`inca`/`capp`/`jade` archives | 2.2a (only if accepted) | Roots `TEXTE_JURI_JUDI`/`TEXTE_JURI_ADMIN`; no official zones → fallback chunking. Optional, flagged. |
| **Derived datasets (fixtures only)** | `AgentPublic/legi`, `legalkit` | regression/smoke tests | **Never** seed canonical records, chunks, or embeddings. |

**Storage prerequisite:** disk for raw archives + extracted XML + canonical records + Postgres data dir + vectors. Size the Phase 0 spike target concretely (DESIGN §13.3): **~50k LEGI article versions + ~10k Judilibre decisions**; the full LEGI baseline (1.1) is materially larger — provision accordingly before full-corpus ingestion.

**Coverage caveat:** Judilibre and justice-administrative coverage dates advance; re-check current ranges before any `status` completeness claim (RESEARCH §6).

---

## 6. Official API credentials (PISTE)  — ✅ acquired; prod Judilibre confirmed

Needed for `cite --online` (1.4), Judilibre ingestion (2.1), and `sync` (2.5). Built in 0.8 (W8).

**Live-verified 2026-06-21 (via `KeyId` header):**
- [x] **Production Judilibre — SUBSCRIBED & working.** `GET https://api.piste.gouv.fr/cassation/judilibre/v1.0/search` → HTTP 200, `total: 109517`. Auth = `KeyId: <prod API key>`.
- [x] **Credentials migrated to `~/.zshrc`** (now `0600`); `secrets.txt` deleted.
- [ ] **Sandbox Judilibre — 403 (NOT subscribed).** Portal confirms the sandbox app (`APP_SANDBOX…`) has **"No subscribed APIs"**, while the prod app (`Juridia`) lists **"JUDILIBRE 1.0.0"**. App approval/enabled ≠ subscription. Add the Judilibre API to the sandbox app for sandbox testing; until then, dev/test against prod (rate-limited) or use decision fixtures.
- [x] **Légifrance API — OAuth2 confirmed working (2026-06-21, prod + sandbox).** `KeyId` does *not* authenticate Légifrance (400-empty), but **OAuth2 client-credentials → Bearer → `/search` → 200** with real results. OAuth Client ID + secret now in `~/.zshrc`; recipe below. Only needed for `cite --online` (LEGI ingestion uses bulk XML, §5).

**⚠ The two PISTE APIs use *different* auth — both schemes required and BOTH VERIFIED (2026-06-21).** RESEARCH §1 assumed OAuth2 everywhere. Each app exposes **both** an **API Key** (`KeyId` header) **and** a separate **OAuth Client ID** (prod `79fb0538…`, sandbox `898ed46a…`). Tested on sandbox + prod: **Judilibre accepts `KeyId`** (200); **Légifrance rejects `KeyId`** (400-empty) but works via **OAuth2 client-credentials → Bearer** (200). W8 implements **both**. All four credential sets (2 API keys + 2 OAuth client id/secret) are in `~/.zshrc` (`0600`).

**Verified auth recipe (for 0.8 / W8):**
- **Judilibre** — `GET https://api.piste.gouv.fr/cassation/judilibre/v1.0/{search,decision,taxonomy,…}` with header `KeyId: $PISTE_API_KEY`.
- **Légifrance** — `POST https://oauth.piste.gouv.fr/api/oauth/token` (form: `grant_type=client_credentials`, `scope=openid`, `client_id=$PISTE_OAUTH_CLIENT_ID`, `client_secret=$PISTE_OAUTH_CLIENT_SECRET`) → take `access_token` → `POST https://api.piste.gouv.fr/dila/legifrance/lf-engine-app/search` with `Authorization: Bearer <token>`. Sandbox: swap `api`/`oauth` → `sandbox-api`/`sandbox-oauth` and use the `PISTE_SANDBOX_*` vars.

- Awareness of **PISTE rate limits** → bulk dumps for full builds, API for deltas + live citation verification.

**Secret handling.** Values live in `~/.zshrc` env (`0600`). The env vars `PISTE_API_KEY` / `PISTE_API_SECRET` and `PISTE_SANDBOX_API_KEY` / `PISTE_SANDBOX_API_SECRET` hold the API key/secret for the Judilibre `KeyId` header; `PISTE_OAUTH_CLIENT_ID` / `PISTE_OAUTH_CLIENT_SECRET` and sandbox equivalents hold the Légifrance OAuth client credentials. **A non-interactive shell does not inherit them** (`~/.zshrc` is interactive-only), so automation/CI must source the block or read from a keyring. **If this transcript leaves your machine, rotate the keys on the PISTE portal** (values appeared in tool output during setup).

---

## 7. Secrets & configuration

- **Config file:** `~/.config/jurisearch/config.toml` — index path, `--format`, `[embedding]`, `[reranker]`, authority weights, corpora (DESIGN §14).
- **Secrets (env / OS keyring, never on disk, never logged):**
  - `PISTE_API_KEY` / `PISTE_API_SECRET` (+ `PISTE_SANDBOX_API_*`) — **in `~/.zshrc` (`0600`)**; hold the **Axway API key/secret** sent as a `KeyId` header for Judilibre — see §6;
  - `PISTE_OAUTH_CLIENT_ID` / `PISTE_OAUTH_CLIENT_SECRET` (+ sandbox equivalents) — **in `~/.zshrc` (`0600`)**; hold the OAuth2 client credentials for Légifrance — see §6;
  - `JURISEARCH_EMBED_API_KEY` (optional; omit / `no-key` for local `llama.cpp`).
- **Env override prefix:** `JURISEARCH_` (e.g. `JURISEARCH_INDEX_PATH`, `JURISEARCH_EMBED_BASE_URL`, `JURISEARCH_MODEL_DIR`).
- **OS keyring** available on the dev/CI platform for secret storage.

---

## 8. Reusable in-house assets (`juridocs`, `gciauto2`, `paradedb`)

`/home/pierre/Apps/juridocs` is the battle-tested ingestion playbook (see `work/notes/2026-06-21-juridocs-ingestion-reuse.md`). Reuse it as a **reference for ingestion**, but be precise about the boundary:

**Reusable (accelerates W4 / 0.5 / 0.5a / 1.0 / 1.1 / 1.2):** archive precedence/streaming, DTD-backed LEGI parser models + validation, temporal versioning rules, ingest run/member/error accounting + resume/quarantine, canonical payload hashing, French sentence splitting + guardrails, link extraction, hierarchy/`CONTEXTE` extraction, jurisprudence bulk parser, quality-gate scripts. Proven crates: `quick-xml 0.39`, `pgvector 0.4.1`, `sqlx 0.8`, `fastembed 5.11`.

**NOT reusable — must be built fresh (this is the key prerequisite insight):**
- **Storage backend.** `juridocs` runs an **external Postgres 18** via `DATABASE_URL` with **native French FTS** (`to_tsvector('french', unaccent(...))` + `pg_trgm`), extensions `pgcrypto/unaccent/pg_trgm/citext/vector` — **no `pg_search`, no embedded Postgres.** jurisearch's locked backend (embedded Postgres child process + `pg_search`) is a different shape; `juridocs` does **not** de-risk task 0.3 at all. It *does* prove the native-FTS fallback works.
- **Search/storage contracts.** `juridocs` `search_document`/`reference_index` and its mean-pooled entity vector are explicitly **not** the jurisearch model (chunk-direct `Document`/`Chunk`/graph + CLI agent contract).
- **Vector dimension assumption.** Take dimension from the embedding fingerprint, not a hardcoded `vector(768)`.

**`/home/pierre/Work/gciauto2` — the embedded-Postgres reference (new).** Reuse `gci_db`'s `embedded.rs` + `advisory.rs` for the W3/0.3 child-Postgres lifecycle, loopback binding, crash-recovery/orphan-reclaim, and single-writer locking (see §2). It does **not** install extensions, so the `pg_search`/`pgvector` drop-in step is the one part it does not cover.

**`/home/pierre/Work/paradedb` — the `pg_search` fork (new).** Our own `pgrx 0.18.1` build (pg15–18) with a `cargo pgrx package` recipe — this is what makes the locked backend buildable in-house against the embedded Postgres (see §2).

**Practical prerequisite:** keep read access to all three. `juridocs` = ingestion parsers/loaders/tests (copy-adapt) **and** the proven native-FTS *fallback*; `gciauto2` = the embedded-PG lifecycle harness; `paradedb` = the buildable `pg_search`. Treat `juridocs`' DB layer (external PG, no `pg_search`) as a counter-example, not a template.

---

## 9. Domain-review prerequisites

Code is the fast part; legal credibility is the slow part. A frontier LLM (codex / GPT-5.5-xhigh, Claude, etc.) is a **force-multiplier** for project-owned labels, not a substitute for legally credible ground truth.

- **Phase 1 external benchmark gate:** because no local legal-domain reviewers are available, the LEGI/statutory Phase 1 claim uses an external expert-annotated French legal retrieval benchmark gate instead of promoting internal LEGI fixtures. BSARD is the primary candidate; LLeQA is a gated secondary candidate. The runner may live outside `jurisearch` and be written in Python, as long as it records durable metrics evidence for the status gate, including dataset revision, jurisdiction, eval-only usage scope, license implications, and the Belgian-law to French-LEGI applicability argument.
- **Legal-domain reviewer(s), if/when available:** project-owned LEGI/Judilibre gold labels still require named ownership for expected article IDs / ECLIs, review status, rationale, and held-out split before they can become project-authored release-gating labels.
- **Vocabulary seed lexicon sourcing/review** for `expand` (1.3) — legal-term synonyms need sourced, reviewed provenance, not invented lists.
- **Minimum eval category coverage owner**: known-article lookup, conceptual statutory, historical `--as-of`, citation states, jurisprudence-by-facts, statute→jurisprudence.

### Gold-label workflow — LLM-draft → official-source-verify → human sign-off

**Why no LLM-only gold set:** the labels *are* the ground truth. Generating *and* validating them with an LLM makes the eval measure "agrees with the LLM," not "legally correct" — and LLMs fail on exactly jurisearch's targets (precise `LEGIARTI` *versions*, ECLIs/pourvois, temporal validity, citation states). Using that class of system to define correctness bakes the bug into the ruler. (True of any LLM, Claude included.)

1. **Draft (LLM):** propose candidate queries/tasks + expected article IDs / ECLIs / citation states.
2. **Verify against the official source — not the model's memory:** confirm every candidate against the **Légifrance / Judilibre APIs** (now available, §6) — exact ID, version, and as-of validity.
3. **Named-human sign-off:** a legal-domain reviewer approves/corrects; only then is a label `release-gating`. Records `drafted_by`, `verified_against`, `reviewer`, status, rationale.
4. **Adversarial / coverage pass (LLM):** flag human↔model and label↔retrieval disagreements for re-review; check category coverage + held-out split.

**Two tiers:**
- **Dev / regression fixtures (non-gating):** LLM-drafted + official-source-checked — fine to author now; **unblocks 0.2 without waiting on a reviewer**.
- **External expert benchmark gate (Phase 1):** BSARD/LLeQA-style expert-annotated retrieval evidence can gate the release claim when local reviewers are unavailable.
- **Project-owned release-gating gold set (later / Phase 2):** smaller, LLM-assisted but **human-verified**, with named-reviewer metadata. Scale the cheap tier wide; keep the project-owned gating tier authoritative.

Identify a reviewer before project-owned gold labels become release-gating. Until then, do not promote internal labels by assertion; use the external benchmark gate for Phase 1.

---

## 10. Compliance & licensing posture (decide/record before distribution)

- **AGPL-3.0 project posture** is locked and is what permits bundling `pg_search` — but bundling AGPL `pg_search` into a distributed binary triggers **source-availability obligations for the combined work**. Prepare the release checklist (W7) before any distribution.
- **Licence Ouverte (Etalab) attribution** recorded in `status`/manifest for all official sources.
- **Pseudonymisation preservation:** never re-identify, never cross-link to defeat source pseudonymisation (DESIGN §16); tested in Phase 2.
- **No legal-advice framing** in outputs/docs — research aid only.
- **Secrets never logged**; upstream API errors surfaced as actionable, exit code `5`, without leaking tokens.

---

## 11. Hardware, platform & network

- **Platform:** Phase 0/v1 is Linux x86_64 only; macOS/Windows require a separate extension-packaging proof. See [`storage-backend-policy.md`](./storage-backend-policy.md).
- **Disk:** raw archives + extracted XML + canonical records + Postgres data dir + pgvector indexes + (optional) local models. Size for the spike first, then the full LEGI baseline.
- **RAM/CPU:** memory-mapped index reads keep query startup small; ingestion (parsing + embedding) is the resource-heavy phase.
- **Network:** required for first-time acquisition (Postgres binaries, crates, models, official dumps, API). Offline/air-gapped installs must pre-stage the PG18 runtime, matching `pg_search`/`pgvector` assets, model files, official archives/DTDs, and config pointing `JURISEARCH_PG_CONFIG` at the staged runtime.

---

## 12. CI / test infrastructure

- **Test database** for integration tests: a managed embedded-Postgres instance with pinned extensions (or, during the fallback period, the native-FTS path `juridocs`-style).
- **Deterministic fixtures:** temporal fixtures (current/modified/abrogated/sentinel/same-day), citation-state fixtures (six states), archive-ordering fixtures, replay snapshots — many adaptable from `juridocs` golden fixtures.
- **CLI-contract test rig:** stdout/stderr discipline, exit codes (`0/2/3/4/5`), `help`/`help schema --json` with no index.
- **No-Python-in-runtime test:** a Rust test proving canonical records index + search with zero Python (DESIGN §13.4).

---

## 13. Prerequisite → Phase 0 task readiness matrix

| Task | Hard prerequisites that must exist to START |
|---|---|
| 0.1 Workspace skeleton | Rust toolchain, `clap`, git. (Nothing exotic.) |
| 0.2 Eval harness first cut | 0.1 schema stubs; **legal reviewer identified** (for credible fixtures); fixture format decided. |
| 0.3 Embedded Postgres spike | `cargo-pgrx 0.18.1` + PG18 server headers; reuse `gciauto2` lifecycle + `paradedb` fork; ABI-match the `pg_search`/`pgvector` build to the selected PG18 runtime and drop into that prefix. |
| 0.4 Embeddings endpoint contract | **A running `/v1/embeddings` endpoint** (local `llama.cpp` or hosted) serving **`bge-m3`**; fingerprint fields decided. |
| 0.5 LEGI XML ingestion spike | **Representative LEGI XML subset** + **current DTDs**; `juridocs` parser as reference. |
| 0.5a Archive precedence + streaming | A few **real LEGI archives** (baseline + ≥1 delta) to test ordering; `juridocs` archive module as reference. |
| 0.6 Baseline hybrid retrieval | 0.3 + 0.4 + 0.5 done (backend, embeddings, canonical subset). |
| 0.7 Reranker feasibility spike | 0.2 metric harness; `ort`/Candle deps; `bge-reranker-v2-m3`. |
| 0.8 Official API client foundation | **PISTE sandbox credentials** + Judilibre subscription; secrets wired. |

---

## 14. Blocking vs deferrable

**Blocking before Phase 0 (acquire now):**
- Rust toolchain + build/extension toolchain (§1, §2).
- Decision + first proof on embedded Postgres binary source and `pg_search` install (§2) — the gating unknown.
- A `bge-m3` embeddings endpoint (§3).
- A representative LEGI XML subset + current DTDs (§5).
- ~~PISTE creds / Judilibre subscription~~ **done** — prod Judilibre confirmed (KeyId auth); creds in `~/.zshrc`. Remaining: sandbox Judilibre subscription (currently 403) + confirm Légifrance auth method (§6).
- **Record** the external expert benchmark gate decision (§9) — done for Phase 1; reviewer identification remains useful before project-owned release-gating labels.

**Deferrable to when its task starts:**
- Full LEGI baseline + delta archives and their storage (1.1).
- Reranker model/inference deps (0.7).
- Production PISTE credentials and Judilibre/justice-admin bulk (Phase 2).
- AGPL source-availability release checklist (before first distribution).
- DILA bulk jurisprudence archives (only if 2.2a is accepted).

---

## 15. Open decisions to resolve before kickoff

1. **PG major + ABI-match strategy for `pg_search`** — Phase 0 uses PostgreSQL **18** and requires `pg_search`/`pgvector` artifacts built against the same `pg_config` prefix used at runtime. See [`storage-backend-policy.md`](./storage-backend-policy.md).
2. **Phase 0 embeddings endpoint** — local `llama.cpp` vs hosted API for the spike. *(Drives 0.4/0.6.)*
3. **Platform policy** — resolved for Phase 0: Linux x86_64 only; macOS/Windows need a separate packaging proof.
4. **Offline-install scope** — resolved for Phase 0: online acquisition is acceptable for development; offline installs must pre-stage the runtime/extensions/models/corpora listed in [`storage-backend-policy.md`](./storage-backend-policy.md).
5. **Legal reviewer engagement / external benchmark gate** — Phase 1 uses external expert-annotated benchmark evidence; reviewer cadence remains needed before project-owned labels become release-gating.
6. **DILA bulk jurisprudence (2.2a)** — accept or leave parked. *(Phase 2 scope; already deferable.)*

---

## 16. Acquisition checklist (actionable)

- [ ] Pin Rust toolchain (`rust-toolchain.toml`); install C toolchain, `pkg-config`, `psql`.
- [ ] Decide embedded-Postgres binary source; obtain Postgres + `pgvector` + `pg_search` builds that match; prove `CREATE EXTENSION vector` and `pg_search` in a throwaway data dir.
  - 2026-06-21 proof: `work/03-implementation/00-setup/smoke-pg-extensions.sh` and `crates/jurisearch-storage/tests/extension_smoke.rs` prove the throwaway-data-dir extension path against the current pgrx-managed PG 18 prefix. `crates/jurisearch-storage/tests/durable_lifecycle.rs` now proves persistent PGDATA restart, concurrent-owner rejection, extension bootstrap, and vector query behavior. Remaining for this checkbox: package/pre-stage the actual runtime artifacts for a distributable/offline install.
- [x] Record Phase 0 storage platform/offline policy in [`storage-backend-policy.md`](./storage-backend-policy.md).
- [ ] Stand up a `bge-m3` OpenAI-compatible endpoint (local `llama.cpp` or hosted) and confirm a 1024-dim vector with mean pooling.
- [ ] Download a representative LEGI XML subset + the matching official DTDs.
- [ ] Fetch ≥1 baseline + ≥1 delta LEGI archive for ordering tests.
- [ ] Register on PISTE; request **sandbox** OAuth2 credentials + Judilibre subscription; queue the production request.
- [ ] Wire secrets via env/OS keyring (`PISTE_*`, optional `JURISEARCH_EMBED_API_KEY`); never commit.
- [ ] Confirm read access to `/home/pierre/Apps/juridocs` and bookmark the reusable modules.
- [x] Record the external expert benchmark gate for Phase 1.
- [ ] Identify and brief the legal-domain reviewer before promoting project-owned gold labels.
- [ ] Record platform policy, offline-install scope, and the AGPL release-checklist owner.

---

## 17. Bottom line

The project is not gated by ordinary tooling. With the two in-house repos in hand, the former #1 risk — the `pg_search`-on-embedded-Postgres backend — drops to a **scoped ABI/drop-in spike**: `gciauto2` supplies the embedded-PG lifecycle harness and `paradedb` supplies a buildable `pg_search` that supports the selected PG18 target. The remaining real lead-time items are now external evidence and services: benchmark dataset access/metrics evidence for the Phase 1 quality gate, PISTE follow-ups, and running `bge-m3` embeddings endpoints. A legal-domain reviewer is still needed before project-owned labels become release-gating, but is no longer the Phase 1 blocker. Budget a focused 0.3 spike to ABI-match the fork's extension build to the selected runtime and prove `CREATE EXTENSION pg_search` in an embedded data dir, but it is no longer an open-ended unknown. (Earlier framing said `juridocs` doesn't de-risk the backend — still true; the de-risking comes from `gciauto2` + `paradedb`, which the first draft hadn't seen.)
