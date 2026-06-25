# Code Review: SOLID/DRY #21 Shared Archive Member-Batching Loop

## Findings

No BLOCKER/WARN/NIT findings.

## Review Notes

- The extracted `read_archive_members_batched` loop in `crates/jurisearch-cli/src/ingest/run.rs` preserves the old LEGI/JURI branch order: pre-member limit guard, `visited += 1`, flush-before-overflow, push and byte accumulation, size/byte flush, post-member limit stop, guarded tail flush, and flush-error precedence over read-error handling.
- `visited_members` remains loop-local accounting. Both `LegiArchiveIngestCounters::merge_committed` and `JuriArchiveIngestCounters::merge_committed` intentionally omit it, and the callers write the returned count back on clean, limit-stop, flush-error, and read-error exits before setting `fatal_error`.
- The flush-drain contract is still satisfied by the real flush functions. `flush_legi_archive_member_batch` and `flush_juri_archive_member_batch` clear `pending_members` and reset `pending_member_bytes` after successful commits, and both call sites pass the helper-owned `&mut pending_members` / `&mut pending_member_bytes` directly through the closures.
- Error semantics match the pre-commit loops: mid-loop flush failures short-circuit through `ArchiveVisit::Stop`, skip the tail flush, preserve the visited count, and surface as storage errors; read failures skip the tail flush and preserve the original LEGI/JURI read-error message text.
- The global `--limit-members` behavior is preserved because each archive seeds the helper from `counters.visited_members`, stops only when the returned report reaches the cap, and continues to later archives when an archive exhausts before the cap.
- The new unit fixtures use `.xml` members in tar.gz archives, so they are yielded by `for_each_xml_member_until`. The assertions cover tail flush, count-triggered full-batch flush plus tail, seeded global limit accounting, and flush-error visited-count propagation.

## Verification

- Reviewed `git diff 202ed57~1 202ed57 -- crates/jurisearch-cli/src/ingest/run.rs crates/jurisearch-cli/src/ingest/legi.rs crates/jurisearch-cli/src/ingest/juri.rs crates/jurisearch-cli/src/ingest.rs`.
- Compared the new helper against the pre-commit LEGI and JURI loops from `202ed57~1`.
- Ran `cargo test -p jurisearch-cli ingest::run::tests`: 4 passed.
- Ran `git diff --check 202ed57~1 202ed57`: no whitespace errors.
- I did not rerun the managed-Postgres contract tests referenced in the brief.

VERDICT: GO
