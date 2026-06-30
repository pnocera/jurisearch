# Codex Review - Dashboard M6a `dist.sh`

## Findings

- WARN: `dist.sh` now invokes `bun run compile` from `apps/dashboard` at `dist.sh:334-337`, but that build path writes more than `apps/dashboard/dist/jurisearch-dashboard`: `scripts/stamp.ts` rewrites `apps/dashboard/server/buildinfo.ts`, `build/gen-embed.ts` rewrites `apps/dashboard/server/embedded-assets.generated.ts`, and the web build rewrites `apps/dashboard/web/dist` before the final binary is emitted (`apps/dashboard/build/compile.ts:33-58`). These paths are repo-local and gitignored, so this does not break the bundle, checksum, or version audit, but it means the robustness criterion "writes only under `apps/dashboard/dist`" is not actually true and release runs leave additional generated source-tree artifacts behind. Concrete fix: either update the release-script comments/plan to document the full gitignored write set, or add a dashboard compile mode that stages generated files under the already validated `$TARGET_DIR`/scratch area and have `dist.sh` install the audited binary from there.

- NIT: The generated README's update-server install snippet still installs only `jurisearch-producer` (`dist.sh:610-616`) even though the bundle table and manifest now declare both `jurisearch-producer` and `jurisearch-dashboard` (`dist.sh:571-574`, `dist.sh:510-514`). M6b is where the dashboard service/config install is planned, so this is not a bundle correctness issue, but the generated release README is internally inconsistent for anyone manually following it. Concrete fix: add the dashboard to the update-server install command, or explicitly state in that snippet that dashboard service/config installation is completed by M6b/deploy integration.

## Review Notes

The Cargo-build set and bundle-membership set are separated correctly. `BUILD_BINS` still derives from `UPDATE_SERVER_BINS`, `SITE_SERVER_BINS`, and `CLI_BINS` only, so `jurisearch-dashboard` never reaches `cargo build --bin` or `copy_bin`. Places that mean legitimate bundle contents use `UPDATE_SERVER_BUNDLE_BINS`: update-server `--audit-only`, both update-server `audit_dir` calls, `ALL_BINS`, the manifest bundle list, and the README bundle table. Because `ALL_BINS` includes the dashboard while site-server and cli allowed lists do not, a dashboard binary leaking into either other bundle is still flagged.

The dashboard `--version` audit reads the just-built `apps/dashboard/dist/jurisearch-dashboard`, compares the whole stdout line to `jurisearch-dashboard $VERSION ($BUILD_COMMIT, $TARGET)`, and exits 6 on mismatch. The shared stamp path is sound: `dist.sh` exports `JURISEARCH_BUILD_COMMIT`, `bun run compile` runs `bun run stamp`, and `scripts/stamp.ts` gives that override precedence before writing `server/buildinfo.ts`. A stale or unstamped binary with a different version, commit, or target would fail before it is installed into `dist/update-server/bin`.

The forbidden-asset and manifest paths are consistent. The dashboard basename does not match the forbidden globs, embedded SPA/font files are not loose files in the bundle, `write_checksums` walks `bin/`, and the existing generated `dist/update-server/SHA256SUMS` and `dist/manifest.toml` both include `bin/jurisearch-dashboard` alongside `bin/jurisearch-producer`.

Checks run during review: `bash -n dist.sh`, `shellcheck dist.sh`, `git diff --check -- dist.sh`, `./dist.sh --audit-only dist/update-server`, and direct `--version` checks for the existing generated update-server producer/dashboard binaries. I did not rerun full `./dist.sh` because this review request only permits writing the review file, and the full build rewrites `dist/` plus dashboard build artifacts.

VERDICT: GO
