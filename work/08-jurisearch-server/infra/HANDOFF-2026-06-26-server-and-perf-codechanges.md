# HANDOFF — jurisearch PG18 server (bear) + performance code changes

Date: 2026-06-26
Purpose: resume cold after a context clear. Covers what's done (bear provisioned as the jurisearch
PostgreSQL 18 server + corpus loaded), the in-progress reindex, and the REMAINING work (code changes to
the jurisearch CLI, shaped by a server-vs-client constraint).

---

## 0. How to reach things

- **bear** (Proxmox host, on tailnet `tail0cb6c3`): `sshpass -p '20Sense20' ssh -o StrictHostKeyChecking=accept-new root@100.102.96.111`
  (password `20Sense20`; tailnet-only, user accepts no securing). Web UI: `https://bear.tail0cb6c3.ts.net:8006`.
- **CT 110 `jurisearch`** (the DB server, Debian 13 LXC on bear, **48 cores / 192 GB**, static `192.168.0.110` on vmbr1, bridge-only):
  run things via `pct exec 110 -- …` from bear. psql as the postgres superuser (peer auth):
  `pct exec 110 -- runuser -u postgres -- psql -d jurisearch -P pager=off -f -` (feed SQL on stdin to avoid quoting hell).
- **Long ops**: launch detached inside the relevant host via `systemd-run --unit=NAME -p StandardOutput=append:/root/NAME.log …`, poll the log for a `SENTINEL:` line (survives SSH drops).
- **fedora** = this workstation (where the dev/embedded jurisearch + the source corpus live). Tailscale IP `100.75.46.116`.

## 1. Stack on CT 110 (verified working)
- **PostgreSQL 18.4** (PGDG; Debian trixie ships only PG17) + **pgvector 0.8.3** + **pg_search 0.24.1**
  (built from the `pnocera/paradedb` fork at `/home/pierre/Work/paradedb`, via `cargo-pgrx 0.18.1`, Rust 1.96.0).
- `shared_preload_libraries = 'pg_search'`. Cluster `18/main`, data on a 1 TB mountpoint at `/var/lib/postgresql`.
- DB **`jurisearch` = 163 GB**, locale **en_US.UTF-8 / UTF8** (had to `locale-gen` it; collation version refreshed 2.43→2.41).
- Schema: `documents` 2.9 M (`legi` articles 1.7 M + jurisprudence `jade`/`inca`/`cass`/`capp`),
  `chunks` 4.7 M (`contextualized_body` BM25-indexed, **French tokenizer**: ascii-folding + French stemmer + French stopwords),
  `chunk_embeddings` 4.64 M (`embedding vector(1024)`, bge-m3), `zone_units`/`zone_unit_embeddings`, `graph_edges` 18.2 M, `official_api_responses`, citation tables.
- Indexes: 232 btree + 2 ivfflat (`vector_l2_ops`, operator `<->`) + 2 bm25 + 1 gin. bge-m3 is normalized → L2 ordering ≡ cosine.

