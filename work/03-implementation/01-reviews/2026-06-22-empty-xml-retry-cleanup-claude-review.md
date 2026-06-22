I have a complete picture of the change and its surrounding flow. Writing the review to stdout now.

---

# Code Review — LEGI Freemium empty-XML resume patch

**Scope reviewed:** `git diff` on `main` (3 files) plus the surrounding ingest flow — `parse_legi_xml`/`detect_root` (`jurisearch-ingest`), `record_ingest_member_with_client` and the health/resume queries (`jurisearch-storage`), and the consuming CLI ingest loop (`jurisearch-cli/src/main.rs`). No files were edited.

**Verdict orientation:** the code is correct, atomic, and well-tested for what it handles. The one substantive issue is a *coverage* gap relative to the stated goal, not a bug. Details below.

---

## 1. Findings (ordered by severity)

### F1 — [Medium / coverage] Empty-XML detection is narrower than the failure mode it targets
`crates/jurisearch-ingest/src/legi/mod.rs:428-432`

The early return fires only when `xml.trim().is_empty()`. But the failure being remediated is the `detect_root` EOF branch at `legi/mod.rs:471-473`, which raises `"missing XML root element"` for **every** input that reaches EOF without an element start — a strictly larger set than "blank":

- byte-empty / whitespace-only — **covered** by this patch.
- prolog-only (`<?xml version="1.0" encoding="UTF-8"?>\n`) — **not covered**: `trim()` leaves the declaration, `detect_root` runs, hits EOF, returns the same `"missing XML root element"` → still `parse_error/parse_malformed_xml` → still `Failed`.
- comment-only / DOCTYPE-only — **not covered**, same reason.
- BOM-only — **not covered**: U+FEFF is not `char::is_whitespace()` in Rust, so a BOM-only member survives `trim()`, reaches `detect_root`, and fails identically.

The task states the 95 members were *sampled* as empty `versions.xml`, not exhaustively confirmed byte-empty. If any of the 95 are prolog/comment/BOM-only, the resume will leave them `Failed`, `health.failed_members > 0`, and the run won't reach the intended clean state — defeating the "resume without bumps" goal and forcing another patch cycle.

Two ways to close it (either is acceptable under GO):
- **Robust:** map the `detect_root` EOF case (`legi/mod.rs:470-474`) itself to `UnsupportedRoot { root: "EMPTY_XML" }` (or `"NO_ROOT_ELEMENT"`), so all rootless inputs are classified uniformly and the `xml.trim().is_empty()` shortcut becomes redundant.
- **Confirm-then-ship:** verify against the actual archive that all 95 failing members are byte-empty (e.g. `tar` the members and check sizes), and document that finding alongside the patch.

This is the finding to resolve or explicitly accept before relying on the resume to fully clear all 95.

### F2 — [Low] Resume-as-skip orphans the on-disk quarantine payload
`ingest_accounting.rs:376-379` (DELETE) vs `main.rs:1914-1923` / `main.rs:1945-1956` (quarantine file write)

When a member originally failed, `record_legi_member_error` wrote a payload file under `quarantine_dir/<run_id>/…`. On the same-run resume the patch deletes the DB `ingest_error` row but nothing removes the on-disk artifact. After resume the health report says zero errors while the quarantine directory still holds files for those members — a DB/disk divergence that will read as "phantom quarantine" during audits. Non-blocking; worth a note or a follow-up cleanup of the corresponding file when the error row is deleted.

### F3 — [Low] Error history is destroyed, not archived
`ingest_accounting.rs:362-380`

Resetting `error_count = 0`, nulling `last_error_*`, and `DELETE FROM ingest_error` permanently erases the record that the member ever failed in this run, including how many attempts and the original class/code/message. That is the intended "make resolved members disappear from health summaries" behavior, but it also removes forensic signal — e.g. you can no longer tell from the DB that these 95 were once `parse_malformed_xml`. Deliberate trade-off; flagging so it's a conscious decision rather than a side effect. (If retention is wanted later, gating health's `error_classes` query on member status — rather than deleting rows — would preserve history while still hiding it from current health.)

### F4 — [Low / efficiency] Two extra write statements per successful member, even with nothing to clear
`ingest_accounting.rs:362-380`

