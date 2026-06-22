I have completed a thorough review of the uncommitted diff. Below is my assessment following the required gate structure.

---

# Claude Review: Status Replay Snapshot Cache Optimization

## 1. Findings (ordered by severity)

### F1 — Medium — The Phase 1 `replay_snapshot` gate does not distinguish a fresh snapshot from a stale cached one
`phase1_gate_payload` passes the replay gate on `replay_snapshot_status == "available"` alone, ignoring `replay_snapshot_source`:

- `crates/jurisearch-cli/src/main.rs:3733` — `if ingest_available && ingest_health["replay_snapshot_status"] == "available" { "pass" }`

A default (cheap) `status` returns a *cached* signature that may predate uncaptured mutations, yet it satisfies the gate identically to a `--deep` refresh. The storage test deliberately demonstrates this staleness window: after `UPDATE documents … title`, the cached read still returns the old signature (`crates/jurisearch-storage/tests/ingest_accounting.rs:281-284`). So a release decision driven off a default `status` can pass the replay gate on stale evidence.

This is by-design per your stated intent and it **fails safe on the cold path** (no cache → `source="missing"`, `status="missing"` → gate pending). It is gate-correct in the normal CLI flow because every supported mutation path refreshes (verified — see §4). The residual exposure is narrow (out-of-band mutation), but the gate currently emits no provenance/age signal a human or CI could use to tell "verified now" from "trusting a cache." See recommendation R1.

### F2 — Low — `ingest legi-archives` refreshes only on `Completed`, so a partial run leaves the cache stale
`crates/jurisearch-cli/src/main.rs:1572` gates the refresh on `run_status == IngestRunStatus::Completed`. A run that inserts rows but ends non-completed leaves `index_manifest['replay_snapshot']` stale relative to the newly-inserted documents/chunks, while a subsequent default `status` would still report `available`. In practice the `latest_completed_ingest_run` and `failed_members` checks block the Phase 1 claim in that scenario, so the *overall* gate stays safe — but the replay check alone is stale. Worth an inline comment documenting that the replay gate's freshness leans on the sibling checks for partial runs.

### F3 — Low — No test pins the refresh-idempotency property that the whole design depends on
Correctness of repeated refreshes rests on excluding the cache row from its own signature (`WHERE key <> 'replay_snapshot'`, `crates/jurisearch-storage/src/ingest_accounting.rs:888`). The only test near this is the post-`UPDATE` `assert_ne!` at `tests/ingest_accounting.rs:285-290`, which changes a `documents` row and therefore would pass *even if the exclusion were deleted* (the doc hash changes regardless). There is no test asserting that a second refresh with unchanged data yields the same signature, nor that `manifests.count` stays `1` after a store. A regression removing the exclusion would slip through. See R2.

### F4 — Low — A corrupt/incompatible cached blob degrades all ingest health to `unavailable`, not just the snapshot to `missing`
`load_cached_replay_snapshot` propagates `serde` errors (`crates/jurisearch-storage/src/ingest_accounting.rs:933-937`). That `StorageError` bubbles out of `load_ingest_health_with_replay_snapshot_mode` into the `Err` branch of `status_index_and_ingest_health` (`main.rs:3886`), marking the entire index `unavailable`. It is gate-safe (degrades to pending), but a future `schema_version` bump or a hand-edited row makes `status` look broken rather than simply signalling a refreshable/missing snapshot. The code already wraps with `schema_version` and tolerates the unwrapped shape — treating a deserialize failure as `None` (→ `missing`) would keep default `status` informative and let `--deep` self-repair. See R3.

### F5 — Info — Refresh cost is O(full corpus) at every write boundary
The refresh recomputes the full ordered hash (~589s, incl. ~12.9M publisher edges) regardless of delta size. Every `legi-archives` completion, `backfill-legi-hierarchy`, and `embed-chunks` now carries a fixed multi-minute tail even for a tiny incremental ingest. Intended per "refresh at command boundaries," but flag it: incremental ingests are no longer cheap end-to-end. Not a correctness issue.

