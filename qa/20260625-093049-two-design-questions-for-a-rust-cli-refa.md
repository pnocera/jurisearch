# Two Rust CLI Refactor Decisions

## 1. Shared Command Request Structs

**Recommendation: choose A. Keep clap `*Args`; add serde `*Request` payload-input structs.**

This is the right tradeoff for this repo even though it does not reduce the raw field-listing count as much as B. The important DRY win is not "one field list everywhere"; it is making the payload input and validation path single-source across one-shot, session, serve, batch, and eval callers without risking clap/help behavior.

Deciding source facts:

- The clap surface is already isolated in `args.rs`, and it owns user-facing help/default behavior. `Cli.index_dir` is a global arg, separate from command args ([args.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/args.rs:24)); `SearchArgs` is a clap `Args` struct with `#[arg(...)]` attributes and help comments but no `index_dir` ([args.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/args.rs:145)).
- The session surface is a separate JSON DTO layer. `SessionSearchArgs` derives `Deserialize`, has serde defaults, and includes `index_dir` ([session.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/session.rs:22)). The wrapper then rebuilds `SearchArgs` field-by-field and passes `index_dir.as_deref()` separately ([session.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/session.rs:144)).
- Validation is duplicated today: one-shot dispatch rejects empty search/top-k before `emit_search` ([dispatch.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/dispatch.rs:32)), and the session wrapper repeats similar checks ([session.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/session.rs:144)).
- The payload/eval path is coupled to `SearchArgs`: `search_payload` takes `(SearchArgs, Option<&Path>)` and delegates to `zone_search_payload` or `search_with_postgres` ([search.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/retrieval/search.rs:38)); `search_with_postgres` takes `&SearchArgs` and is reused by eval paths ([search.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/retrieval/search.rs:150)).

Concrete shape:

- Add `crates/jurisearch-cli/src/request.rs`.
- Define `SearchRequest`, `FetchRequest`, `CiteRequest`, etc. as serde DTOs with `index_dir: Option<PathBuf>`.
- Move request-derived helpers to the request type: `SearchRequest::retrieval_options()` and `SearchRequest::decision_filters()`.
- Move validation to either `XRequest::validate()` or the payload entrypoint. Preserve exact error strings by copying the current one-shot strings where they are currently user-visible. For search, be careful: one-shot currently says `search --top-k must be at least 1`, while session says `search top_k must be at least 1`; if byte-identical session/one-shot parity is now required, intentionally pick and test one canonical message.
- Add `impl SearchArgs { fn into_request(self, index_dir: Option<PathBuf>) -> SearchRequest }` and equivalent small conversions.
- Change payloads to take `XRequest` or `&XRequest`; make session wrappers just deserialize `XRequest` and call the payload.
- Change eval callers to construct `SearchRequest` directly. This removes their dependency on clap `SearchArgs`.

Why not B:

- The compiled schema is not derived from clap structs, so B should not directly perturb the golden schema. The core schema is hand-assembled from domain files in `compiled_schema()` ([schema/mod.rs](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema/mod.rs:10)), and command names/schema names come from a hand-maintained `COMMANDS` inventory ([contract.rs](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/contract.rs:72)). The golden test guards the assembled JSON byte-for-byte ([schema/mod.rs](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema/mod.rs:120)).
- But B still couples two unrelated contracts in one struct: clap help/defaults and JSON session DTO defaults. A harmless-looking serde rename/default change could alter session behavior; a harmless-looking clap change could alter `--help`; and `index_dir` would become a hidden field that must be post-filled into nested subcommand structs after `Cli::parse()`.
- `#[arg(skip)]` on `Option<PathBuf>` should be mechanically viable because `Option<PathBuf>: Default` gives `None`, but it is precisely the kind of clap derive detail I would not put on the critical path when the requirement is byte-identical CLI/session behavior and no help drift.
- B also makes eval code depend on a clap-derived type forever unless you introduce a request/newtype later anyway. A separates "CLI parsing" from "command request" now.

Index-dir-only / low-duplication commands:

Use option **ii**: keep leaf payload signatures for commands that have essentially no duplication.

- `doctor_payload(index_dir: Option<&Path>)` and `stats_payload(index_dir: Option<&Path>)` are called directly from one-shot dispatch ([dispatch.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/dispatch.rs:89)) and from tiny session DTOs ([session.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/session.rs:386)).
- `model_fetch_payload(model, allow_download)` is similarly small; session has a tiny `SessionModelFetchArgs` and calls the payload directly ([session.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/session.rs:359)).

Do not create `DoctorRequest` / `StatsRequest` / `ModelFetchRequest` in the CLI implementation just for aesthetic uniformity unless you are also using them to remove real validation or payload duplication. The schema can still advertise request shapes independently in `jurisearch-core`; implementation uniformity is not worth churn here.

