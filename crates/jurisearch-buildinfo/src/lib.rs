//! Single-source version stamping for the JuriSearch binaries.
//!
//! Every produced binary must answer `--version` with a string that embeds the workspace version, the
//! git commit, and the build target, e.g. `jurisearch-producer 0.1.0 (389d17e0437b, x86_64-unknown-linux-gnu)`.
//! This crate splits that into two halves that meet in the CALLING (binary) crate's compile env:
//!
//!   * [`stamp`] runs from the binary crate's `build.rs`. It resolves the commit + target and emits them
//!     as `cargo:rustc-env=…` directives, so they become `env!`-readable while THAT crate compiles.
//!   * [`version!`] expands, inside that same crate, to a `&'static str` built from `CARGO_PKG_VERSION`
//!     (the workspace version, via DRY `version.workspace = true`) plus the two env vars `stamp()` set.
//!
//! Because the env vars are scoped to whichever crate's `build.rs` called `stamp()`, the macro MUST be
//! used in a crate that itself invokes `stamp()`; pulling the macro into a crate without its own stamping
//! build.rs would fail to compile (the `env!`s would be unset). Each of the five bin crates wires both.

/// Expand to the binary's `--version` string: `"<pkg-version> (<commit>, <target>)"`.
///
/// `CARGO_PKG_VERSION` is the workspace version (all crates unify on `version.workspace = true`).
/// `JURISEARCH_BUILD_COMMIT` / `JURISEARCH_BUILD_TARGET` are stamped by [`stamp`] from the calling
/// crate's `build.rs`; this macro therefore only compiles in a crate whose own build.rs called `stamp()`.
#[macro_export]
macro_rules! version {
    () => {
        concat!(
            env!("CARGO_PKG_VERSION"),
            " (",
            env!("JURISEARCH_BUILD_COMMIT"),
            ", ",
            env!("JURISEARCH_BUILD_TARGET"),
            ")"
        )
    };
}

/// Stamp build metadata into the calling crate's compile env. Call this from a binary crate's `build.rs`
/// (`fn main() { jurisearch_buildinfo::stamp(); }`); the macro [`version!`] then reads it back via `env!`.
///
/// Emits two `cargo:rustc-env` directives:
///   * `JURISEARCH_BUILD_TARGET` — Cargo's `TARGET` triple (fallback `"unknown"`).
///   * `JURISEARCH_BUILD_COMMIT` — resolved with the precedence below.
///
/// Commit resolution precedence (first non-empty wins):
///   1. The `JURISEARCH_BUILD_COMMIT` env var, when set non-empty. This is the OVERRIDE contract that lets
///      release builds pin a deterministic, reproducible commit string regardless of the working tree.
///   2. Otherwise `git rev-parse --short=12 HEAD`, suffixed with `-dirty` when `git status --porcelain`
///      reports a non-empty working tree.
///   3. Otherwise `"unknown"` (no git, detached tarball build, any git failure) — never panics.
///
/// Rerun hints: so a dev build's `--version` never carries a stale commit, this emits `rerun-if-changed`
/// for the git paths that move the commit — HEAD, the active branch ref when HEAD is symbolic (the file
/// that actually changes on a branch commit, since `.git/HEAD` itself does not), and `packed-refs` — plus
/// `rerun-if-env-changed` for the override. Paths are resolved via `git rev-parse --git-path` (worktree-
/// correct) and emitted only when they exist. A no-git build emits no path hints and degrades silently.
///
/// Robustness: every git interaction is best-effort. Any failure degrades to `"unknown"`; this function
/// never panics and never blocks the build.
pub fn stamp() {
    // Build target triple — Cargo always sets TARGET for build scripts, but stay defensive.
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=JURISEARCH_BUILD_TARGET={target}");

    let commit = resolve_commit();
    println!("cargo:rustc-env=JURISEARCH_BUILD_COMMIT={commit}");

    // Reproducible release builds flip the commit via the env override, so rebuild when it changes.
    println!("cargo:rerun-if-env-changed=JURISEARCH_BUILD_COMMIT");

    // Rebuild when the commit can move, so a dev build's `--version` never carries a stale commit. We ask
    // git for the paths rather than guessing `.git/...`, which is worktree-correct: `git rev-parse
    // --git-path` resolves relative to this build script's CWD (the crate dir), exactly what
    // `rerun-if-changed` wants. The subtle case is a normal branch checkout: `.git/HEAD` then holds
    // `ref: refs/heads/<branch>` and does NOT change on commit — the file that moves is the resolved branch
    // ref (`.git/refs/heads/<branch>`), so we must watch THAT too. We emit a hint only for paths that exist,
    // so a not-yet-created path (e.g. a branch with no loose ref) can't force a rerun on every build.
    // No git / detached-HEAD tarball builds degrade silently: each helper returns None and emits nothing.
    let mut rerun_paths: Vec<String> = Vec::new();
    if let Some(head) = git_output(&["rev-parse", "--git-path", "HEAD"]) {
        rerun_paths.push(head);
    }
    // On a branch, `symbolic-ref -q HEAD` prints e.g. `refs/heads/main`; detached HEAD → empty/non-zero.
    if let Some(branch_ref) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(ref_path) = git_output(&["rev-parse", "--git-path", &branch_ref]) {
            rerun_paths.push(ref_path);
        }
    }
    if let Some(packed) = git_output(&["rev-parse", "--git-path", "packed-refs"]) {
        rerun_paths.push(packed);
    }
    for path in rerun_paths {
        if std::path::Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}

/// Apply the documented commit-resolution precedence (override env → git → `"unknown"`).
fn resolve_commit() -> String {
    // 1) Explicit override for reproducible/deterministic release builds.
    if let Ok(override_commit) = std::env::var("JURISEARCH_BUILD_COMMIT") {
        if !override_commit.is_empty() {
            return override_commit;
        }
    }

    // 2) Derive from git, appending `-dirty` for an unclean working tree. Any failure falls through.
    if let Some(mut short) = git_output(&["rev-parse", "--short=12", "HEAD"]) {
        if git_dirty() {
            short.push_str("-dirty");
        }
        return short;
    }

    // 3) No git available / git failed.
    "unknown".to_string()
}

/// Run `git <args>` and return its trimmed stdout, or `None` on any failure (missing git, non-zero exit).
fn git_output(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// True only when `git status --porcelain` succeeds AND reports a non-empty (dirty) working tree.
fn git_dirty() -> bool {
    git_output(&["status", "--porcelain"]).is_some()
}
