# Codex Review — M7 Rebaseline Repair

## Findings

None.

## Verification Notes

- Diff scope is confined to `crates/jurisearch-producer` and `work/10-next-plans/05-hardening-and-freshness-followups.md`; no deploy/package-build/storage/client source is touched.
- Manual rebaseline routes through `UpdateOptions::rebaseline(...)` and the existing `run_update` orchestration, then calls `run_rebaseline_cycle(...)`, which invokes `jurisearch_package_build::rebaseline_cycle(...)` under the same `update-core` lock. Adoption is recorded after publish via `adopt_new_baselines(...)`, per source.
- `rebaseline --dry-run` uses `plan_forced_rebaseline(...)` directly from fetch cursors and returns before fetch, lock acquisition, DB access, or run-record/checkpoint mutation.
- Retention scan is allowlisted to fetch quarantine, ingest quarantine, archive `.part` sidecars, and stale JSON temp writes. The delete leg performs a second guard before `remove_file`, rejects anything under `corpora_dir`, and allows archive-mirror deletion only for `.part` sidecars; accepted `.tar.gz` archives, manifests, packages, and committed cursors are not selected by the scan.
- Judilibre freshness is represented only as a deferred diagnostic. The update path still treats Judilibre as optional enrichment: missing credentials return `SkippedNoCredentials` and classify as `published-enrich-degraded`, which remains a successful publish class rather than blocking ingest/embed/publish.
- I did not rerun the reported cargo validation because the review instructions said the relevant suite had already run green, and rerunning cargo here would write build artifacts outside the requested review file.

VERDICT: GO
