# Central ingest + packaged distribution — deployment & test prerequisites

Date: 2026-06-26
Status: Prerequisites (real-world capabilities the built system depends on — not a build plan)
Companion to: `2026-06-26-central-ingest-package-distribution-implementation-plan.md` (the phases)
Builds against: the design (`2026-06-26-…-design.md`) and analysis (`2026-06-25-…-analysis.md`)

> The implementation plan says **what to build and in what order**. This document says **what must
> exist around the code** for the built system to actually be *deployed* and *tested in real-world
> conditions*. These are not coding tasks — they are environments, credentials, keys, data, infra,
> legal clearances, and operational procedures that the code assumes but cannot create for itself. Each
> item names *what*, *why*, *which plan phase it blocks*, and *who owns it* (Eng / Ops / Legal / Data).
> Section references like "§7.4" point at the **design** document; "P3" etc. at the **implementation
> plan** phases.

---

## 0. Why a separate prerequisites document

Three properties of this system make its prerequisites unusually load-bearing, so they are called out
on their own rather than buried in the phases:

1. **It is a distributed trust system.** Clients apply server-built data, schema, *and* prebuilt
   embeddings they cannot cheaply re-derive (§11.2). Nothing can be tested for real without a **signing
   trust root** and a way to get the public half to clients — including on physical media.
2. **It is a two-machine system at minimum.** "Producer builds, client applies" cannot be validated on
   one host. A real test needs a producer and **at least one genuinely separate client machine with its
   own PostgreSQL**, ideally two clients to prove per-corpus entitlement and generation independence.
3. **Its inputs are gated and expensive.** A real baseline needs a real ingested corpus, real
   embeddings, and **upfront** PISTE/Judilibre enrichment under quota (§9.1). These are procurement and
   data-pipeline prerequisites, not code.

---

## 1. Environments and hardware

