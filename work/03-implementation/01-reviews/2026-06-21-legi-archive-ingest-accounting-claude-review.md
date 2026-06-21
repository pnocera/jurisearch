# Claude Review: LEGI Archive Ingest Accounting Flow

Verdict: GO

## Findings

No blocking (correctness / data-loss / plan-contract / test-gap) issues found. The
items below are non-blocking robustness observations recorded for visibility.

- **Low — `crates/jurisearch-cli/src/main.rs:661,686-692`.** An oversized member (or
  any other `ArchiveReadError` from `for_each_xml_member_until`, e.g.
  `MemberTooLarge` when a member exceeds `--max-member-bytes`) is turned into a single
  fatal error that aborts the whole run. Unlike a parse/validation failure — which is
  recorded as a `failed` member, optionally quarantined, and ingest continues — the
  oversized member is never recorded in `ingest_member` (the error is raised inside
  `read_bounded` *before* the visit closure runs), so it cannot be traced or
  quarantined and it stalls every member after it in that archive plus all later
  archives. Impact: bounded — it aborts loudly (run marked `failed`, error JSON,
  non-zero exit), no silent data loss, and the operator can raise `--max-member-bytes`
  to get past it; real LEGI `ARTICLE` members are kilobytes, far under the 16 MiB
  default. Required fix: none for this slice; if pursued later, treat
  `MemberTooLarge` as a recordable per-member `failed`/quarantine case rather than a
  whole-run abort so the streaming guard degrades gracefully.

- **Low — `crates/jurisearch-cli/src/main.rs:672-675`.** A storage error on any single
  member's `insert_legi_documents` / `record_ingest_member` / status update aborts the
  entire run (sets `fatal_error`, `Stop`), where a per-document constraint violation
  would also kill all remaining members. This is a defensible "systemic errors are
  fatal" policy and resume recovers cleanly (the member stays `parsed`/unfinished →
  retried, and inserts are idempotent), so it is not a data-loss bug. Required fix:
  none; optionally document the policy or distinguish data-specific from systemic
  storage errors in a later slice.

## Suggestions

- `crates/jurisearch-cli/src/main.rs:1153` — `open_or_create_index` is byte-for-byte
  identical to `open_index` (both `PgConfig::discover()` + `start_durable`, and
  `start_durable` already `create_dir_all`s the index). The fresh-vs-existing
  distinction is fully carried by `require_configured_index_dir` vs
  `require_existing_index_dir`, so `open_or_create_index` is redundant — call
  `open_index` from the legi-archives path and drop the duplicate.
- CLI test coverage is solid for the fresh-index ingest, quarantine, accounting rows,
  and compatible replay skip, but the CLI-level mapping of `BlockedIncompatible`
  (record `failed` + `compatibility_mismatch` error) and `Failed`/`parsed` → retry is
  only exercised at the storage layer. Consider one CLI contract test that re-runs an
  archive after bumping a compatibility field (or with mutated member bytes) to assert
  the run is marked `failed` and the member records a `validation_error` /
  `compatibility_mismatch`, and one that re-runs over a previously `failed` member to
  assert it is retried.
- The command exits `0` (success) while emitting `run_status: "failed"` when only
  per-member failures occurred (covered and asserted by the test). This is intentional
  and discoverable via `failed_members` / `run_status` in the JSON, but the exit-0 /
  status-failed split is worth a one-line note in the command's docs so agents do not
  read exit 0 as "all members ingested".
- Unsupported roots are recorded by overloading `ingest_member.source_entity` with the
  root tag (e.g. `SECTION_TA`). This satisfies the per-root persistence follow-up
  (plan line 350) at member granularity and is also surfaced in the JSON
  `unsupported_roots` map, but the semantic overload of `source_entity` is mildly
  surprising; a dedicated column or a note would make the intent clearer for W2
  reporting.

## Verification Notes

- Confirmed the working tree equals commit `e8ace4b` (only untracked `.codegraph/`);
  `git diff --check` is clean.
- Read the full `ingest legi-archives` path in `crates/jurisearch-cli/src/main.rs`
  (`ingest_legi_archives_payload`, `process_legi_archive_member`, `record_legi_member`,
  `record_legi_member_error`, `maybe_quarantine_payload`, `sanitize_quarantine_component`,
  `legi_parse_error_class`).
