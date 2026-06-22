# Claude Review Request: Status Replay Snapshot Cache Optimization

Repo: `/home/pierre/Work/jurisearch`

Review scope:

- Current uncommitted diff only.
- Do not edit files.
- Focus on correctness, release-gate safety, replay freshness semantics, CLI/session contract compatibility, and test/evidence adequacy.

User intent and constraints:

- Make lengthy status/replay operations faster without weakening the Phase 1 gate.
- Default `jurisearch status` must be cheap and read cached replay signatures.
- `jurisearch status --deep` must explicitly recompute and cache full replay signatures.
- Successful heavy write phases should refresh the cache at command boundaries:
  - LEGI archive ingestion completion
  - LEGI hierarchy backfill
  - dense embedding finalization
- Keep the existing Claude review gate workflow: findings first, then recommendations, then final verdict.

Changed files:

- `crates/jurisearch-storage/src/ingest_accounting.rs`
- `crates/jurisearch-storage/tests/ingest_accounting.rs`
- `crates/jurisearch-cli/src/main.rs`
- `crates/jurisearch-cli/tests/cli_contract.rs`
- `crates/jurisearch-core/src/schema.rs`
- `work/03-implementation/IMPLEMENTATION_PLAN.md`
- `work/03-implementation/02-evidence/2026-06-22-status-cache-optimization-summary.md`
- Live evidence JSON/time files under `work/03-implementation/02-evidence/2026-06-22-status-cache-*`

Implementation summary:

- Added `ReplaySnapshotMode::{Cached, Refresh}`.
- Default `load_ingest_health` uses cached replay snapshot only.
- `load_ingest_health_with_replay_snapshot_mode(..., Refresh)` recomputes the full replay snapshot and stores it in `index_manifest['replay_snapshot']`.
- Missing cache reports `replay_snapshot_source="missing"` and `replay_snapshot_status="missing"`, so the Phase 1 replay gate remains pending until a cached/deep snapshot exists.
- Cached snapshots are excluded from the manifest component hash by filtering `index_manifest WHERE key <> 'replay_snapshot'`.
- CLI `status --deep` and JSONL status `{ "deep": true }` trigger refresh mode.
- Successful LEGI archive ingestion, hierarchy backfill, and `ingest embed-chunks` dense finalization call `refresh_replay_snapshot` and include a compact `replay_snapshot_cache` summary in command output.

Validation already run:

- `cargo fmt --all`
- `cargo test -p jurisearch-storage ingest_accounting_records_members_errors_and_resume_decisions`
- `cargo test -p jurisearch-cli status_reports_ingest_health_from_existing_index`
- `cargo test -p jurisearch-cli ingest_embed_chunks_uses_endpoint_pool_and_finalizes_dense_index`
- `cargo test -p jurisearch-cli ingest_backfill_legi_hierarchy_updates_full_index`
- `cargo test -p jurisearch-cli`
- `cargo test -p jurisearch-storage`
- `cargo clippy --workspace --all-targets -- -D warnings`

Full-index evidence:

- Index: `/home/pierre/Work/jurisearch/index/phase1-freemium-20250713`
- `status` before cache: 3.00s, `replay_snapshot_source=missing`, replay gate pending.
- `status --deep`: 589.34s, `replay_snapshot_source=refreshed`, replay gate pass.
- `status` after cache: 3.01s, `replay_snapshot_source=cached`, same signature as deep refresh.
- Deep counts: 1,736,165 documents; 1,852,745 chunks; 12,949,444 publisher edges; 1,852,745 embeddings; 2 manifests.
- Signature: `430af44453662d6107a46e7baedde246`.

Required output structure:

1. Findings, ordered by severity, with file/line references.
2. Open questions or residual risks.
3. Recommendations or compatible suggestions.
4. Verification notes.
5. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
