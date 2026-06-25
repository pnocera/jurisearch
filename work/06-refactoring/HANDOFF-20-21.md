# Handoff: refactoring-plan.md — remaining SOLID/DRY follow-ups #20 and #21

Date: 2026-06-25. Branch: `refactor/cli-module-split` (NOT pushed, NOT merged to main).
Next task: implement **SOLID/DRY #20 (shared request structs)** and **#21 (ArchiveIngestRun
runner)** — the user said to do BOTH (this supersedes an earlier "#21 only" answer).

---

## 1. Current state (what's DONE)

Implementing `work/06-refactoring/refactoring-plan.md`. 40 commits on the branch since
`540609d`. **Everything is done and codex-reviewed (GO) EXCEPT #20 and #21:**

- Phases 0–6: full CLI decomposition. `crates/jurisearch-cli/src/main.rs` 13,747 → **285 lines**.
  Split into args/output/dispatch/session/serve, leaf modules (ascii/date/citation/errors/
  query_support/legifrance_search/embedding_runtime/index_runtime), and subtrees retrieval/,
  enrichment/, eval/, ingest/, gates/, plus status.rs. Unit tests → `src/tests.rs`. Integration
  tests `tests/cli_contract.rs` → 6 domain suites + `tests/support/mod.rs`.
- All 8 secondary library splits: ingest legi/juri, storage retrieval/projection/
  ingest_accounting, core schema (golden-test-guarded), embed, official-api.
- Visibility minimization across all splits (private / pub(super) / pub(crate) / pub).
- SOLID/DRY #19 (command-inventory session-exclusion unification) — `CommandSpec.session_excluded`.

**DoD met:** no production file >2000 lines; CLI JSON contracts byte-identical; all tests green
(`cli 106, storage 42 incl. PG integration, ingest 62, core 12 incl. schema golden, embed 15,
official-api 18`); CodeGraph can scope per-command context.

Reviews are in `reviews/2026-06-25-*.md` (16 codex reviews, all GO; the secondary batch took
3 rounds r1→r3 to GO over the visibility issue).

## 2. Verify before starting

```
git -C /home/pierre/Work/jurisearch log --oneline -5
cargo test -p jurisearch-cli            # 106 pass (53 unit + 53 contract, 2 ignored)
cargo test -p jurisearch-core           # 12 pass incl. compiled_schema_matches_golden
```

## 3. GOTCHAS — read these first

1. **NEVER `git add -A`.** There are untracked `work/07-datasets/*.sh` + README in the working
   tree — the user's SEPARATE work. `git add -A` once swept them into a commit (had to amend it
   out). Always `git add <specific crate/review paths>`.
2. **`cargo fmt --check` fails repo-wide, pre-existing** (nightly rustfmt 1.9, no CI fmt gate).
   All moves are VERBATIM to avoid mass-reformat churn. Do NOT run `cargo fmt` on the tree. The
   DoD's "fmt passes" conflicts with the plan's mechanical-commit rule — documented, not chased.
3. **Schema-affecting struct fields need `#[serde(skip)]`.** `core::contract::COMMANDS` is
   serialized into the agent schema; the byte-identical golden test
   `core::schema::tests::compiled_schema_matches_golden` (fixture `src/schema_golden.json`)
   catches any drift. (#19 added `CommandSpec.session_excluded` with `#[serde(skip)]` for exactly
   this reason.) Regenerate the golden only on an INTENTIONAL schema change:
   `cargo test -p jurisearch-core regenerate_schema_golden -- --ignored`.
4. **Module visibility:** binary-crate (CLI) submodules use `use crate::*` (main.rs is the hub
   that keeps external imports + re-exports submodules); library-crate submodules use
   `use super::*` (mod.rs/lib.rs hub re-exports public API). Descendants can glob a parent's
   private `use` bindings — that's how the hub pattern resolves everything. After any split,
   run `work/06-refactoring/tools/minimize_crate.py` to revert pubify's over-widening.
5. **Per-follow-up codex review.** Use the `codex-review` skill (`/home/pierre/.claude/skills/
   codex-review/scripts/codex_session.sh review ...`), run in background. Give minimal scope
   (the commit/diff), let codex review independently. FIXES_REQUIRED → fix + re-review (rN).
   Apply ALL severities (BLOCKER/WARN/NIT).
6. The user is autonomous-execution oriented: don't stop for non-blockers; ask codex (not the
   user) for technical decisions. (See memory `autonomous-execution`, `review-before-execute`,
   `ask-codex-before-important-decisions`.)

## 4. Tooling (`work/06-refactoring/tools/`)

- `cli_split.py` — Rust top-level-item span extractor. Modes: `spans <f>`, `show <f> <anchors>`,
  `names <f> <name1,name2>` (resolve names→anchor lines; errors on ambiguity — for duplicate
  impls pass anchor numbers directly), `extract <f> <out_blob> <anchors>` (cut items + remove),
  `pubify <f>` (mark items/fields/inherent-methods pub(crate)). Used for all the moves.