- **Archive order / streaming / limits:** `plan_from_dir`/`plan_from_paths`
  (planner.rs) select the latest baseline and sort deltas ascending; the CLI processes
  `[baseline, deltas…]` in that order. `for_each_xml_member_until` (reader.rs) streams
  one `.xml` member at a time with `read_bounded` enforcing `max_member_bytes`.
  `--limit-members` is a global cap across baseline+deltas, checked both before and
  after each member and propagated via `break 'archives`. Zero `--limit-members` and
  zero `--max-member-bytes` are rejected with `bad_input` before any index is opened.
- **Fresh-index behavior:** `require_configured_index_dir` only requires the dir to be
  configured (not pre-initialized) and `start_durable` `create_dir_all`s + initdb, so a
  brand-new `--index-dir` is created and ingested into (exercised by the test's
  tempdir index).
- **Resume / replay correctness (key acceptance):** `ingest_resume_decision`
  (ingest_accounting.rs) keys on `(archive_name, member_path)`, returns
  `Skip` for compatible `inserted`/`skipped`, `Retry` for `failed`/`parsed`, and
  `BlockedIncompatible` on any `parser_version`/`schema_version`/`code_version`/
  `source_payload_hash` mismatch — backed by the `UNIQUE (run_id, archive_name,
  member_path)` constraint and `ingest_member_resume_idx`. Re-processing is duplicate-
  safe: `insert_legi_documents` (projection.rs) is a single transaction with
  `ON CONFLICT DO UPDATE` upserts, and the canonical IDs it keys on are deterministic
  (`document_id = legi:{id}@{valid_from}`, `chunk_id = chunk:{document_id}:0`,
  `publisher_edge_id` derived from document_id+index+tag+target+text). This satisfies
  "interrupted ingest can resume without duplicate canonical records" and "parser/
  schema/code/source changes cannot silently preserve stale bad rows".
- **Run/member/error accounting:** `start_ingest_run` (idempotent upsert on run_id) →
  per-member `record_ingest_member` (`parsed`→`inserted`, `skipped`, or `failed`) →
  `record_ingest_error` (transactional, bumps member error_count, links member_id) →
  `finish_ingest_run` with `Completed` only when `failed_members == 0 && fatal_error
  is None`, else `Failed`. Failed/blocked members never insert canonical rows (parse/
  block precede insert), so failures leave no stale data.
- **Quarantine:** failed/blocked payloads are written under
  `<quarantine_dir>/<run>/<archive>__<member>` with both path components sanitized to
  `[A-Za-z0-9._-]`; the JSON error `context` records `quarantined`. Quarantine failure
  surfaces as a storage error (fail-loud).
- **safe_mode:** persisted as `ingest_run.safe_mode` metadata and echoed in JSON; it
  has no behavioral effect yet, which the plan update accurately discloses as deferred.
- **JSON/exit discipline:** success via `write_json` (pretty JSON + newline to stdout);
  errors via `emit_error` (JSON to stdout, `process::exit`). `bad_input`→2,
  `index_unavailable`→3, `dependency_unavailable`→4. stderr stays empty (tests assert
  this; Postgres logs go to the index log file).
- **Plan accuracy:** the Phase 1.0 status edit truthfully describes the delivered slice
  (stream baseline/deltas, run/member/error accounting, compatible resume, unsupported-
  root counts, safe_mode-as-metadata, optional quarantine, four named tests) and the
  "Remaining" list correctly carries forward replay-snapshot status, safe-mode
  behavior, out-of-status query gating, and broader gate thresholds.
- **Tests run locally:** `git diff --check` (clean) and
  `cargo test -p jurisearch-cli --test cli_contract ingest_legi_archives` — both tests
  passed in 1.80s, confirming the Postgres-gated
  `ingest_legi_archives_records_accounting_and_quarantines_failures` actually executed
  the full ingest, accounting-row, quarantine, and compatible-replay-skip assertions
  (not the no-DB early return). I relied on the reviewer-provided workspace test,
  clippy, and full contract-suite runs for the remainder.
