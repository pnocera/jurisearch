I have traced the full path. Here is my review.

---

## 1. Findings (ordered by severity)

No blocking findings. The change is correct, minimal, and matches the stated intent. Detailed analysis below, ordered most-significant first.

### F1 — (Confirmed correct) The discriminator `previous_run_id` is exactly the demotion key — `main.rs:1636`
The demotion the fix targets is a primary-key collision: `record_ingest_member_with_client` upserts `ON CONFLICT (run_id, archive_name, member_path) DO UPDATE SET status = EXCLUDED.status, … attempt_count = … + 1` (`ingest_accounting.rs:333-343`). A `Skipped` write demotes an existing row **only** when that row shares the current `run_id`. The resume query is keyed by `(archive_name, member_path)` and returns the most-recent row regardless of run (`ingest_accounting.rs:497-501`), with `previous_run_id` = that row's run. Therefore `resume.previous_run_id.as_deref() == Some(run_id)` is precisely the "would collide and demote" condition, and the guard suppresses the write in exactly that case. Using `previous_run_id` rather than `previous_status` is the right choice: a cross-run `inserted` member must still get a fresh `skipped` row for the new run, and it does.

### F2 — (Confirmed correct) `Skip` always carries `Some(previous_run_id)`, so the guard is well-defined — `ingest_accounting.rs:540-556`
`IngestResumeAction::Skip` is only produced from the matched-row branch, which always sets `previous_run_id: Some(...)` (line 553). The `None` case (`previous_run_id: None`) only arises with `action: Process` for a brand-new member (line 506-512), which never reaches the `Skip` arm. So `as_deref() != Some(run_id)` reduces to `prev != run_id`. Even in the unreachable `None` case the expression evaluates `true` → writes the row, which is the safe cross-run default. Defensive and correct.

### F3 — (Confirmed no regression) Counters still increment; resume backfill still fires — `main.rs:1650-1651`, `main.rs:1470`
`skipped_members` / `skipped_compatible_members` are incremented unconditionally, outside the guard. This matters beyond the JSON manifest: `full_resume_backfill = counters.skipped_compatible_members > 0` (`main.rs:1470`) is what triggers the full hierarchy backfill on a resume. That trigger reads the in-memory **counter**, never a `skipped` DB row, so suppressing the row does not weaken the backfill path. The per-pass manifest still reports the member as a skip, which is accurate for that pass.

### F4 — (Confirmed no regression) Cross-run skip and other skip paths untouched
- Cross-run compatible skip still writes a `skipped` row for the new run — covered by the existing `run-no-text` → `run-no-text-resume` test (`cli_contract.rs:2245-2278`), which still passes since its assertion is scoped to the first run's row.
- The metadata / no-text skip paths (`main.rs:1782`, `:1800`, `:1869`) and the `Retry` paths for failed/unfinished members (`main.rs:1687+`) are not in the edited branch, so failed-member resume behavior is preserved per the constraint.
- No parser/schema/code version bump — `compatibility` is unchanged (`main.rs:1622-1627`).

### F5 — (Test quality, positive) The new test is a strong negative discriminator — `cli_contract.rs:2103-2186`
The post-run assertion `inserted:1:LEGIARTI000006419320` plus `count(*) = 1` pins all three regression vectors of the old behavior simultaneously: status (`inserted`, not demoted to `skipped`), `attempt_count` (stays `1`, not bumped to `2`), and `source_entity` (preserved, not nulled — the suppressed call passed `source_entity: None`). Under the pre-fix code this row would read `skipped:2:none`, so the test genuinely fails without the fix.

## 2. Open questions / residual risks

- **Theoretical interleaving (out of scope, non-blocking):** the guard assumes the current run's row is the *most-recent* row for `(archive_name, member_path)`. If another run `S` touched the same member *after* the current run `R` inserted it and `R` is then resumed, `previous_run_id` would be `S`, the guard would treat it as cross-run, and the `skipped` write would collide with `R`'s row and demote it. This requires two runs interleaving on the same archive+member — not the sequential same-`run_id` retry this fix targets. Worth a one-line code comment, not a fix.
- **Test gating:** the test early-returns `Ok(())` when `discover_pg_config` finds no Postgres (`cli_contract.rs:2106-2108`), so it is a silent no-op in environments without PG. This matches the rest of the suite; the validation run with `--nocapture` confirms it actually executed here.
- **Manifest vs DB divergence (by design):** on a same-run resume the per-pass manifest reports `skipped_compatible_members: 1` while no `skipped` DB row exists (the row stays `inserted`). This is the intended accounting — manifest = this pass, `ingest_member` = cumulative truth. `load_ingest_health` recomputes from the DB independently and does not cross-check the manifest, so nothing errors on the divergence.

## 3. Verification notes

- Traced the edited branch through `process_legi_archive_member` → `ingest_resume_decision_with_client` → `record_ingest_member_with_client`, confirming the upsert PK and the resume query's ordering/keying drive the demotion the guard prevents.
- Confirmed `Skip` ⟹ `previous_run_id = Some`, so the comparison is total and the unreachable `None` path is safe.
- Confirmed `full_resume_backfill` depends on the counter, not the suppressed row.
- Reviewed adjacent tests (`run-cli` multi-status `failed:1,inserted:1,skipped:3` at `cli_contract.rs:1836`; cross-run `run-no-text-resume` at `:2245`) — neither asserts on the suppressed same-run write, so both remain green.
- Relied on the already-run gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, and the two named `cli_contract` tests. I did not re-run them (review-only; no files edited).

Recommendation (optional, non-blocking): add a brief comment at `main.rs:1636` explaining that `previous_run_id == run_id` is the PK-collision/demotion guard, to protect the invariant against the interleaving edge in the residual-risk note.

VERDICT: GO
