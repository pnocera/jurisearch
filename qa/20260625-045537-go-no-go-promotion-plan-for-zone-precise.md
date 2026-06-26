# Go/No-Go: Zone-Precise Retrieval Promotion

## Verdict

**GO.** Based on the evidence you list and the current source, promoting the clone by directory swap is the right move.

The go criteria are met:

- Coverage is meaningful: 72,911 zoned Cassation decisions is well above the prior 25k / 5% threshold.
- The measured zone benchmark passed: `all_meet_proposed_floor=true`, dense enabled, correct `bge-m3:1024:normalize:true` fingerprint, and strong recall for `dispositif`, `motivations`, and `moyens`.
- Zone-unit dense coverage is complete: 283,333 units, 283,333 embeddings, `units_pending=0`, manifest fingerprint present, IVFFlat finalized.
- Legislation enrichment is complete enough for its non-blocking cache role: no pending rows, with honest `ok` / `not_found` / `upstream_error` accounting.

I do not see a correctness reason to delay promotion.

## Swap Safety

The `mv`-based swap is sound **provided all Postgres instances and consumers are stopped**.

Source check:

- `ManagedPostgres::start_durable` derives `pg_root`, `data_dir`, `socket_dir`, and `log_path` from the runtime `--index-dir`.
- It calls `write_runtime_conf` before `pg_ctl`.
- `write_runtime_conf` rewrites `pg/data/jurisearch.conf` with the current socket directory and port.
- `pg_ctl` receives `-D <current index dir>/pg/data`.
- The client connection string is loopback host/port/database, not a stored data-dir path.

I also checked the clone files non-mutatingly: the three directories exist and no `postmaster.pid` is present. The clone `jurisearch.conf` currently contains the old clone socket path, but that is expected and not a blocker because the first managed open after the rename overwrites it before start.

The only operational flaw in the proposed two-`mv` sequence is that the live path is absent briefly between commands. Since you say no consumer is running, that is fine. If you want a slightly more defensive form:

```bash
ROOT=/mnt/models/jurisearch-index
TS=$(date -u +%Y%m%d-%H%M%S)

test ! -e "$ROOT/phase2-full-juridic/pg/data/postmaster.pid"
test ! -e "$ROOT/phase2-full-juridic.zone-rollout-20260624/pg/data/postmaster.pid"

mv "$ROOT/phase2-full-juridic" "$ROOT/phase2-full-juridic.pre-zone-$TS"
mv "$ROOT/phase2-full-juridic.zone-rollout-20260624" "$ROOT/phase2-full-juridic"
```

Because both paths are under `/mnt/models/jurisearch-index`, the renames should be same-filesystem. If you want to assert that explicitly:

```bash
stat -c '%d %n' \
  "$ROOT/phase2-full-juridic" \
  "$ROOT/phase2-full-juridic.zone-rollout-20260624"
```

The device number should match.

## Extra Pre-Swap Checks

Before the `mv`, I would add only these quick guards:

```bash
ROOT=/mnt/models/jurisearch-index

test -d "$ROOT/phase2-full-juridic/pg/data"
test -d "$ROOT/phase2-full-juridic.zone-rollout-20260624/pg/data"
test -d "$ROOT/phase2-full-juridic.backup-20260624/pg/data"

test ! -e "$ROOT/phase2-full-juridic/pg/data/postmaster.pid"
test ! -e "$ROOT/phase2-full-juridic.zone-rollout-20260624/pg/data/postmaster.pid"
test ! -e "$ROOT/phase2-full-juridic.backup-20260624/pg/data/postmaster.pid"
```

If available, also run a process-level check:

```bash
pgrep -af 'postgres.*phase2-full-juridic' || true
```

No matches should refer to prod or clone.

## Post-Swap Verification

After the swap, verify these before declaring success:

1. Status on the new prod:

```bash
jurisearch --index-dir "$ROOT/phase2-full-juridic" status
```

Require:

- schema version 17,
- base corpus still query-ready,
- `zone_retrieval.zone_units.total = 283333`,
- `zone_retrieval.embeddings.total = 283333`,
- `zone_retrieval.embeddings.units_pending = 0`,
- `zone_retrieval.embedding_manifest.embedding_fingerprint = "bge-m3:1024:normalize:true"`.

2. Ordinary search still works:

```bash
jurisearch --index-dir "$ROOT/phase2-full-juridic" \
  search "responsabilité du fait des produits défectueux" --mode hybrid --top-k 5
```

3. Zone search works:

```bash
jurisearch --index-dir "$ROOT/phase2-full-juridic" \
  search "responsabilité du fait des produits défectueux" --zone motivations --mode hybrid --top-k 5
```

Require non-empty results and `official_zone_retrieval` / zone scope in the response.

4. One official part fetch if you have a known zoned document id from the benchmark or smoke:

```bash
jurisearch --index-dir "$ROOT/phase2-full-juridic" \
  fetch <known-zoned-document-id> --part motivations
```

Require `zone_accurate=true` / Judilibre provenance for that decision.

5. Optional but useful: confirm legislation enrichment came along:

```bash
jurisearch --index-dir "$ROOT/phase2-full-juridic" status
```

If status exposes the legislation-citation block in your current build, require `pending=0` and the expected 17,220 unique resolutions.

## Rollback

Your rollback plan is correct:

```bash
ROOT=/mnt/models/jurisearch-index

# stop the managed PG first if the verification opened it
mv "$ROOT/phase2-full-juridic" "$ROOT/phase2-full-juridic.failed-$TS"
mv "$ROOT/phase2-full-juridic.pre-zone-$TS" "$ROOT/phase2-full-juridic"
```

Then run `status` on the restored prod. Keep both the immutable backup and the pre-zone rollback for now.

## Do Not Delete Yet

Do not delete `phase2-full-juridic.pre-zone-$TS` until:

- at least one full consumer cycle has run against the promoted path,
- `status` and zone smoke have been captured from the promoted path,
- no stale process is still pointed at the old directory,
- you have a fresh post-promotion backup or have consciously accepted the existing immutable backup plus pre-zone rollback.

Bottom line: **promote now, with the pre-swap process guard and the post-swap ordinary-search + zone-search + known-fetch smoke.**
