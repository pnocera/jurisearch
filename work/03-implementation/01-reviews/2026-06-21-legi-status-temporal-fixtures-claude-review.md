Review complete. The live tree matches the commit, the targeted test passes, and I've validated the contract against the parser source and the real LEGI corpus + DTDs.

# Claude Review - LEGI Status Temporal Fixtures

Verdict: GO

The new unit test faithfully pins the intended LEGI status/temporal normalization contract, and no parser behavior changed. All findings below are non-blocking.

## What I verified

- **No hidden parser change.** The commit diff (`git show c83a27d`) touches only the `mod tests` block (single hunk `@@ -1541,6 +1541,34 @@`) plus `IMPLEMENTATION_PLAN.md`. Critically, `normalize_end_date` already matched **both** sentinels in the parent baseline `b00aa8d` (`b00aa8d:.../legi/mod.rs:1284 → matches!(value, "2999-01-01" | "2999-12-31")`). So the `2999-12-31` handling is pre-existing behavior the test now documents, not a new behavior introduced here.
- **Assertions mirror the parser exactly** (`crates/jurisearch-ingest/src/legi/mod.rs`):
  - `source_status` = opaque `ETAT` passthrough (`mod.rs:860,890`)
  - `valid_to` = `normalize_end_date(...)` → `None` for sentinels, else `Some(raw)` (`mod.rs:868,894,1282-1289`)
  - `valid_to_raw` = raw `DATE_FIN` preserved regardless of normalization (`mod.rs:867,895`)
  - `canonical_version` = `legi_article:v1:nature=…:etat=…:type=…` (`mod.rs:903-906`)
  The four cases exercise both branches of the sentinel normalizer plus two finite end-dates — a tight pin.
- **Fixture realism against the real corpus** (`/home/pierre/Apps/juridocs/opendata/LEGI`, ~1.75M articles sampled):
  - `VIGUEUR`, `MODIFIE`, `ABROGE`, `ABROGE_DIFF` are all attested real `ETAT` values.
  - `2999-01-01` is the dominant open-ended sentinel (ubiquitous, incl. `fin=` attrs).
  - `2999-12-31` is a **documented Légifrance-family** sentinel — `@example` in `DTD/kali/kali_article.dtd:68` and `DTD/jorf/jorf_section_ta.dtd:298,326` — so the defensive handling is justified, though `rg "2999-12-31"` returns **zero hits across all of `opendata/`** (it does not appear in actual LEGI article data).
- **Live tree matches commit** (no uncommitted edits to `mod.rs`/plan) and the targeted test passes:
  ```
  test legi::tests::preserves_article_status_and_temporal_variants ... ok
  ```

## Non-blocking suggestions

1. **`ABROGE_DIFF` + `2999-12-31` is a semantically unrealistic pairing** (`mod.rs:1550`). A deferred repeal (`ABROGE_DIFF`, "abrogation différée") implies a *finite future* `DATE_FIN`; pairing it with an open-ended sentinel never occurs in real LEGI data. The test is mechanically valid (the parser treats status and date independently), but consider pinning `ABROGE_DIFF` with a finite future date and exercising `2999-12-31` separately on a `VIGUEUR` case — that would mirror reality and still cover both normalizer branches.
2. **The `("VIGUEUR", "2999-01-01", None)` case yields two no-op `.replace` calls** (`mod.rs:1547`), making the produced XML identical to the base fixture. It still earns its place (it is the *only* test asserting the article `canonical_version` string — `parses_official_article_to_canonical_document` does not), but worth noting the redundancy.
3. **Plan wording precision** (`work/03-implementation/IMPLEMENTATION_PLAN.md:535`): "known LEGI open-ended sentinels (`2999-01-01` and `2999-12-31`)" — `2999-12-31` is documented in the JORF/KALI DTDs and not present in the LEGI article corpus; "Légifrance-family sentinel" would be more accurate.
4. **Untested-but-real statuses** (`TRANSFERE`, `MODIFIE_MORT_NE`, `PERIME`, `ANNULE` all appear in the sample). Acceptable for this focused slice since `ETAT` is an opaque passthrough that follows the identical code path, and the plan explicitly defers full-corpus status/temporal expansion. Adding one (e.g. `TRANSFERE`) would broaden coverage cheaply.
5. Optionally assert `valid_from`/`document_id` are unchanged across cases to guard against an accidental `DATE_DEBUT` disturbance (very minor — the replaces target only `ETAT`/`DATE_FIN`, and `validate()` at `mod.rs:914` already guards date shape).

## Verification commands inspected / recommended

```bash
cargo test -p jurisearch-ingest preserves_article_status_and_temporal_variants   # passes (live tree)
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
git show c83a27d -- crates/jurisearch-ingest/src/legi/mod.rs   # confirm only mod tests hunk changed
git show b00aa8d:crates/jurisearch-ingest/src/legi/mod.rs | grep -n '2999-12-31'  # sentinel predates commit
```