## 2. Open questions / residual risks
- **Operational contract for canonical-table writes.** The cached-gate freshness guarantee holds only if the three CLI commands are the *sole* mutation paths. Out-of-band SQL (migrations, manual repairs, any future maintenance command) won't refresh and would leave the default-status replay gate reading stale `available`. Please confirm that contract, and that any new mutating command must call `refresh_replay_snapshot`.
- **Concurrency (future).** Read runs under `REPEATABLE READ` then stores outside that snapshot — correct for a single-writer CLI. If concurrent ingest is ever introduced, two refreshes race on the `replay_snapshot` upsert (last-writer-wins) and one could capture another writer's in-flight rows. Acceptable today; note for later.

## 3. Recommendations (compatible, non-blocking)
- **R1 (strongest):** Make provenance visible at the gate. Include `replay_snapshot_source` (and ideally the cache `updated_at`) in the `replay_snapshot` gate-check message, and consider requiring `source == "refreshed"` (or a `--deep` run) for `claim_allowed`. This keeps default `status` cheap while preventing a stale GO — it directly addresses "without weakening the Phase 1 gate" as defense-in-depth.
- **R2:** Add an idempotency test: refresh → `S1`; refresh again with no data change → assert `== S1` and `manifests.count == 1`. This pins the exclusion clause that F3 leaves unguarded.
- **R3:** Have `load_cached_replay_snapshot` return `None` on a deserialize failure (degrade to `missing`/pending) rather than erroring the whole ingest-health load.
- **R4 (housekeeping):** A stray `work/03-implementation/01-reviews/2026-06-22-status-replay-cache-claude-review.md.tmp` is in the working tree — exclude it from the commit.

## 4. Verification notes
- **Idempotency mechanism confirmed:** both the signature computation and the `manifests` component carry `WHERE key <> 'replay_snapshot'` (`ingest_accounting.rs:882-889`), so storing the cache does not perturb the next computed signature. The exclusion is correct; only the *test* coverage of it is missing (F3).
- **Mutation-path coverage confirmed:** enumerated `IngestSubcommand` — `PlanArchives` (dry-run, no mutation), `LegiArchives`, `EmbedChunks`, `BackfillLegiHierarchy`. All three mutating paths call `refresh_replay_snapshot` (`main.rs:1573`, `2079`, `2193`). No mutating CLI command is missing a refresh.
- **No manifest-key interference:** `latest_manifest` reads `ingest_run`; `embedding_manifest` reads `key='embedding'`; `dense.rs:184` writes `key='embedding'` *before* the embed-chunks refresh, so the snapshot captures it. No code enumerates all `index_manifest` keys, so the new `replay_snapshot` row is inert elsewhere.
- **Backward compatibility confirmed:** `Command::Status` → `Status(StatusArgs)` keeps bare `jurisearch status` working (`deep` defaults false); JSONL `status` with null/absent args and without `deep` defaults to cached via `SessionStatusArgs::default()` + `#[serde(default)]` (`main.rs:1142-1153`). `replay_snapshot_source` is emitted through `serde_json::to_value(report)` in `ingest_health_payload`, matching the new assertions; `StatusResponse.ingest_health` is an opaque `{type:object}` so no response-contract field was required; `StatusRequest.deep` added with `default:false` and asserted in the contract test.
- **Evidence adequacy:** the summary shows the intended 3.00s (missing) → 589.34s (refreshed) → 3.01s (cached, identical signature `430af44…`) progression with deep counts — sufficient to substantiate the optimization and signature stability.
- **Not independently executed:** per scope I made no edits and did not re-run the suite; I relied on the stated `cargo test`/`clippy` runs. The static logic review is consistent with those passing.

**Summary:** The implementation is correct, backward-compatible, and gate-safe in the supported flow — it degrades to `missing`/`pending` rather than producing a false GO on a cold cache, and every canonical-table-mutating CLI command refreshes. The one substantive concern (F1) is a by-design tradeoff you explicitly chose, and it fails safe; the remaining items harden an already-safe design rather than fixing incorrectness. R1 is worth doing before this gate is trusted for an actual Phase 1 claim, but it is not required for this change to land.

VERDICT: GO
