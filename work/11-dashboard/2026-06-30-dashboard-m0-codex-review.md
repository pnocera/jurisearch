# Findings

## BLOCKER: `bun.lock` is generated but currently ignored, so the scaffold cannot commit the reproducible Bun workspace lockfile

`apps/dashboard/bunfig.toml:6-8` says installs should write exact versions into `bun.lock`, and `apps/dashboard/bun.lock` does exist, but root `.gitignore:33` ignores `*.lock`. `git status --ignored --porcelain apps/dashboard` reports `!! apps/dashboard/bun.lock`, while `git status --porcelain apps/dashboard` only shows the directory as untracked. That means a normal `git add apps/dashboard` will omit the lockfile, undermining the Bun 1.3.14/reproducible-tooling contract for M0.

Concrete fix: add an explicit unignore after the root `*.lock` rule, for example `!/apps/dashboard/bun.lock`, or narrow the Codex-review ignore pattern so it does not catch package-manager lockfiles. Then verify `git check-ignore -v apps/dashboard/bun.lock` reports nothing and `git status --porcelain apps/dashboard/bun.lock` shows it as addable.

## WARN: The smoke test is false-green for the hard `--version` contract

The only committed dashboard test is `apps/dashboard/shared/src/index.test.ts:4-6`, which asserts the brand string. `bun run test` does run `scripts/stamp.ts` first via `apps/dashboard/package.json:19`, but it never asserts the generated `BUILD_VERSION`, `BUILD_COMMIT`, `BUILD_TARGET`, or the final `server/main.ts:11` output. A regression such as hardcoding the package version, changing the target string, dropping the env override, or formatting the line differently would still pass `bun test`; it would only be caught by manual observation or a later release audit.

Concrete fix: add an automated contract test. The most direct shape is to refactor `apps/dashboard/scripts/stamp.ts` into testable helpers for resolving the Cargo workspace version, commit override/fallback, target, and generated contents, then add tests for default commit, empty `JURISEARCH_BUILD_COMMIT`, non-empty override, and exact `jurisearch-dashboard <version> (<commit>, x86_64-unknown-linux-gnu)` formatting. Also add a smoke script that compiles the binary and runs `dist/jurisearch-dashboard --version` against an expected string derived from root `Cargo.toml` plus the override, so the same path M6a will audit is covered before merge.

## WARN: `stamp.ts` does not fully mirror the release fallback behavior when git metadata is unavailable

`dist.sh:253-262` derives `BUILD_COMMIT` from `git rev-parse --short=12 HEAD`, then falls back to a shortened full commit or `unknown`; `crates/jurisearch-buildinfo/src/lib.rs:96-115` also degrades to `unknown` instead of failing. In contrast, `apps/dashboard/scripts/stamp.ts:45-49` throws if git cannot resolve a short commit and `JURISEARCH_BUILD_COMMIT` is unset. Release integration will likely export `JURISEARCH_BUILD_COMMIT`, so this is not breaking the current happy path, but it is not faithful to the existing source contract for no-git or source-tarball builds.

Concrete fix: make `resolveCommit()` use the same precedence as the release/Rust buildinfo path: first non-empty `JURISEARCH_BUILD_COMMIT`, then `git -C <repo> rev-parse --short=12 HEAD`, then a full `rev-parse HEAD` truncated to 12 if available, then `"unknown"`. Cover that behavior in the stamp tests above.

## NIT: The `server` workspace does not yet declare or exercise the shared contract dependency

`apps/dashboard/web/package.json:9-11` declares `@jurisearch-dashboard/shared` and `apps/dashboard/web/src/App.vue:4` proves web-side resolution. The server package has no dependency entry in `apps/dashboard/server/package.json:1-7`, and `apps/dashboard/server/main.ts:1-22` does not import `shared`. That is acceptable for a pure M0 `--version` stub, but the design says shared DTOs/validators are imported by both backend and frontend, and M1 will immediately need the server side of that dependency.

Concrete fix: either add `@jurisearch-dashboard/shared: "workspace:*"` to `server/package.json` now with a minimal server-side import/test that proves resolution, or explicitly defer that manifest edge to M1 and add it as an M1 checklist item so backend DTO imports do not rely on undeclared workspace behavior.

VERDICT: FIXES_REQUIRED