- `move_test_module.py <src.rs> <out_tests.rs> "<doc>"` — moves a trailing
  `#[cfg(test)] mod tests { ... }` into a sibling file with a RAW-STRING-AWARE dedent (preserves
  XML/JSON fixtures byte-identically). Use if a payload's tests need relocating.
- `minimize_crate.py <crate> <submodule.rs>...` — strips `pub(crate)`→private in the listed
  files, then compiler-drives `pub(super)` back onto genuine cross-submodule/test items. Run
  after any new split. NOTE: it bumps cross-crate items to pub(super) too — for items reached via
  `crate::x::y` from ANOTHER crate-module you must hand-bump to pub(crate) (see how
  retrieval's zone_retrieval items were handled in commit "minimize over-widened visibility").
- `strip_pub_crate.py`, `split_schema.py` — one-offs (schema split is already done).

The mechanical move recipe: `cli_split.py names`→`extract`→`pubify` the blob → write the new
module file with a header (`use crate::*` for CLI, `use super::*` + imports for libs) → wire
`mod`/`use`/`pub use` in the hub → build → fix → `minimize_crate.py` → test → review.

---

## 5. SOLID/DRY #20 — shared command request structs (DRY, "biggest hole")

Plan ref: `refactoring-plan.md` "SOLID/DRY Design Follow-ups" #2 + the Phase 2 notes.

**The duplication:** `crates/jurisearch-cli/src/session.rs` has 14 `Session*Args` serde DTOs
(SessionSearchArgs, SessionFetchArgs, SessionCiteArgs, SessionContextArgs, SessionRelatedArgs,
SessionCompareArgs, SessionStatusArgs, SessionEvalPhase1Args, SessionModelFetchArgs,
SessionDoctorArgs, SessionStatsArgs, SessionInspectArgs, SessionVersionsArgs, SessionDiffArgs)
that duplicate nearly every field of their clap `*Args` twin in `args.rs`. Each
`session_*_payload(args: Value)` wrapper does `from_value::<Session*Args>` then manually rebuilds
the clap `*Args` field-by-field and calls the payload builder.

**Target design:** a new `crates/jurisearch-cli/src/request.rs` with shared internal request
structs (SearchRequest, FetchRequest, CiteRequest, ContextRequest, RelatedRequest, CompareRequest,
… one per command). Each:
- holds the command's fields **plus `index_dir: Option<PathBuf>`** (the wrinkle — see below);
- derives `Deserialize` with the SAME serde defaults the `Session*Args` had (these reuse the
  `default_*` fns in args.rs);
- is produced two ways: `TryFrom<*Args>` (or a `*Args::into_request(self, index_dir)` method) for
  the one-shot/clap path, and serde `from_value` for the session path.

Payload builders take the shared request (e.g. `search_payload(req: SearchRequest)`) instead of
`(SearchArgs, Option<&Path>)`, using `req.index_dir.as_deref()` internally.

**THE index_dir WRINKLE (key):** clap `*Args` do NOT carry `index_dir` (it's a global CLI arg
passed separately today). The session DTOs DO (server-injected into the request JSON). So the
shared request must carry `index_dir`. The clap path (`dispatch::run` / the `emit_*` fns) must
build the request from `*Args` + the global `index_dir`. So `TryFrom<*Args>` alone can't carry
index_dir — either add it after (`*Args::into_request(self, index_dir: Option<&Path>)`), or accept
a 2-arg builder. Confirm the schema's `*Request` (in `core/schema/search.rs` etc.) already
documents/accepts `index_dir` the same way `Session*Args` did (it should — Session*Args is the
current session deserialization target and matches the schema; so SearchRequest == SessionSearchArgs
serde-shape). NO schema change should be needed (the golden test will confirm).

**Validation:** today the empty-query / top_k==0 checks live BOTH in `dispatch::run` (clap path,
emits bad_input→exit 2) and in each `session_*_payload` wrapper. Consolidate into the payload (or
a request `validate()`), so both paths validate once. Same error code (bad_input) + same exit (2)
= behavior preserved.

**Files touched:** NEW `request.rs`; `args.rs` (TryFrom/into_request impls; keep the clap `*Args`);
`session.rs` (delete the 14 `Session*Args`; each wrapper becomes
`from_value::<XRequest>(args).map_err(...)?` → `x_payload(req)`); `retrieval/*.rs` + `status.rs`
(payload signatures); `dispatch.rs` (run builds the request from args + index_dir; remove the
pre-dispatch validation it duplicates).

**Approach:** PILOT on `search` end-to-end first (SearchRequest + signature + both call sites +
delete SessionSearchArgs), `cargo test -p jurisearch-cli` (esp. `session_dispatch_matches_one_shot_
only_set`, `every_command_and_arg_has_help`, the search contract tests), then apply to the rest
one command at a time. Likely 1 commit (or a couple). Then codex review.

**Risk:** moderate — session/one-shot parity is contract-heavy; the contract suites guard it.

---

## 6. SOLID/DRY #21 — ArchiveIngestRun runner (DRY)

Plan ref: `refactoring-plan.md` "SOLID/DRY Design Follow-ups" #3.

**The duplication:** `crates/jurisearch-cli/src/ingest/legi.rs::ingest_legi_archives_payload` and
`ingest/juri.rs::ingest_juri_archives_payload` share the whole archive-run lifecycle:
open index → bulk pg client + `SET synchronous_commit off` → `plan_from_dir` → 
`select_archives_to_process` → build initial manifest → `start_ingest_run_with_client` → the
per-archive member-batching loop (read members via `for_each_xml_member_until`, batch by
LEGI_INGEST_TRANSACTION_BATCH_SIZE / _BYTE_LIMIT, flush, fatal-error handling, `--limit-members`
stop) → build final manifest (RECOMPUTE terminal run_status after the manifest update) →
`update_ingest_run_manifest_with_client` → `finish_ingest_run_with_client` → replay-snapshot
refresh → response assembly.

**They differ in:** the `source.is_jurisprudence()` precheck (juri only); plan source
(`ArchiveSource::Legi` vs the juri `source` param); run_id default (`default_legi_run_id()` vs
`default_juri_run_id(source)`); manifest fn (`legi_archive_manifest(plan,...)` vs
`juri_archive_manifest(source, plan,...)`); `start_ingest_run` source/parser_version/schema_version
(LEGI_PARSER_VERSION/CANONICAL_SCHEMA_VERSION vs JURI_*); **the counters TYPE**
(`LegiArchiveIngestCounters` vs `JuriArchiveIngestCounters` — different structs); the flush fn
(`flush_legi_archive_member_batch(client, run_id, name, ...)` vs
`flush_juri_archive_member_batch(client, source, run_id, name, ...)`); the read-error label; **a
LEGI-ONLY hierarchy-backfill step** between the loop and the manifest
(`backfill_legi_article_hierarchy_from_metadata_scoped`, updating two counter fields); and the
final **response shaping** (counters → JSON).

**THE hard part:** the counters are different concrete types, and the loop reads
`counters.visited_members` while the flush mutates the rest. A fully-generic
`ArchiveIngestRun<A: Adapter>` would need an associated `Counters` type + trait methods. Two
designs:
- **(a) Full runner + adapter trait** (the plan's vision): `trait ArchiveIngestAdapter { type
  Counters: Default; fn source_str/parser_version/schema_version/run_id_default; fn
  flush(&mut Counters,...); fn manifest(plan, latest, &Counters, status) -> Value; fn
  visited_members(&Counters)->usize; fn failed_members(&Counters)->usize; fn post_loop(&postgres,
  &mut Counters) (legi backfill, no-op for juri); fn response(...) -> Value; }`. Higher risk.
- **(b) Lighter: extract just the member-batching loop** (the ~80 nearly-identical lines) into a
  helper `read_archive_members_batched(archive, max_bytes, limit, &mut visited, flush_closure)`
  taking the flush as a closure that captures the source-specific `&mut counters`. The
  `visited_members` counter likely needs to move out of the counters struct (or be passed as
  `&mut usize`). Keeps open/start/finish/manifest/backfill/response in each payload. LOWER risk,
  captures most of the dedup. **Recommended starting point** — discuss with codex which design.

**Behavior preservation is critical** (corrupting ingestion accounting/resume/quarantine is bad).
Guards: the `cli_ingest_contract` integration tests (`ingest_legi_archives_records_accounting_and_
quarantines_failures`, `..._same_run_resume_keeps_inserted_members_inserted`, `..._skips_no_text_
articles...`, the juri equivalents, `ingest_backfill_legi_hierarchy_updates_full_index`) — these
spin up a managed postgres, so run `cargo test -p jurisearch-cli` (PG is available in this env).
Also `cargo test -p jurisearch-storage ingest_accounting` / `zone_units`.

**Files touched:** `ingest/legi.rs`, `ingest/juri.rs`, and the new runner/helper (put it in
`ingest.rs` root or a new `ingest/run.rs`, private to the ingest module). Keep it private — do
NOT promote into `jurisearch-ingest` (no non-CLI caller).

**Risk:** high (intricate, integration-tested ingestion path; heterogeneous counter types).
Consider asking codex to sanity-check the adapter design before implementing.

---

## 7. After both land

- Full validation: `cargo test` across all crates (counts above).
- Update `reviews/` with each follow-up's codex review.
- Update memory `~/.claude/projects/-home-pierre-Work-jurisearch/memory/cli-refactor-progress.md`.
- The branch still needs the user's decision on push/merge to main (recent history commits
  directly to main, but this branch isolates the big refactor).
