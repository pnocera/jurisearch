# Review — Task 0 Contracts/API Survey

## Findings

### WARN — Root workspace manifest ownership is missing from the parallel split

The survey marks M1-A as collision-free and lets M1-A, M1-C, and M2-A start from C0 (`work/10-next-plans/task0-contracts-survey.md:90`, `work/10-next-plans/task0-contracts-survey.md:105`), but all three proposed “new crate” paths require the same root workspace manifest. `Cargo.toml:1` through `Cargo.toml:16` currently lists all workspace members explicitly; adding `crates/jurisearch-deploy`, `crates/jurisearch-pipeline`, and either a fetch crate or module will collide there even if their crate-local files are disjoint. This is exactly the kind of first-wave merge collision the Task 0 gate is meant to flush out.

Recommended fix: add root `Cargo.toml` to the shared-file hazard list and define a single owner/handoff. The cleanest option is a tiny C0/scaffolding commit that creates the workspace-member entries and empty crate manifests before parallel agents start; otherwise serialize root-manifest edits through the orchestrator and require each agent to rebase before review.

### WARN — S7 must not offer `&mut postgres::Client` as an equivalent public seam

The survey’s S7 row says to generalize `producer` to `&impl DbClientSource` “or `&mut postgres::Client`” (`work/10-next-plans/task0-contracts-survey.md:76`). The second option is not equivalent for the current public build path: `build_incremental` opens one client for the corpus build lock and a separate `fence_conn` for the outbox fence (`crates/jurisearch-package-build/src/incremental.rs:105`, `crates/jurisearch-package-build/src/incremental.rs:110`). `build_remote_manifest` also opens its own fresh client before taking the corpus lock (`crates/jurisearch-package-build/src/remote_manifest.rs:62`). Passing one borrowed client as the top-level seam either loses the dedicated fence connection or forces the caller to know internals that the seam is supposed to hide.

Recommended fix: tighten S7 to a client factory/connection-source trait only, e.g. `DbClientSource { fn client(&self) -> Result<postgres::Client, StorageError>; }`, with an optional private helper that takes explicit `&mut db` and `&mut fence_conn`. Do not let the M1-C prompt present a single `&mut postgres::Client` as a safe replacement for the public producer/build API.

### WARN — Producer config parser ownership is still unresolved against the source plans

The survey assigns M1-A to site config/rendering only (`work/10-next-plans/task0-contracts-survey.md:92`) and M1-C to reusable pipeline APIs (`work/10-next-plans/task0-contracts-survey.md:94`), but the macro M1 deliverables include a minimum producer config parser for `[producer]`, `[database]`, `[fetch]`, `[package]`, `[enrichment]`, `[embedding]`, and `[baseline_refresh]` (`work/10-next-plans/00-macro-implementation-plan.md:129`). The detailed producer plan also defines that config shape (`work/10-next-plans/02-auto-update-server-crons.md:260`). The orchestrator instructions later put the producer config parser on the Task 2 producer update agent (`work/10-next-plans/04-claude-orchestrator-instructions.md:281`), so the survey needs to reconcile that source-doc split instead of leaving the owner implicit.

Recommended fix: update §3/§4 with an explicit decision: either M1-A’s deploy/config crate owns shared config primitives plus the producer config skeleton now, or M2-B owns producer config later and the survey records that this is an intentional deferral from the macro M1 list. Also assign shared redaction/file-permission helpers from the macro M1 deliverables (`work/10-next-plans/00-macro-implementation-plan.md:135`) so M1-A and the later producer agent do not invent parallel secret-handling code.

### NIT — The `ErrorObject` cycle warning names the wrong owner

The survey warns not to let `jurisearch-pipeline` use “`ErrorObject` from cli” (`work/10-next-plans/task0-contracts-survey.md:171`). In the actual code, `ErrorObject` is owned by `jurisearch-core` (`crates/jurisearch-core/src/error.rs:17`), and `jurisearch-cli` only depends on that core crate (`crates/jurisearch-cli/Cargo.toml:15`). So there is no `jurisearch-cli` cycle merely from referencing `ErrorObject`.

Recommended fix: reword the cycle check to “pipeline must not depend on `jurisearch-cli`; prefer pipeline-local typed errors for producer APIs, while `jurisearch-core::error::ErrorObject` remains a core protocol type.” That preserves the intended boundary without misleading implementation agents.

## Verified Points

The main codebase claims in the survey check out. `producer_cycle` and the package build/manifest path are `ManagedPostgres`-typed but use it as a fresh-client source (`crates/jurisearch-package-build/src/cycle.rs:59`, `crates/jurisearch-package-build/src/incremental.rs:97`, `crates/jurisearch-package-build/src/remote_manifest.rs:62`). The external connection primitives already exist in storage (`crates/jurisearch-storage/src/backend.rs:30`, `crates/jurisearch-storage/src/backend.rs:107`, `crates/jurisearch-storage/src/backend.rs:172`). Migrations are currently a `ManagedPostgres` method over the free `MIGRATIONS` array (`crates/jurisearch-storage/src/migrations.rs:40`, `crates/jurisearch-storage/src/migrations.rs:1130`). `request_model` is not part of `storage_embedding_fingerprint()` (`crates/jurisearch-embed/src/config.rs:48`, `crates/jurisearch-embed/src/fingerprint.rs:17`). `jurisearch-producer` and `jurisearch-deploy` are absent from the workspace (`Cargo.toml:2`), and `deploy/` currently contains only the three static systemd units.

VERDICT: FIXES_REQUIRED