## 2. What's DONE (all script→Codex-GO→run; scripts + reviews in `infra/bear-storage/` and `infra/proxmox-upgrade/`)
1. **Proxmox upgrade** `pve9-upgrade.sh`: bear **8.4.1 → 9.2.3** (Debian 12→13), kernel 7.0.12-1-pve.
2. **Disk merge** `bear-disk-merge.sh`: reclaimed empty `/home` into `/` → one **~3.5 TB `local`** pool. (Earlier: removed dead `quasar` cluster node + 8 orphaned guest disks, restored quorum.)
3. **`create-jurisearch-lxc.sh`**: CT 110 (Debian 13, was 16c/64 GB → now **48c/192 GB**, 1 TB PG mountpoint, bridge-only).
4. **`switch-to-pg18.sh`**: PG17→PG18 (PGDG) + pgvector 0.8.x. (Note: upstream pgvector 0.8.0 can't compile on PG18; 0.8.x shares the IVFFlat on-disk format, so 0.8.3 reads 0.8.0 indexes — no reindex needed for the version diff.)
5. **`build-pg-search.sh`** (param `PGVER=18 EXT_FEATURES=pg18,deferred_wal`): built pg_search 0.24.1 for PG18.
6. **`load-corpus.sh`**: physically copied the 165 GB PG18 PGDATA (rsync'd fedora→bear staging) over `18/main`; lessons baked in (CT needs `rsync` + the corpus's libc locale generated before PG starts).
7. **Retrieval tested** — both legs return relevant French jurisprudence:
   - **BM25** (fast, 87 ms with the right shape): "trouble anormal de voisinage" → Cass. Ch. civile 3; "responsabilité du fait des produits défectueux" → Cass. Ch. civile 1 + Conseil d'État.
   - **Dense** (pgvector): more-like-this returns the same chamber/doctrine (coherent), but was ~13 s — see §3.
   - **CRITICAL BM25 query shape**: push the top-N into the BM25 scan BEFORE joining, else it runs away (a naive `JOIN + ORDER BY paradedb.score` over common terms hit 500 s+). Use:
     `SELECT d.* FROM (SELECT chunk_id, document_id, paradedb.score(chunk_id) AS score FROM chunks WHERE contextualized_body @@@ 'query' ORDER BY paradedb.score(chunk_id) DESC LIMIT k) hits JOIN documents d ON d.document_id=hits.document_id ORDER BY hits.score DESC;`

## 3. IN PROGRESS — the reindex we're waiting on (this is the thing to check first after clearing)
- **`tune-pg18.sh`** (Codex r2 = GO) is running in CT 110 as unit **`tune-pg18`**, log **`/root/tune-pg18.log`** (in the CT), sentinel **`SENTINEL: TUNE-DONE`**. Background poll task id was `b0va4k6p4`.
- It applies the SERVER PG profile (shared_buffers 48 GB, work_mem 128 MB persistent / 16 GB SET LOCAL for the build, autovacuum_work_mem 1 GB cap, max_parallel_workers 48, etc.) → restarts PG → **atomically rebuilds `chunk_embeddings_embedding_ivfflat_idx` at lists≈2154** (was `lists=32`) → ANALYZE → verifies a timed dense query.
- **RESULT (DONE, 2026-06-26 ~15:23, `TUNE-DONE rc=0`)**: `chunk_embeddings` = 4,701,354 rows → IVFFlat
  rebuilt at **lists=2168** (was 32), atomically (~10 min build), `shared_buffers`=48 GB applied, ANALYZE done.
  **Dense query: ~13–19 s → 491 ms** at probes=64 (high recall); EXPLAIN shows
  `Index Scan using chunk_embeddings_embedding_ivfflat_idx`. ~30× faster. Root cause was `lists=32` for
  4.7 M vectors (a finalize-gap from the source corpus), NOT the migration. **bear perf work is complete.**
- (Re-check if ever needed: `pct exec 110 -- tail -40 /root/tune-pg18.log`.)

## 4. REMAINING WORK — propagate the perf fixes into the jurisearch CLI (the main next phase)

> **STATUS 2026-06-26 (DONE):** all three fixes implemented, Codex-reviewed to GO, and committed on
> `main`. Fix #1+#2 = `ee68ba1` (auto-`lists` + coupled `probes`; codex GO r2; reviews
> `reviews/2026-06-26-ivfflat-autolists-probes-codex-review*.md`). Fix #3 = `011f3ff` (conservative +
> env-tunable managed-PG profile; codex GO r3 vs PG 18.4 source;
> `reviews/2026-06-26-managed-pg-perf-profile-codex-review*.md`). The `infra/` folder is already
> committed (`25abbdb`). Remaining = only the non-blocking bear-side housekeeping in §5 (reclaim the
> 165 GB staging, add a vzdump backup job for CT 110). The original plan is kept below for reference.

