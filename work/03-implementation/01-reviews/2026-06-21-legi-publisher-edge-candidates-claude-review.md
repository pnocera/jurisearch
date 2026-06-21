# Claude Review: LEGI publisher edge candidates

Verdict: GO

Reviewed commit `b5ee0f3` ("Add LEGI publisher edge candidates") against parent `85ea343`.
Scope: Phase 0.5 — extract DILA publisher links into canonical graph-edge candidates.

## Findings
All findings are low-severity and non-blocking.

- **(Low) Tag coverage asserted in the plan is only partially unit-tested** — `is_publisher_link_tag` matches `LIEN`, `LIEN_ART`, `LIEN_SECTION_TA`, `LIEN_TXT`, `a`, `A` (`crates/jurisearch-ingest/src/legi/mod.rs:549-554`), but unit tests exercise only `LIEN` (`mod.rs:790-811`) and `a` (`mod.rs:836-856`). The matching path is uniform so behavior is correct, but the plan now claims all four DILA tags are emitted with no fixture locking `LIEN_ART`/`LIEN_SECTION_TA`/`LIEN_TXT`.
- **(Low) No negative assertion that `<LIENS>`/`LIEN` text is excluded from `body`** — body/edge separation is correct: `assign_article_text` only appends inside `BLOC_TEXTUEL/CONTENU` (`mod.rs:329`), and the fixture's `LIEN` ("Decret no 73-138 - art. 11") lives under `<LIENS>` so it never reaches `body`. Verified by tracing and by the passing tests, but `parses_official_article_to_canonical_document` doesn't assert `!body.contains("Decret no 73-138")`, so a future regression mixing link text into body would go uncaught.
- **(Low, robustness) `extract_known_source_uid` does not bound the captured suffix** (`mod.rs:573-588`) — it greedily takes all trailing ASCII alphanumerics after a known prefix, so a malformed `href`/`id` like `LEGIARTI000006554637X` would yield a non-canonical `to_source_uid`. Acceptable here because the "to" side is an unresolved candidate (canonical `validate()` only checks the "from" side), but downstream Postgres materialization must re-validate before resolving `to_document_id`.
- **(Low) Inline `<a>`/`<A>` anchors with no resolvable target still emit an edge** with `to_source_uid = None` (`mod.rs:492-497`). Non-reference anchors (e.g. `<a name=...>`) would become low-signal candidates. Fine since they retain `source_text`/attributes and downstream can filter on target presence, but worth a conscious decision.

## Suggestions
- Add a fixture/unit test covering `LIEN_ART`, `LIEN_SECTION_TA`, and `LIEN_TXT` so the "all four tags emitted" claim in the plan is enforced, not just the uniform matcher.
- Add `assert!(!document.body.contains("Decret no 73-138"))` (or similar) to lock body/edge separation, and assert the inline-anchor case still keeps "article suivant" in `body` to lock text continuity.
- The two-step `let document = CanonicalDocument { ... publisher_edges: Vec::new(), ... }; let mut document = document; document.publisher_edges = ...` (`mod.rs:392-423`) reads awkwardly; declaring `let mut document` directly (edges depend only on fields set before the initializer) would be cleaner with no behavior change.
- `collect_attributes` runs `local_name(attribute.key.as_ref())` (`mod.rs:533`); for attributes that passes the full raw key through a UTF-8-lossy helper that does not strip a namespace prefix. Harmless for DILA's unprefixed attributes, but the name implies handling it doesn't perform for attribute keys.
- Consider a doc comment on `CanonicalGraphEdge` capturing the candidate contract (`relation = refers_to`, `edge_source = publisher`, `to_document_id = None` until resolved, `edge_id` is content-addressed and order-stable via `index`) so the later graph-materialization step has the resolution rules in one place.
- The smoke's `publisher_edges > 0` assertion (`tests/legi_archive_subset.rs:103-106`) is sample-window dependent; it's safe because the test is `#[ignore]` and run manually, but a note that the 25-article window is expected to contain links would help future maintainers if the window ever shifts.

## Verification Notes
- Inspected the live diff with `git show b5ee0f3` and `git show --stat b5ee0f3` against parent `85ea343`.
- Read the full `crates/jurisearch-ingest/src/legi/mod.rs` and `crates/jurisearch-ingest/tests/legi_archive_subset.rs`.
- `cargo test -p jurisearch-ingest` → 16 unit + 3 contract tests pass; the real-archive smoke is correctly `ignored` (no normal-CI fragility).
- `cargo clippy -p jurisearch-ingest --tests` → no warnings or errors.
- `grep` for other `CanonicalDocument {` construction sites → none outside `legi/mod.rs`, so adding the `publisher_edges` field introduces no ingestion-contract break for downstream consumers.
- Traced the inline-anchor flow: anchor inner text is still appended to `body` (the `BLOC_TEXTUEL/CONTENU` path stays active while `<a>` is open) and is independently captured as the edge's `source_text`; `LIEN` under `<LIENS>` is excluded from `body`. Confirmed edges preserve provenance (`source_payload_hash`, `source_archive`, `source_member_path` copied from the resolved document), raw DILA attributes (`typelien`, `cidtexte`, `id`, `sens`), and `edge_source = "publisher"` with a conservative `refers_to` relation.
- Confirmed `edge_id` is deterministic and content-addressed (`from_document_id|index|source_tag|to_source_uid|source_text` → sha256, `publisher-edge:` prefix), giving order-stable, re-ingestion-idempotent candidate IDs suitable for Postgres graph materialization.
- Confirmed the plan update (`IMPLEMENTATION_PLAN.md:345-346`) accurately moves publisher-link extraction to Done and leaves `SECTION_TA`/`TEXTELR`, structural chunks, and DTD re-verification as remaining.
