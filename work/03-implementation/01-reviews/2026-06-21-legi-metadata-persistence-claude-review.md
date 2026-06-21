I've completed a thorough review of the live tree (which exactly matches commit `e3e9ab8` — `git diff HEAD` is empty). Here is my review.

# Claude Review - LEGI Metadata Persistence

Verdict: GO

The implementation is correct, idempotent, and well-tested. Migration v4 is additive and gated by `schema_migrations` version tracking; the projection upsert is keyed on stable, parse-derived fields; every `date`-cast column is validated upstream; and the new table has no effect on existing query paths. No blocking issues found.

## Why this is safe (key checks)

- **Date-cast safety (the main risk) is covered.** The projection sends `valid_from`/`valid_to` through `$6::text::date` / `$7::text::date` (`projection.rs:177`+). All values entering these casts are validated to a real calendar date (`YYYY-MM-DD`, `days_in_month`) before the struct is built — `normalize_required_date` for `TEXTE_VERSION`/`SECTION_TA` `valid_from` (`legi/mod.rs:932,981`), `normalize_end_date` for `valid_to`, and `validate_date("LIEN@debut", …)` for the `TEXTELR` hint that becomes `valid_from` (`legi/mod.rs:833`). So a malformed date is rejected at parse time (per-member quarantine), not at INSERT time (which would fatally abort the archive run). The `::text::` step also correctly pins the param type as text. Sentinel ends (`2999-01-01/12-31`) normalize to NULL with `valid_to_raw` preserved.
- **Migration/idempotency.** `CREATE TABLE/INDEX IF NOT EXISTS` + per-version `schema_migrations` gating (`migrations.rs:262-274`) means v4 runs once on an existing v3 DB and is re-run-safe. `CHECK (root_kind IN ('TEXTE_VERSION','SECTION_TA','TEXTELR'))` matches the only literals `from_root` emits. `validate_migration_list` keeps versions contiguous.
- **Upsert idempotency** on `ON CONFLICT (metadata_key) DO UPDATE` is directly proven by the projection test (re-inserting `TextVersion` keeps `count(*) = 3`).
- **Metadata-key stability.** Keys derive only from stable parse outputs (`legi:{kind}:{uid}@{date}`, payload-hash fallback when uid/date absent — `projection.rs:395`), so re-ingesting the same archive and baseline→delta reprocessing both upsert the same rows ("latest wins"). `TEXTE_VERSION` (uid+`valid_from`) and `SECTION_TA` (section_id+`valid_from`) are uniquely keyed.
- **Resume compatibility.** Bumping `CANONICAL_SCHEMA_VERSION` to `canonical_record:v2` (`main.rs:52`) forces a `schema_version` mismatch (`compatibility_mismatches`, `ingest_accounting.rs:751`) for any pre-bump member, so old `Skipped` metadata rows are surfaced as `BlockedIncompatible` rather than silently `Skip`-reused without the new projection — exactly the stated intent.
- **Accounting & ordering.** Metadata is committed before `record_legi_member(Skipped)`; a crash in that window leaves an orphan row that the idempotent key re-upserts on resume, so no corruption. `persisted_metadata_members += report.metadata_roots` (1 root/call) is asserted end-to-end in the contract test (`= 2`).
- **No query-readiness side effects.** Retrieval reads `documents`/`chunks`; `legi_metadata_roots` is write-only this slice. `parent_source_uid` is intentionally a plain column (no FK), since parent texts aren't ingested as documents.

## Non-blocking suggestions

1. **`TEXTELR` key is weaker than the others (`projection.rs:411-440`, `from_root` TEXTELR arm).** When `text_id` is present the key omits the payload hash, so two distinct `TEXTELR` structural versions of the same text that resolve to the same earliest `LIEN@debut` hint (or both lack one) collide and overwrite. `source_payload_hash` records which won, so it's last-writer-wins, not corruption — but the next slice ("assemble hierarchy across article versions") consumes this table, so consider whether `TEXTELR` rows need per-version uniqueness (e.g. include the payload hash, or a stronger version anchor) before that work lands.
2. **Two-pass recovery on an existing index.** After the version bump, the *first* resume marks pre-bump members `Failed`/`compatibility_mismatch`; only the *second* resume (which sees the matching-v2 `Failed` row → `Retry`) actually back-fills the metadata. Harmless for the from-scratch ingest that's normal at this stage, but worth a one-line operator note if any populated index exists.
3. **`persisted_metadata_members` is currently tautological** — always equal to `parsed_metadata_members` (each call persists exactly one root). Fine as an explicit "persisted" signal; just noting it can't diverge today.
4. **Double match + `unreachable!` (`main.rs:858-875`).** The outer arm already guarantees only the three metadata variants; re-matching `&parsed` with an `unreachable!` Article/Unsupported arm is correct but slightly redundant — destructuring once would drop the dead arm. Style only.
5. **CLI-path coverage gap for `TEXTELR`.** The contract fixture exercises only `TEXTE_VERSION`+`SECTION_TA` end-to-end; `TEXTELR` persistence is covered by the projection unit test but not through the CLI ingest branch. Low risk (the branch mirrors the others), optional to add.

## Verification

The working tree is identical to the reviewed commit, so the locally-run suite applies directly. Recommended to confirm:

```
cargo test -p jurisearch-storage --test legi_metadata_projection
cargo test -p jurisearch-storage --test schema_migrations
cargo test -p jurisearch-cli ingest_legi_archives_records_accounting_and_quarantines_failures --test cli_contract
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
```

I inspected the migration runner (`run_migrations`/`validate_migration_list`), the upsert and key logic (`insert_legi_metadata_roots`, `from_root`, `legi_metadata_key`), the resume gate (`ingest_resume_decision`/`compatibility_mismatches`), and all upstream date validators; the tests assert stable keys, upsert idempotency (count stays 3), parent/hierarchy JSON fidelity, the v4 migration entry, and the `persisted_metadata_members` accounting.