The UPDATE + DELETE run for **every** non-failed `record_ingest_member` call, which is the overwhelming majority of members in a full ingest (every inserted/skipped row), even though errors can only exist for a member that was previously attempted. Errors are only created on the `Failed` path, so a member can have error rows only if a prior attempt existed — i.e. at cleanup time its post-increment `attempt_count` (already returned at `row.get(1)`) is `>= 2`. Gating the cleanup on `attempt_count > 1` would skip both statements for all first-attempt members (the common case) with no behavior change. Pure optimization; safe to defer, but cheap to add given the table is multi-million-row at full scale.

### F5 — [Info] `"EMPTY_XML"` sentinel mixes with real roots in the unsupported-root breakdown
`legi/mod.rs:430` → `main.rs:1766-1781`

The sentinel flows into `counters.unsupported_roots["EMPTY_XML"]` and `source_entity = "EMPTY_XML"` alongside genuine XML root names. Harmless and arguably informative (it cleanly distinguishes empty members from real unsupported roots in the manifest), just noting it's a pseudo-root, not an element name.

---

## 2. Open questions / residual risks

- **Are all 95 truly byte-empty?** (F1) This is the gating question for whether the resume fully clears the run in one pass. If unconfirmed, expect a possible second pass for any prolog/BOM/comment-only members.
- **Same `run_id` resume assumed.** The patch's DELETE is only material when resume reuses the original `run_id` (`default_legi_run_id()` at `main.rs:2034` mints a fresh one each invocation; the test at `ingest_accounting.rs:164-181` uses `"run-1"` for both records and confirms `recovered.member_id == failed.member_id`). With a *new* `run_id`, the upsert inserts a fresh member row, the DELETE is a no-op, and health is clean only because it filters by `latest_run_id`. Confirm the operator runbook passes the same `--run-id` (or equivalent) on resume — otherwise the cleanup logic, while harmless, isn't what's making health clean, and stale rows persist under the old run.
- **`start_ingest_run` on an existing run_id.** Not in this diff, but reusing a run_id depends on `start_ingest_run_with_client` (`main.rs:1349`) tolerating an existing `ingest_run` row. Worth a glance if the same-run resume path is the supported one.

---

## 3. Verification notes

- **Atomicity confirmed.** `record_ingest_member_with_client`'s upsert + UPDATE + DELETE all execute on the `GenericClient` passed from `process_legi_archive_member_batch` (`main.rs:1593-1608`), which wraps each member batch in a single transaction. A mid-sequence failure rolls back cleanly; the Article `Parsed → insert → Inserted` path (`main.rs:1689-1715`) is likewise inside that transaction, so a failed insert restores the deleted error rows. Good.
- **Status source is consistent.** The cleanup keys off `input.status`, which always equals the upserted/`RETURNING` status here (the upsert sets `status = EXCLUDED.status`), so there's no divergence risk between the gate and the persisted row.
- **Schema/FK safe.** `ingest_error.member_id` is `REFERENCES ingest_member ON DELETE SET NULL` (`migrations.rs:160`); deleting error rows directly raises no FK issue.
- **Downstream mapping correct.** `UnsupportedRoot{"EMPTY_XML"}` → `IngestMemberStatus::Skipped` with `source_entity="EMPTY_XML"` and `skipped_members += 1` (`main.rs:1766-1781`); resume of a `Failed` member yields `Retry/"previous_failed"` (`ingest_accounting.rs:543`), so the empty members are re-processed and reclassified as intended.
- **Test alignment.** `tests/ingest_accounting.rs:164-181` exercises the failed→skipped same-run transition and the new assertions (`failed_members=0`, `error_classes.is_empty()`, `recovery_warnings.is_empty()`) match the cleared-state behavior; `recovery_warnings` empty additionally requires the run to be finished `completed` (`ingest_accounting.rs:636-646`). The unit test covers whitespace-only input, confirming `trim()` handling.
- **Build/lint gates** (fmt, targeted unit + storage + CLI contract tests, clippy `-D warnings`) reported as run and green; the `member_id` local-binding type resolves via the `IngestMemberRecord.member_id` field, consistent with clippy passing.

I did not independently re-run the suite (review-only); findings rely on the diff, the surrounding source as read, and the stated validation results.

---

## Recommendation summary

- **F1** is the one to act on or explicitly accept: broaden the classification to the `detect_root` EOF case, *or* confirm all 95 members are byte-empty. As code, the patch is correct; as a remediation, its completeness hinges on this.
- **F2–F4** are non-blocking improvements compatible with shipping.

No correctness blocker found in the changed code.

VERDICT: GO
