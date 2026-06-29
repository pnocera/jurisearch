# Codex Re-Review: M2-A DILA Fetch Integrity

## Findings

No BLOCKER/WARN/NIT findings.

## Verification Notes

- The prior blocker is fixed in source. `read_full_targz` scopes the `entries()` iterator, drains each tar member fully, then recovers the underlying `GzDecoder` with `archive.into_inner()` and drains it to EOF with `io::copy` (`crates/jurisearch-fetch/src/integrity.rs:119`, `crates/jurisearch-fetch/src/integrity.rs:127`, `crates/jurisearch-fetch/src/integrity.rs:157`). That final read forces flate2 to validate the gzip CRC-32/ISIZE trailer before `verify_targz` can return an `IntegrityReport` (`crates/jurisearch-fetch/src/integrity.rs:97`).
- Cursor advancement still happens only after integrity succeeds and the `.part` file is promoted into the mirror (`crates/jurisearch-fetch/src/engine.rs:206`, `crates/jurisearch-fetch/src/engine.rs:209`, `crates/jurisearch-fetch/src/engine.rs:212`). Integrity failures go to quarantine and do not call `cursor.record` (`crates/jurisearch-fetch/src/engine.rs:219`, `crates/jurisearch-fetch/src/engine.rs:224`, `crates/jurisearch-fetch/src/engine.rs:227`), so I do not see a remaining path where the footer-corrupt archive is promoted or recorded.
- The new regression fixture is targeted at the prior gap: it starts from a valid archive and flips only the trailing 8-byte gzip trailer, leaving the deflate body and tar members intact (`crates/jurisearch-fetch/tests/support/mod.rs:123`, `crates/jurisearch-fetch/tests/support/mod.rs:129`). The unit test asserts `verify_targz` rejects that fixture as `IntegrityError::Corrupt` (`crates/jurisearch-fetch/tests/fetch.rs:147`, `crates/jurisearch-fetch/tests/fetch.rs:157`), and the end-to-end test asserts the same bytes are quarantined, absent from the mirror, and absent from the cursor (`crates/jurisearch-fetch/tests/fetch.rs:375`, `crates/jurisearch-fetch/tests/fetch.rs:399`, `crates/jurisearch-fetch/tests/fetch.rs:408`, `crates/jurisearch-fetch/tests/fetch.rs:418`).
- Re-confirmed the prior positives: `git diff main -- crates/jurisearch-ingest` is empty, and the only `change_seq` hits in `jurisearch-fetch` are documentation/test comments. Selection is by parsed archive filename/cursor membership (`crates/jurisearch-fetch/src/listing.rs:113`, `crates/jurisearch-fetch/src/engine.rs:124`, `crates/jurisearch-fetch/src/cursor.rs:119`).
- I did not run `cargo test` because the review instruction allowed writing only the review file.

VERDICT: GO
