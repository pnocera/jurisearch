I have everything needed. The R1 blockers are both resolved, verified against real data, and the build/tests are green. Here is the review.

---

# Claude Review R2 - LEGI Metadata Roots

Verdict: GO

Scope: commits `e2c63a9` ("Parse LEGI metadata roots") + `ee736b9` ("Address LEGI metadata review fixes") against the live tree. Working tree matches the commits (only untracked `.codegraph/` and this review's `-r2.md`). Focus: the R1 blocker, accounting compatibility, query-readiness side effects, and test coverage.

## R1 blocking issues — both fully fixed

- **Major #1 (NATURE hard-required → in-force empty-`NATURE` regression to Failed/quarantine): RESOLVED.** `crates/jurisearch-ingest/src/legi/mod.rs:90` now types `nature: Option<String>`; `into_text_version` uses `optional_non_empty(self.nature)` (`mod.rs:929`) and the canonical fallback `nature.as_deref().unwrap_or("absent")` (`mod.rs:959-962`), mirroring the article `etat` pattern. The two cited real files (`LEGITEXT000049371154`, `LEGITEXT000024235014`) now parse `Ok(TextVersion)` and route to the `Skipped` / `parsed_metadata_roots` arm (`crates/jurisearch-cli/src/main.rs:848-871`) instead of the `Err → Failed` arm — so they no longer flip `run_status` to `failed` or get quarantined. I confirmed the real file `…/JORFTEXT000000337687/…/LEGITEXT000049371154.xml` carries `<NATURE/>` + `<ETAT>VIGUEUR</ETAT>` and the test fixture mirrors it byte-for-shape.
- **Major #2 (test gap): RESOLVED.** New `parses_text_version_with_empty_nature_as_absent` (`mod.rs:1766-1801`) pins the empty-`NATURE` case: `nature == None`, `canonical_version == "legi_text_version:v1:nature=absent"`, `valid_from`/`valid_to` correct. The happy-path test was also strengthened with `text.nature.as_deref() == Some("CODE")` (`mod.rs:1755`).

## Correctness / compatibility checks (all clean)

- **Accounting unaffected.** `source_uid()` (`mod.rs:66`) and `date_anchor()` (`mod.rs:76`) for `TextVersion` derive from `text_id`/`valid_from`, independent of `nature` — so `source_entity` and `date_anchor` on the Skipped record stay correct for empty-`NATURE` members. `parsed_metadata_members`/`parsed_metadata_roots` increment as before; no contribution to `failed_members`.
- **Query-readiness: no side effects.** Metadata roots are not yet projected to documents (only the `Article` arm inserts). `TextVersion`/`ParsedTextVersion` is consumed solely at `main.rs:849`; no storage/projection path reads `.nature`. `CanonicalDocument.source_nature` is already `Option<String>`, so the Option is forward-compatible when projection lands. No golden fixtures/snapshots pin the old `String` shape.
- **Helper robustness.** `optional_non_empty` (`mod.rs:1095`) reduces `None`, `<NATURE/>`, `<NATURE></NATURE>`, and whitespace-only to `None` uniformly. `TEXTELR` already used the same (`mod.rs:1026`), so the three roots are consistent.
- **Other TEXTE_VERSION required fields stay required** (`id`, `title`, `status`, `date_debut`, `date_fin`) — matching the R1 corpus scan that found those empty-counts at 0. No over-loosening.

## Verification commands inspected/run

- `cargo test -p jurisearch-ingest --lib legi` → 21 passed (incl. the new regression test).
- `cargo test -p jurisearch-cli --test cli_contract` → 18 passed, 2 ignored (live-endpoint), incl. `ingest_legi_archives_records_accounting_and_quarantines_failures`.
- `cargo check --workspace --tests` → clean (rules out any remaining `nature: String` consumer).
- `find … LEGITEXT000049371154.xml` + `grep <NATURE…>` → confirmed real `<NATURE/>` + `VIGUEUR`.

## Non-blocking observations (not gating)

- **Regression coverage is parser-level only.** The new test asserts the parse succeeds; there is no end-to-end CLI-contract fixture asserting an empty-`NATURE` `TEXTE_VERSION` lands in `parsed_metadata_roots` (Skipped) rather than quarantined. Routing is deterministic for any `Ok(TextVersion)`, so this is adequate — but a one-member CLI fixture would lock the accounting outcome against future routing changes.
- **Tolerance policy now inconsistent across the parser (pre-existing, out of scope).** The article path still hard-requires `META_COMMUN/NATURE` (`mod.rs:859`) and `META_ARTICLE/NUM` (`mod.rs:861`); R1 measured ~32 real articles per 60k with empty `<NUM/>` that still go Failed/quarantine. That's the article projection path, not the metadata roots in this slice, so correctly deferred — but it's the same "DTD-required yet sometimes-empty in real LEGI" class the NATURE fix just addressed. Worth a single deliberate tolerance policy in a later slice.
- The R1 suggestions on `hierarchy_path` dedup (`mod.rs:777`), `SECTION_TA` validity proxy, and `canonical_version` embedding per-record nature remain open and explicitly deferred per Plan §1.1 — unchanged by this fix, no action needed now.
- Trivial: `nature.clone()` at `mod.rs:951` then reuse at `mod.rs:961` could move instead of clone; negligible.

The R1 blocker is fully and correctly fixed, scoped precisely to the real defect, covered by a passing test, and free of accounting or query-readiness regressions.
