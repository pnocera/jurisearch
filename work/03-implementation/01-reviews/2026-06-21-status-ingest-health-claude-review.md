# Review: Expose ingest health in status (f5b1c08)

Verdict: GO

Scope: commit `f5b1c08` ("Expose ingest health in status") reviewed against
`work/03-implementation/IMPLEMENTATION_PLAN.md` Phase 1.0 "Ingest Operational
Accounting and Replay". Reviewed files: `crates/jurisearch-cli/src/main.rs`,
`crates/jurisearch-cli/tests/cli_contract.rs`,
`work/03-implementation/IMPLEMENTATION_PLAN.md`. Storage API
(`jurisearch-storage::ingest_accounting::load_ingest_health` /
`IngestHealthReport`) inspected for behavior, not modified.

## Blocking findings

None. All review-focus items pass:

- **Status still succeeds without an index and keeps JSON-only stdout.**
  `Command::Status => write_json(&status_payload(index_dir.as_deref()))`
  (`main.rs:278`) always returns `Ok(())`. With no configured index,
  `status_index_and_ingest_health` (`main.rs:782-792`) returns the
  `not_configured` index block + `pending_ingest_health()` and never writes to
  stderr. Independent compile + the pre-run `status_returns_json_without_index`
  contract test confirm exit 0 / empty stderr.
- **Initialized + configured index reports live health.** When
  `JURISEARCH_INDEX_DIR`/`--index-dir` resolves and `pg/data/PG_VERSION` exists
  (`main.rs:794`), it opens storage via `open_index` and calls
  `load_ingest_health` (`main.rs:806-807`). The full report (latest run id/
  status, member counts, error classes, projection/embedding coverage,
  recovery warnings) is serialized into `ingest_health` and `query_ready` is
  derived from `coverage_complete(projection) && coverage_complete(embedding)`
  (`main.rs:809-815`, `coverage_complete` = `total > 0 && covered == total`,
  `main.rs:895-897`). Matches plan acceptance "report latest ingest health,
  coverage, and recovery warnings."
- **Invalid/unavailable index stays structured JSON and does not affect
  retrieval.** Missing `PG_VERSION` → `not_initialized`; `open_index` failure →
  `unavailable` + structured `error`; `load_ingest_health` failure →
  `unavailable` + `error` with `pending_ingest_health()` (`main.rs:794-855`).
  All paths still exit 0 with JSON only. Retrieval error behavior is untouched:
  the refactor only extracted `configured_index_dir` (`main.rs:648-652`) and
  `require_existing_index_dir` (`main.rs:632-646`) is byte-for-byte
  behavior-preserving; `retrieval_command_without_index_is_json_and_uses_exit_code_3`
  still asserts exit 3 / `index_unavailable`.
- **JSONL session status remains compatible.** `dispatch_session_request`
  calls `status_payload(None)` (`main.rs:737`). With env unset this yields the
  same `not_configured` payload as before; `session_jsonl_preserves_order_*`
  still passes. `None` is consistent with the session path generally — session
  search/fetch resolve the index from request args / env, never the global
  `--index-dir` flag.
- **Implementation-plan status is accurate.** Lines 495-496 claim status
  reports live ingest health, latest completed run, recovery warnings, and
  coverage-derived query readiness — all true. It does **not** claim real
  ingest-flow wiring, quarantine output, or replay-snapshot computation;
  `replay_snapshot_status` is still surfaced honestly as the storage `"pending"`
  placeholder (`ingest_accounting.rs:530`), and those items remain in the
  "Remaining" bullet. The remaining bullet's "block/mark query access … outside
  the status report" correctly reflects that status *marks* `query_ready` but
  does not yet *block* retrieval.

## Non-blocking suggestions

1. **Test hermeticity regression (recommended).**
   `status_returns_json_without_index` (`cli_contract.rs:47`) and
   `status_reports_embedding_budget_env_overrides` (`cli_contract.rs:74`) do not
   call `.env_remove("JURISEARCH_INDEX_DIR")`, unlike the retrieval/bad-input
   tests (`cli_contract.rs:205,223,241,259`). Before this change status ignored
   the index entirely, so the env var was harmless; now a developer/CI
   environment with `JURISEARCH_INDEX_DIR` set would make these tests open an
   index (slow, or fail the `query_ready == false` assertion). Add
   `.env_remove("JURISEARCH_INDEX_DIR")` to both for hermeticity.