## 2. ArchiveIngestRun Runner

**Recommendation: choose b, but adjust the seam: extract only the member batching/read loop, and have the helper own/return the visited count instead of taking `&mut visited` while the flush closure captures `&mut counters`.**

The full generic adapter runner is too much ceremony for the current divergence. It would abstract the most sensitive part of ingestion accounting, and it would force heterogeneous counters, manifest shape, LEGI hierarchy backfill, and response JSON into a trait whose methods mostly exist to re-express the current functions indirectly.

Deciding source facts:

- The duplicated lifecycle is real, especially the archive/member batching loop. LEGI loops archives, batches XML members by count/byte limit, flushes before overflow, flushes tail members, handles read errors, and stops on `--limit-members` ([legi.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/legi.rs:162)). JURI has the same control flow with a source-aware flush call ([juri.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/juri.rs:160)).
- The surrounding accounting is not uniform enough for a full runner. LEGI has metadata-root counters and hierarchy-backfill state in `LegiArchiveIngestCounters` ([legi.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/legi.rs:5)), then performs a LEGI-only scoped hierarchy backfill before final manifest/update/finish ([legi.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/legi.rs:244)). JURI has different counters, explicit `zone_accurate=false` manifest fields, and no post-loop backfill ([juri.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/juri.rs:41)).
- The flush functions have different signatures and commit behavior. LEGI flushes with `(client, run_id, archive_name, pending, bytes, quarantine, counters)` ([legi.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/legi.rs:361)); JURI flushes with the extra `source` parameter and a different counter type ([juri.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/juri.rs:318)).
- The terminal run status is deliberately recomputed after manifest update in both paths; the JURI code comment calls this out as a reviewed correctness point ([juri.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/ingest/juri.rs:245)). I would not hide that in a generic trait runner.

Better lightweight seam:

Extract one helper that processes **one planned archive** and owns batching mechanics only:

```rust
struct ArchiveBatchReadReport {
    visited_members: usize,
    stopped_by_limit: bool,
}

enum ArchiveBatchReadError {
    Flush { visited_members: usize, error: StorageError },
    Read { visited_members: usize, error: anyhow::Error }, // or the concrete archive read error type
}

fn read_archive_members_batched<F>(
    archive_path: &Path,
    archive_name: &str,
    max_member_bytes: u64,
    limit_members: Option<usize>,
    visited_members: usize,
    mut flush: F,
) -> Result<ArchiveBatchReadReport, ArchiveBatchReadError>
where
    F: FnMut(&str, &mut Vec<ArchiveMember>, &mut usize) -> Result<(), StorageError>,
```

The helper should own:

- `pending_members`,
- `pending_member_bytes`,
- the `for_each_xml_member_until` call,
- "flush before overflow",
- "flush at batch size/byte limit",
- tail flush after successful read,
- `ArchiveVisit::Stop` for `limit_members`.

The caller should own:

- `fatal_error` conversion and exact read-error message text,
- `counters.visited_members = report.visited_members`,
- archive loop break conditions,
- manifest/start/finish/run-status recomputation,
- LEGI hierarchy backfill,
- final response JSON.

Important caveat on the proposed `&mut visited` split:

- Do **not** pass `&mut counters.visited_members` while the flush closure captures `&mut counters`; that will create an awkward or impossible borrow shape because one mutable borrow targets a field and another targets the whole struct.
- Instead pass `visited_members` by value into the helper and return the updated count in both success and error cases. The flush closure can freely capture `&mut counters` for source-specific counters.

Why not full generic runner:

- `ArchiveIngestAdapter { type Counters }` would remove more visible lines but replace them with an indirect protocol around counters, manifest construction, post-loop hooks, and response assembly. Most adapter methods would be thin wrappers over existing functions plus field accessors like `visited_members(&Counters)`.
- That abstraction makes it harder to audit the ingestion contract: exactly when the run starts, when the final manifest is written, when `run_status` is recomputed, and when replay snapshot refresh occurs.
- The managed-Postgres ingest tests are guarding behavior, not abstraction elegance. A small batching helper is far less likely to corrupt accounting/resume/quarantine semantics.

Validation after the loop extraction:

- Run the managed-Postgres ingest contract tests covering LEGI and JURI accounting/resume/quarantine.
- Add a small unit test around the batching helper if practical: it should flush on byte overflow, flush on count limit, flush the tail once, stop exactly at `limit_members`, and return the visited count on flush/read failure.

Bottom line: take the real duplication in the member reader/batcher, but keep ingestion lifecycle and accounting explicit in the two payloads.
