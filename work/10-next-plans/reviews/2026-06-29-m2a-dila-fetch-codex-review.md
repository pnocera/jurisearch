# Codex Review: M2-A DILA Fetch

## Findings

### BLOCKER: Integrity check can accept archives without consuming the full gzip stream

`verify_targz` calls `read_full_targz` and then accepts the archive once tar entry iteration finishes ([crates/jurisearch-fetch/src/integrity.rs:97](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/integrity.rs:97), [crates/jurisearch-fetch/src/integrity.rs:109](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/integrity.rs:109)). Inside `read_full_targz`, the code drains each returned member, increments `members`, and returns immediately after the `tar::Archive::entries()` iterator ends ([crates/jurisearch-fetch/src/integrity.rs:122](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/integrity.rs:122), [crates/jurisearch-fetch/src/integrity.rs:124](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/integrity.rs:124), [crates/jurisearch-fetch/src/integrity.rs:144](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/integrity.rs:144)).

That does not prove the underlying gzip reader reached EOF or validated the gzip footer. The tar iterator stops when it sees an all-zero header block; it does not require the gzip stream to be read to completion. Because `flate2::read::GzDecoder` validates the footer only when it is read past the compressed body, a file with a complete readable tar prefix but a missing/corrupt gzip trailer can pass this gate. `fetch_one` then promotes the `.part` file and records the cursor immediately after this report ([crates/jurisearch-fetch/src/engine.rs:206](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/engine.rs:206), [crates/jurisearch-fetch/src/engine.rs:209](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/engine.rs:209), [crates/jurisearch-fetch/src/engine.rs:212](/home/pierre/Work/jurisearch-worktrees/m2a-dila-fetch/crates/jurisearch-fetch/src/engine.rs:212)), so this violates the "full gunzip+tar drain" integrity requirement and can advance the cursor for a truncated archive.

Actionable fix: after the tar entry loop, explicitly consume the underlying gzip decoder to EOF and map any footer/trailer error to `IntegrityError::Corrupt`. One shape is to scope/drop the `entries` iterator, call `archive.into_inner()` to recover the `GzDecoder<File>`, then `read_to_end` or `io::copy` it into a sink. Add a regression fixture that removes/corrupts only the gzip footer while leaving all tar members readable; it should be quarantined and must not advance the cursor.

## Notes

- Confirmed `git diff main -- crates/jurisearch-ingest` is empty; the ingest crate is unchanged.
- I found no `change_seq` coupling in `jurisearch-fetch`; selection is by parsed archive filename/timestamp and per-file cursor state.
- The implemented fixture file currently contains 16 `#[test]` functions, not the 17 reported in the brief. The important no-op and quarantine retry tests do assert zero second-run downloads and no cursor record for the corrupt archive.

VERDICT: FIXES_REQUIRED