### THE CONSTRAINT (shapes every default)
**Server (bear)** = dedicated, powerful → aggressive config OK (that's what `tune-pg18.sh` does, bear-only).
**Clients** = weaker (≈ a workstation like fedora) and **share RAM with local LLMs + embedding services** → the jurisearch **CODE defaults must be conservative + tunable**; the aggressive server values are an explicit *override*, never the default. **IVFFlat is the right index for this** (small memory footprint; HNSW would be wrong — graph wants RAM). Clients **build indexes locally** after applying packages (work/08 design §9.3) within an apply-time/resource budget (§9.4), so build-time `maintenance_work_mem`/parallelism must also be conservative/tunable.

### Fix #1 — auto-scale IVFFlat `lists` (root cause of lists=32)  [do first]
- `crates/jurisearch-cli/src/args.rs:730` (EmbedChunks) and `:778` (EmbedZoneUnits): `--index-lists default_value_t = 32` → **default 0 (= auto)**, help "0 = auto, scaled to corpus size (recommended)".
- `crates/jurisearch-storage/src/dense.rs`:
  - `finalize_dense_rebuild` (≈L93-201): already computes the valid `embeddings` row count. Add `pub fn recommended_ivfflat_lists(rows: i64) -> u32` (pgvector rule: `rows/1000` if ≤1e6 else `sqrt(rows)`, clamp ≥1). Compute `effective_lists = if spec.index_lists==0 { recommended_ivfflat_lists(embeddings) } else { spec.index_lists }`; use it in the `CREATE INDEX … WITH (lists=…)`, the `index_manifest` json (≈L163-175), and `DenseRebuildReport.index_lists` (≈L199).
  - `validate_dense_spec` (≈L234): currently rejects `index_lists==0` → **allow 0 (= auto)**.
- `crates/jurisearch-storage/src/zone_units.rs` `finalize_zone_dense_rebuild` (≈L435): apply the same auto-lists to the zone-unit index.
- Tests: add unit tests for `recommended_ivfflat_lists` boundaries; update the existing `dense_spec_validation_rejects_invalid_inputs` test (0 is now valid).

### Fix #2 — couple query `probes` to `lists` (MUST ship with #1, else recall regresses)  [do with #1]
- `crates/jurisearch-storage/src/retrieval/types.rs:177-178`: `effective_probes(options) = options.ivfflat_probes.unwrap_or(4)` — fixed 4. With a corpus-sized `lists` (~2154), probes=4 probes ~0.2% of clusters = low recall.
- Plan: at build, store a recommended `default_probes` (≈ `sqrt(lists)`, clamp e.g. [1,4096]) in the `index_manifest` `vector_index` json (dense.rs + zone_units.rs). At query, when `--probes` is not given, default to that stored value (read `index_manifest` once in the retrieval path). Emitter is `crates/jurisearch-storage/src/retrieval/hybrid.rs:19-20` (`SET ivfflat.probes = {effective_probes}`); zone path is `zone_retrieval.rs:217-220`.
- Keep the explicit `--probes` override (`query_support.rs:35`, range 1..=4096) untouched.

### Fix #3 — managed-Postgres perf profile (conservative + tunable) — for the EMBEDDED/client PG
- `crates/jurisearch-storage/src/runtime.rs` `apply_runtime_profile` (≈L489) currently only sets durability (`synchronous_commit`, `max_wal_size`). Add a **conservative, env-tunable** perf profile: small default `shared_buffers` (e.g. 256–512 MB, NOT 25% of RAM), low `work_mem`, bounded parallelism, modest `maintenance_work_mem` for client index builds — explicitly leaving RAM for co-located LLMs/embedding. Expose knobs (env vars like `JURISEARCH_PG_SHARED_BUFFERS`, etc.). The bear-style aggressive values are an operator override only. (Lower priority than #1+#2; bear's standalone PG is already tuned by `tune-pg18.sh`.)

### Workflow for the code changes (from project memory / CLAUDE norms)
- **Commit on `main` directly** (no feature branches). One Codex-reviewed commit per coherent change (do #1+#2 together since coupled; #3 separate).
- `cargo fmt` + `cargo check` (and relevant tests) BEFORE each Codex review.
- Codex review via the `codex-review` skill: give it ONLY the phase/scope name and let it discover the diff; apply findings across ALL severities (BLOCKER/WARN/NIT); commit per GO; if FIXES_REQUIRED, re-review (r2/r3) to a clean GO.
- Commit trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- Codegraph MCP (`codegraph_*`) is available for structural lookups; prefer it over grep for "what calls X".

## 5. Open housekeeping (non-blocking)
- **165 GB staging** on bear `/root/jurisearch-staging` + the read-only `mp1` bind-mount on CT 110 — a second on-bear copy. Keep as a local backup, or reclaim: `pct set 110 --delete mp1 && pct reboot 110 && rm -rf /root/jurisearch-staging`.
- **No vzdump backup job** for CT 110 → `storebox` (Hetzner CIFS, mounted on bear) yet. Recommended. (storebox had only ~1-yr-old backups; the failures were the old quorum break, now fixed.)
- The **`infra/` folder is untracked in git** — offer to commit it (scripts + reviews + README + this handoff) to `main` alongside the work/08 design docs.
- Two existing guests left intact and not needed: `nats1` (CT 101) and `postgresql` (CT 107, has an empty `juridia` app-schema scaffold). User said destroy "if required" — not required.
- The design docs being realized: `work/08-jurisearch-server/2026-06-26-*-{analysis,design,conception,implementation-plan,prerequisites}.md` (already committed). The new architecture: central producer (bear) builds + distributes signed per-corpus incremental packages; read-only clients apply them. The `juridia` schema on CT 107 resembles the planned writable `jurisearch_app` layer.

## 6. RESUME CHECKLIST
1. ✅ Reindex done (§3) — dense queries now 491 ms (was ~13–19 s). bear perf work complete.
2. ✅ Fix #1 + #2 (auto-`lists` + coupled `probes`) — committed `ee68ba1` (codex GO r2).
3. ✅ Fix #3 (conservative + tunable managed-PG profile, `runtime.rs`) — committed `011f3ff` (codex GO r3).
4. ✅ `infra/` folder already committed (`25abbdb`); this handoff lives inside it.
5. ☐ (optional, non-blocking, §5) bear-side housekeeping: reclaim the 165 GB staging + `mp1` bind-mount,
   and add a vzdump backup job for CT 110 → `storebox`. Operator decisions; not code.