2. **Latent overwrite defeats the serialization fallback** (`main.rs:866-882`).
   The `unwrap_or_else` fallback sets `"state": "unavailable"`, but the
   following `map.insert("state", "available")` overwrites it unconditionally —
   a serialization failure would be reported as `available` carrying a
   "failed to serialize ingest health" warning. Currently unreachable
   (`IngestHealthReport`'s only `f64`s come from `percentage`, which returns
   `None` when `total == 0`, so no NaN/Inf), but the overwrite contradicts the
   fallback's intent. Build the `available` payload only on the serialize-Ok
   branch, or skip the `state` insert when the fallback fired.

3. **Session status ignores a per-request `index_dir`.**
   `dispatch_session_request` discards `request.args` for `status` and passes
   `None`, while `session_search_payload` honors `args.index_dir`
   (`main.rs:444`). Behavior is backward-compatible (env fallback still works),
   but the asymmetry is surprising. Consider accepting `args.index_dir` for
   session status, or documenting status as env-only.

4. **`latest_completed_run` semantics vs. name** (`main.rs:861-865`). It returns
   the latest run id only when *that* run's status is `"completed"`; if the most
   recent run is `running`/`failed` but an earlier run completed, the field is
   `null` despite a completed run existing. The name reads as "most recent
   completed run." Consider renaming (e.g. `latest_run_completed`) or querying
   the most recent completed run if the latter is intended.

5. **Payload shape / state naming.** `pending_ingest_health()` omits the report
   fields (`latest_run_id`, totals, `replay_snapshot_status`, …) that the
   `available` payload includes, so consumers must branch on `state` — fine, but
   worth keeping in mind for the W2 reporting contract. Separately, an
   initialized-but-empty index reports index `state: "ready"` with
   `query_ready: false`; the boolean is authoritative, but "ready" for an empty
   index could read as misleading vs. e.g. "initialized" (low priority).

6. **Coverage gaps in the new contract tests.** The only managed-Postgres
   status test (`status_reports_ingest_health_from_existing_index`) exercises
   the all-green happy path. Worth adding: (a) a *hermetic* `not_initialized`
   test (set `JURISEARCH_INDEX_DIR` to an empty temp dir — no Postgres needed —
   asserting `index.state == "not_initialized"` and
   `ingest_health.state == "pending"`); (b) a `query_ready == false` case (a
   document without a chunk, or a chunk without an embedding) to prove the gate
   actually gates; and strengthen `status_returns_json_without_index` to assert
   `index.state == "not_configured"` and `ingest_health.state == "pending"` so
   the contract is pinned.

7. **Theoretical panic on non-UTF-8 index path** (`"path": index_dir` in the
   `json!` blocks, e.g. `main.rs:799,825`). `json!` serializes a `PathBuf` via
   `to_value(...).unwrap()`, which errors on non-UTF-8 paths. Extreme edge on
   Linux; very low priority, noted for completeness.

## Verification notes

- Inspected the full diff (`git show f5b1c08`) plus
  `crates/jurisearch-storage/src/ingest_accounting.rs` (the `load_ingest_health`
  query semantics: member/error counts scoped to the latest run, projection &
  embedding coverage corpus-wide — consistent with the status message and plan
  wording).
- Independently ran:
  - `git diff f5b1c08^ f5b1c08 --check` → clean (no whitespace errors).
  - `cargo check -p jurisearch-cli --all-targets` → clean.
  - `cargo clippy -p jurisearch-cli --all-targets -- -D warnings` → clean.
- Relied on the pre-stated verification for the heavier suite
  (`cargo test --workspace`, `cargo test -p jurisearch-cli --test cli_contract`,
  the targeted `status_reports_ingest_health_from_existing_index`,
  `cargo clippy --workspace -D warnings`). The managed-Postgres tests skip
  gracefully via `discover_pg_config` when PG extensions are unavailable
  (`cli_contract.rs:614-638`), so a green run in an environment without
  `pg_search`/`vector` does not exercise the new health test — confirm it ran
  against managed Postgres (or with `JURISEARCH_REQUIRE_PG_EXTENSIONS` set) to
  fully validate the happy path.