| # | Prerequisite | Why | Blocks | Owner |
|---|---|---|---|---|
| E1 | **Producer host**: one PostgreSQL 18 with `pgvector` + `pg_search`, enough storage for the authoritative corpus **plus** the `package_change_log` outbox **plus** a staging-apply DB (§5.4) **plus** built artifacts before publish. | The producer holds authoritative state, the outbox, and builds/stages packages. | P1, P3, P9 | Ops/Eng |
| E2 | **≥1 separate client host** with its own PostgreSQL 18 + `pgvector` + `pg_search`, distinct from the producer (different machine, not just a different DB). | "Apply on a second machine" is the first end-to-end proof (M3); cross-host surfaces the real extension/version/arch constraints client-build is designed to avoid. | P3 | Ops/Eng |
| E3 | **A 2nd client host** (ideally differing in PostgreSQL minor/extension build) entitled to a *different* corpus mix. | Proves per-corpus generation independence (§4.1) and entitlement tiering (§11.3) — a single client can't. | P5, P6, P9 | Ops/Eng |
| E4 | **Physical media** (USB key / SSD) sized for the full baseline, plus a **media-writing + verification station** and a documented handling/chain-of-custody procedure. | Baselines and re-baselines ship on media (§6.1); the media is signed/verified on apply (§11.2) and is a logistics/trust line item (analysis risk #3). | P3, P5 | Ops |
| E5 | **A reference client hardware profile** (CPU/RAM/disk) used to calibrate apply-time budgets. | The catch-up policy is partly apply-cost-driven (§9.4); the budget must be measured on a named profile, not guessed. | P7 | Eng/Ops |
| E6 | **Network path** producer-hosting → clients with TLS termination, and a way to **simulate offline / long-offline** clients (disconnect, then catch up). | Incrementals cross the network (§6.1); catch-up + gap-free ordering must be tested against real offline windows (§9.4, INV-2). | P7, P9 | Ops |

---

## 2. Software and version parity

| # | Prerequisite | Why | Blocks | Owner |
|---|---|---|---|---|
| S1 | **`pg_search` (ParadeDB) installed and licence-cleared on every client.** | BM25 is built on every client (decided; §9.3) — it is not an availability gate only *because* it is guaranteed present. Bundling it triggers an AGPL source-availability obligation (see L2). | P3 (client index build) | Eng/Legal |
| S2 | **`pgvector` present and version-compatible across producer and clients.** | IVFFlat finalize runs on the client; the embedding dimension/opclass must match. | P3 | Eng |
| S3 | **PostgreSQL major-version policy recorded.** Client-build keeps the *logical* package path major-version-flexible, but extension presence + behaviour must be pinned; the *physical* prebuilt-index variant (rejected by default) would require exact major+arch parity (§9.3). | Avoids the engine/major/arch trap that ruled out a physical standby; sets the support matrix. | P3, P6 | Eng/Ops |
| S4 | **Extension version stamping** in the embedded manifest (`requires_extensions` — §6.2.2) wired to the actual installed versions on producer and clients. | Lets the verifier reject `extension_missing`/incompatible before apply (§6.3). | P6 | Eng |
| S5 | **Deterministic build toolchain** for the producer (so canonicalisation + digests are reproducible) and a pinned `minimum_client_version` scheme. | Digests are the postcondition proof (§5.4, §11.1); the version gate (§10) depends on a coherent client-version line. | P0, P6 | Eng |

---

## 3. Data prerequisites

| # | Prerequisite | Why | Blocks | Owner |
|---|---|---|---|---|
| D1 | **A real, fully ingested corpus on the producer** (the existing LEGI/jurisprudence pipeline actually run to completion) for at least the first `core` corpus. | You cannot build a baseline of a corpus you have not ingested; M3 needs real rows + chunks + embeddings + graph edges. | P3 | Data/Eng |
| D2 | **Corpus attribution present on every replicated row** (the P0 task done over real data, not just fixtures). | Per-corpus packaging, sequencing, and entitlement all key on corpus (§4.1, §5.1). | P0, P3 | Eng/Data |
| D3 | **Embedding capacity** — an OpenRouter `bge-m3` key (or equivalent fingerprint-compatible endpoint) with throughput/quota to embed a full baseline and to re-embed on a model change. | Baselines carry vectors; a re-embed forces a re-baseline (§6.1, §10). The corpus is large (analysis §"Why"). | P3, P5 | Ops/Eng |
| D4 | **PISTE/Judilibre credentials + quota for *upfront* enrichment**, sufficient to enrich `decision_zones` and archive `official_api_responses` for the covered scope (Cour de cassation `cass`+`inca`; the rest stays `zone_accurate=false`). | The replicate-upfront decision turns a lazy client-side cache into proactive server-side enrichment (§9.1); the quota is spent once for the fleet. | P9 | Ops/Data |
| D5 | **Storage/bandwidth budget for the `official_api_responses` archive** (raw body + parsed jsonb + sha256 per exchange) — measured, not estimated. | It is a *large* line item for whichever tier carries it (§9.2) and shapes both media sizing (E4) and catch-up thresholds (§9.4). | P9, P7 | Ops |
| D6 | **Measured baseline + incremental diff sizes** for each corpus (compressed and uncompressed), from real P3/P4 runs. | The catch-up policy thresholds are design *defaults to be confirmed against measured sizes* (§9.4, §15.3); they are manifest-configured but must be seeded from reality. | P7, P10 | Eng/Data |
| D7 | **A populated `jurisearch_app` test dataset** with soft references of both kinds (pin-by-`document_id`; logical `source_uid`/`version_group` + `as_of_date`). | The whole point of the re-baseline design is that this survives (§7.4, §8); it cannot be proven without real app rows to preserve. | P8 | Eng |

---

## 4. Security, trust, and cryptographic prerequisites

| # | Prerequisite | Why | Blocks | Owner |
|---|---|---|---|---|
| T1 | **Signing key material generated and custodied** (KMS/HSM or an offline root), with a `key_id`/epoch scheme and a documented **rotation cadence**. | Every artifact (network *and* media) is signed and verified on apply (§11.2). The code fixes `key_id`/epoch and a rotation-tolerant verifier; the *keys themselves* are an ops prerequisite (§15.4). | P6 | Ops/Security |
| T2 | **Trust-root distribution to clients** — the verification public key/cert must reach every client out-of-band, *including bootstrapping it onto physical media* so a first baseline can be verified before any network contact. | Media is not a weaker trust path (conception §3, INV-5); a client with no trust root cannot safely apply anything. | P3, P6 | Ops/Security |
| T3 | **Entitlement / license-token issuance** — a way to mint and install a per-corpus, per-client **license token** the client verifies locally (§11.3). Even a minimal issuer is required to test tiering. | Entitlement is an *apply precondition*, not URL hiding (§11.3); `missing_entitlement` can't be tested without real tokens. | P6, P9 | Ops/Business |
| T4 | **Authenticated, TLS-protected hosting** with **credential-based per-corpus enforcement at the edge** (object store / CDN + auth). | Serving signed packages to many external clients requires this; `serve` is loopback-only and unauthenticated (C9, §11.4). | P9 | Ops |
| T5 | **Tamper-test capability** — a procedure to corrupt an artifact/manifest in transit to confirm `signature_invalid`/`digest_mismatch` rejection with no cursor movement. | INV-9 (warn-and-reject, no partial movement) must be proven, not assumed. | P6, P10 | Eng/Security |

---

## 5. Operational prerequisites

| # | Prerequisite | Why | Blocks | Owner |
|---|---|---|---|---|
| O1 | **A scheduler/orchestrator** for the producer ingest → outbox → `package build` → sign → publish loop, with per-corpus sequence assignment. | The producer is an operated, scheduled service the project does not run today (analysis risk #8). | P9 | Ops |
| O2 | **Backups** of the producer authoritative DB, the outbox, the signing keys, and the artifact store; plus a **retention-window policy** (e.g. ~90 days / ~120 packages) driving `min_available_sequence` + `catchup_ranges`. | Catch-up correctness depends on a coherent retention window (§9.4); key loss is unrecoverable. | P9 | Ops |
| O3 | **Monitoring/alerting** on producer build/publish health, client apply outcomes (reject-code rates), retention edges, and quota consumption (D3/D4). | A distributed fleet needs operational visibility; reject codes (§6.3) are the machine-readable signal. | P9, P10 | Ops/Eng |
| O4 | **Media production runbook** — build → sign → verify → label → ship → on-client verify-and-apply, including the re-baseline scoped-reload procedure and rollback (§7.4). | Baselines/re-baselines are a physical, human-in-the-loop process with integrity + chain-of-custody requirements (analysis risk #3). | P3, P5, P9 | Ops |
| O5 | **Disaster-recovery procedure** documented as the *only* place `DROP SCHEMA … CASCADE` is sanctioned (§7.4), distinct from the operated swap path. | The operated path must never destructively drop; DR must be explicit and separate. | P5 | Ops/Eng |
| O6 | **Client service deployment/run mechanism** (`jurisearch-syncd` install, autostart, advisory-lock coordination with the CLI) on the client OS profile. | The client runs a long-running service alongside the CLI (§7.1); it must be deployable on the target client environment. | P3, P9 | Ops/Eng |

---

## 6. Compliance and legal prerequisites

| # | Prerequisite | Why | Blocks | Owner |
|---|---|---|---|---|
| L1 | **Redistribution-licensing clearance** for derived data **and** raw upstream API bodies (`official_api_responses`) before shipping any **restricted/subscription** corpus (e.g. `inpi`/RNE, licensed sources). | The operator now ships derived + byte-faithful upstream bodies to many clients; the analysis/design **defer** this but it is a hard go-live gate for restricted tiers (§1.3, §9.2). Open corpora (`core`) can ship without it. | P9 (restricted tiers only) | Legal/Business |
| L2 | **AGPL-3.0 source-availability checklist** satisfied for bundling `pg_search` and the project binaries on client machines. | The project's licensing posture; bundling triggers source-availability obligations. | P3, P9 | Legal/Eng |
| L3 | **Pseudonymisation/redaction preserved through packaging**, with the rare `delete` event (§5.2) exercised for a real redaction case. | Legal corpora carry pseudonymisation duties; a redaction must propagate as a genuine delete, not be lost in an additive stream (INV-1). | P4 | Legal/Eng |
| L4 | **Attribution / provenance** (Licence Ouverte etc.) carried with packaged corpora. | Source-licence attribution obligation for the distributed data. | P9 | Legal/Eng |

---

## 7. Minimum viable test bed

The smallest real-world setup that can validate the system end-to-end (drives the P10 acceptance gate).
Everything here is the intersection of the tables above marked as blocking P3–P8:

- **1 producer host** (E1) with a **real, small but genuine `core` baseline** ingested (D1), corpus
  attribution done (D2), embeddings present (D3).
- **2 client hosts** (E2, E3): client-A entitled to `core` only; client-B entitled to `core` + a second
  (even tiny) corpus, to prove generation independence and entitlement tiering.
- **1 signing key** (T1) with the **public root pre-loaded onto the baseline media and onto each client**
  (T2), and a **minimal license-token issuer** (T3).
- **1 USB/SSD** (E4) carrying the signed `core` baseline; a documented media apply runbook (O4).
- **A TLS endpoint** (T4) serving the remote manifest + signed incrementals, with per-corpus auth.
- **A way to take a client offline and reconnect** (E6) to exercise catch-up.
- **A populated `jurisearch_app`** on a client (D7) to prove re-baseline survival.

**The acceptance run on this bed exercises every invariant:** media baseline → reproduce producer state
on client-A (INV-3/4/6/8); network incremental with an in-place `valid_to` close + a `replace_set`
dropping a chunk (INV-1/2); out-of-order apply rejected `sequence_gap` (INV-2); offline-then-catch-up
(INV-2); `core` re-baseline with `jurisearch_app` intact and client-B's second corpus untouched
(INV-4/5/7); tampered artifact rejected (INV-9); client-A denied the second corpus `missing_entitlement`
(INV-9); out-of-date client denied `client_too_old` (INV-9).

---

## 8. Production-readiness superset (beyond the test bed)

To operate for a real fleet, add to the minimum bed:

- **Quota-backed upfront enrichment** at corpus scale (D4/D5) and the proactive enrichment scheduler (P9).
- **HSM/KMS-custodied keys with a tested rotation** (T1) and transparency-log readiness reserved in the
  manifest (§6.2.2).
- **CDN/object-store hosting** with real per-corpus credential enforcement at scale (T4) and **backups +
  retention management + monitoring** (O2/O3).
- **Redistribution licensing cleared for every restricted corpus shipped** (L1) — the gate for `inpi`
  and licensed tiers.
- **A measured catch-up policy** per corpus seeded from real baseline/diff sizes (D6, §9.4), and a
  **reference-client apply budget** (E5).
- **Media production at logistics scale** (O4) with chain-of-custody for restricted corpora.

---

## 9. Prerequisite → plan-phase blocking matrix

| Plan phase | Hard prerequisites before it can be tested in real conditions |
|---|---|
| P0 Contract + corpus attribution | D2 (over real data), S5 |
| P1 Outbox | E1, D1 |
| P2 Client topology | E2 |
| P3 Baseline vertical slice | E1, E2, E4, S1, S2, S3, D1, D2, D3, T2, O4, O6, L2 |
| P4 Incremental vertical slice | (P3 bed) + L3 (redaction case) |
| P5 Re-baseline | E3, E4, D3, O4, O5 |
| P6 Trust & gating | T1, T2, T3, T4, T5, S4 |
| P7 Catch-up | E5, E6, D5, D6 |
| P8 Reference model | D7 |
| P9 Operated producer | T1, T3, T4, D4, D5, O1, O2, O3, O4, L1(restricted), L4 |
| P10 Acceptance gate | §7 minimum viable test bed in full |

---

## 10. Open procurement / decision items (not code)

These must be *decided/acquired by a human*, and the implementation plan deliberately leaves them open
(§15 of the design):

1. **Signing scheme + key custody + rotation cadence** (T1; design §15.4) — Security/Ops.
2. **Hosting/CDN topology + authentication mechanism** (T4; design §15.5) — Ops.
3. **On-the-wire per-file encoding** (`copy-binary` / `jsonl` / `parquet`) — chosen by measurement
   (design §15.2); affects D5/D6 sizing.
4. **Final catch-up thresholds** per corpus — from measured sizes (D6; design §9.4/§15.3).
5. **View vs stable-function indirection** on hot read paths — the one measured performance trade-off
   (design §4.3/§15.1).
6. **Redistribution-licensing position** for restricted corpora and raw API bodies (L1; design §1.3) —
   Legal/Business; gates which corpora may ship at all.
7. **License-token / subscription issuance system** ownership and lifecycle (T3) — Business/Ops.

---

## 11. Bottom line

The code this plan builds assumes a world around it that the code cannot create: a producer host and at
least one genuinely separate client with matching `pgvector`/`pg_search`; a real ingested corpus with
corpus attribution and embeddings; **a signing trust root whose public half reaches clients even on
physical media**; a minimal entitlement-token issuer; TLS hosting with per-corpus auth; upfront
PISTE/Judilibre enrichment quota; and measured baseline/diff sizes to seed the catch-up policy. The
**minimum viable test bed** (one producer, two clients, one signing key, one USB baseline, one TLS
endpoint, a populated `jurisearch_app`) is enough to prove all nine design invariants end-to-end; the
**production superset** adds quota-scale enrichment, HSM-custodied rotating keys, CDN hosting with
backups/monitoring, and — the one true go-live gate for restricted corpora — **redistribution-licensing
clearance**, which the design explicitly defers but which must be resolved before any subscription corpus
leaves the building.
