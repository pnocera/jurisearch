# M7 follow-ups: rebaseline repair, retention, deferred freshness, and future hardening

Date: 2026-06-29

This note records the M7 deliverables that are operator-repair affordances, deferred features, and a
FUTURE security plan. It is documentation only — the hardening section is NOT implemented in this release.
It complements `00-macro-implementation-plan.md` (milestone M7, resolved decisions #5/#6/#7) and
`02-auto-update-server-crons.md` (Phases 5/6).

---

## 1. Manual rebaseline repair (shipped in M7)

Automatic rebaseline shipped in M3: when DILA re-issues a global baseline for a source, the
`auto-on-new-baseline` path runs a recorded rebaseline and publishes a signed `core` rebaseline package
without a human command. M7 adds an explicit operator REPAIR affordance on top of — not instead of — that:

```
jurisearch-producer rebaseline --config <path> --source <src> [--dry-run] [--skip-fetch] [--skip-enrich]
```

- `--source <src>` targets a DILA source (legi/cass/capp/inca/jade). A rebaseline re-anchors the whole
  `core` corpus and the run is locked + ingested per fetch GROUP, so the repair runs over the source's
  group (e.g. `--source cass` runs over `jurisprudence`).
- It FORCES the rebaseline branch regardless of `[baseline_refresh].mode` or whether a newer baseline is
  pending, re-anchoring `core` to each group source's currently-fetched DILA baseline and re-recording
  per-source adoption.
- It reuses the M3 machinery exactly — it does NOT invent a second rebaseline mechanism. The forced run
  drives the SAME `jurisearch_package_build::rebaseline_cycle` discard-and-rebuild path, under the SAME
  single `update-core` lock, with the SAME integrity/order/convergence checks (in ingest +
  `rebaseline_cycle`), the SAME per-source adoption marker writes (only after the signed package is
  published), and the SAME structured run record as the automatic path.
- `--dry-run` reports the per-source baselines it WOULD re-anchor + re-adopt, computed purely from the
  on-disk fetch cursors — no fetch, no lock, no DB, no mutation.
- It is an explicit repair/debug affordance, NOT the default operating mode.

Design constraint (already decided, preserved here): producer rebaseline crash-recovery is
DISCARD-AND-REBUILD with PER-SOURCE adoption. A rebaseline is always rebuilt fresh from the current locked
DB state; a staged rebaseline is never resumed. The manual command does not add a resume-a-staged path.

---

## 2. Judilibre freshness accelerator — DEFERRED (resolved decision #6)

v1 jurisprudence freshness is **daily DILA polling**: the jurisprudence timer fetches + ingests new DILA
archives once a day. A post-v1 "Judilibre freshness accelerator" — using the PISTE/Judilibre API to
surface same-day Cassation *Bulletin* decisions before the next DILA drop — is explicitly deferred and
NOT implemented in this release.

- `jurisearch-producer freshness` reports the policy and an honest
  `judilibre_accelerator: deferred-not-implemented` diagnostic, so any flag/command that references the
  accelerator surfaces a clear "not implemented in this release" message rather than pretending it exists.
- The core `update` path has NO hard dependency on Judilibre. Judilibre/PISTE is used ONLY for optional
  cass/inca zone enrichment, which HONESTLY SKIPS (`EnrichmentMode::SkippedNoCredentials`, exit class
  `published-enrich-degraded`) when no PISTE credentials are present — it never blocks ingest, embed, or
  publish. So Judilibre API unavailability degrades to DILA-only freshness and a still-successful
  (enrich-degraded) publish; it never blocks core updates.

---

## 3. Retention (shipped in M7, minimal; resolved decision #7)

Storebox retains ALL accepted official archives indefinitely (reproducibility, audit, rebuilds,
debugging). Retention tooling reclaims ONLY transient files:

```
jurisearch-producer retention --config <path> [--dry-run | --delete]
```

- Default is dry-run: it reports reclaimable files + bytes and deletes nothing. `--delete` is the explicit
  opt-in to actually reclaim.
- Reclaimable (allowlist only): fetch quarantine (`state_dir/quarantine`), ingest quarantine
  (`state_dir/ingest-quarantine`), interrupted partial-download sidecars (`archives_dir/<src>/.<name>.part`),
  and leftover atomic-write temps (`*.json.part` / `*.json.tmp`).
- NEVER touched: accepted official archives (`*.tar.gz` in the mirror), published packages and signed
  manifests under `corpora_dir`, and committed cursors/markers/run records. The delete leg has a
  defense-in-depth re-check: nothing under `corpora_dir`, and within the mirror only `.part` sidecars
  (never an accepted archive) — a quarantined corrupt `.tar.gz` under the state dir is still reclaimable.

---

## 4. FUTURE security hardening plan (NOT implemented in this release)

This section is a forward-looking plan, intentionally documentation only. None of it is code in this
release; it scopes the next security pass for the update-server and site-server roles.

### 4.1 Credential hardening
- Keep all secrets in `0600` files referenced by config (already enforced by the producer config loader);
  extend the same permission/redaction discipline to every future secret (PISTE keys, alert-hook tokens).
- Support short-lived / rotatable DB credentials and a documented rotation runbook; avoid long-lived
  superuser credentials on the operating host. `provision-db` already separates an admin identity from the
  least-privilege writer/read roles — build credential rotation on that separation.
- Consider a secrets manager / OS keyring integration as an optional backend behind the existing
  secret-file seam, so the config surface does not change.
- Never log secret values; continue routing all secret access through the redaction helpers.

### 4.2 Least-privilege service users
- Run each systemd unit as a dedicated non-login service user (e.g. `jurisearch-producer`,
  `jurisearch-site`) that owns only its state/served directories; never root.
- Apply systemd sandboxing directives to the generated units: `NoNewPrivileges=yes`, `ProtectSystem=strict`,
  `ProtectHome=yes`, `PrivateTmp=yes`, `ReadWritePaths=` limited to the served root + state dir,
  `RestrictAddressFamilies=`, `MemoryDenyWriteExecute=yes`, `CapabilityBoundingSet=` empty.
- Tighten filesystem ownership/modes on `corpora_dir`, `archives_dir`, and `state_dir` so only the service
  user writes and only the read role/site can read what it must.
- The DB writer role already holds only the privileges its DDL/DML needs; audit and narrow further before
  GA, and verify the read role cannot mutate.

### 4.3 Network hardening
- Pin/verify TLS for DILA and PISTE endpoints; fail closed on certificate problems.
- Bind the local query embedder to loopback only (already enforced for site query embeddings) and keep the
  external embedding provider restricted to the producer's public-text document embedding path.
- Restrict egress on the update-server host to the DILA mirror, the embedding provider, and (later) PISTE;
  restrict ingress to the package-serving surface only.
- Sign + checksum every published artifact (already done) and require signature verification on the site
  before apply (already done); document the trust-anchor bootstrap + rotation procedure as part of this
  plan.
- Add rate-limiting / backoff and a documented incident response path for upstream throttling or abuse.

These items are tracked here as the next security milestone; they are deliberately out of scope for the
current release, which delivers the repair/retention/deferral affordances above.
