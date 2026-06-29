# M5-B demo/smoke/watchdog review

## Findings

### BLOCKER: live watchdog can never classify normal PostgreSQL timestamps as stale

`watchdog_corpus` gets the cursor age by reading `applied_at` through `corpus_status`, parsing it with `parse_rfc3339_unix`, and treating parse failure as `None`:

- `crates/jurisearch-deploy/src/ops/watchdog.rs:207`
- `crates/jurisearch-deploy/src/ops/watchdog.rs:256`
- `crates/jurisearch-deploy/src/ops/watchdog.rs:131`

The source value is `applied_at::text` from PostgreSQL:

- `crates/jurisearch-syncd/src/status.rs:35`

PostgreSQL `timestamptz::text` is normally rendered with a space between date and time, e.g. `2026-06-29 12:00:00+00`, not RFC3339 `2026-06-29T12:00:00Z`. `parse_rfc3339_unix` requires `split_once('T')`, so live `cursor_age_secs` will be `None` for the normal DB value. For a cursor behind the verified producer head, `None` is classified as `CatchingUp`, not `StalledCursor`.

Impact: the watchdog has the exact false-green the milestone forbids: a site cursor can be stuck behind the producer head for longer than the stall threshold and still report healthy/non-alerting `watchdog.catching_up`.

Fix: avoid string parsing here. Query a numeric age/epoch from PostgreSQL, or extend the syncd status API to return a typed timestamp/epoch. If string parsing remains, add coverage for the actual `applied_at::text` format and fail closed when a behind cursor has an unparseable `applied_at`.

### BLOCKER: the committed fixture artifact is missing, so demo/CI cannot consume the documented fixture corpus

The fixture contract says the static signed artifact is committed under `crates/jurisearch-deploy/fixtures/published/` and CI consumes that committed tree:

- `crates/jurisearch-deploy/fixtures/README.md:25`
- `crates/jurisearch-deploy/fixtures/README.md:50`
- `crates/jurisearch-deploy/src/ops/fixture.rs:42`

In this diff, `crates/jurisearch-deploy/fixtures/` contains only `README.md`; there is no `published/core/manifest.json` or package payload. That means the default fixture-backed demo path cannot actually apply the stable fixture corpus from the repository. The live acceptance script then invokes `demo up` and `demo smoke` as real legs:

- `crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:68`
- `crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:74`

There is also no fixture-specific preflight that turns the missing artifact into a clear diagnostic before the operator falls into generic catch-up/readiness failures. The generator helper is useful and uses the real package-build APIs, but generator + constants + docs are not enough for a milestone that claims a tiny signed fixture corpus and a demo/CI path that consumes committed bytes.

Fix: commit the generated `fixtures/published/core/...` tree, or change the milestone/runbook contract so the live demo is explicitly unavailable until fixture generation is performed. If the bytes remain deferred, add a direct guard such as `fixture published artifact missing: expected crates/jurisearch-deploy/fixtures/published/core/manifest.json; run <documented generation command>` before `demo up`/fixture smoke/catch-up proceeds.

### WARN: negative smoke classifiers accept non-contract responses as passing

The missing-id negative leg is supposed to prove not-found. Instead, it passes any served error, regardless of `ErrorObject.code`, and even passes a successful response that returns unrelated documents:

- `crates/jurisearch-deploy/src/ops/smoke.rs:361`
- `crates/jurisearch-deploy/src/ops/smoke.rs:364`

The bad-query leg similarly passes any served error:

- `crates/jurisearch-deploy/src/ops/smoke.rs:380`

`ErrorObject` has explicit machine codes including `BadInput`, `NoResults`, and `Internal`:

- `crates/jurisearch-core/src/error.rs:6`

Impact: these negative checks can go green on server-side internal errors or malformed fetch behavior. That weakens the "missing id -> not-found; bad query handled" gate into "the server returned any JSON error" and leaves a false-green hole.

Fix: for missing-id, pass only an empty `documents` array or a served `NoResults`/documented not-found code if the site contract emits one; fail non-empty `documents` unless the response shape has a first-class not-found signal. For bad-query, require `ErrorCode::BadInput` or a clean empty result shape that the site contract explicitly permits; fail `Internal`, `DependencyUnavailable`, and unrelated errors.

### WARN: `demo up` is only an alias for `site install`

The CLI documents `demo up` as "provision + trust + catch-up the fixture corpus + gated start", but the implementation directly delegates to `run_install`:

- `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:276`

`run_install` provisions, renders, bootstraps trust, starts prerequisite units, then checks readiness; it does not synchronously run `catch_up_corpus` for the fixture before the readiness gate. This leaves the demo dependent on asynchronous syncd timing and does not implement the documented "catch-up fixture" step.

Fix: either make `demo up` an explicit sequence that runs fixture catch-up before gated site start, or update the command/runbook text so it does not claim a synchronous fixture catch-up.

## Audit Notes

- Scope is confined to `Cargo.lock`, `crates/jurisearch-deploy/**`, and docs under `work/09-jurisearch-cli/**`; no source edits to `jurisearch-client`, `jurisearch-producer`, `jurisearch-package-build`, or `jurisearch-storage`.
- The thin-client cone itself was not edited. `jurisearch-deploy` newly depends on `jurisearch-client`, which does not add dependencies to `jurisearch-client`.
- The smoke live path does use the real thin client and real site protocol requests for status/fetch/search (`run_smoke` sends `send_request` per leg), so the demo/site smoke legs are not mocked.
- The no-silent-skip invariant is asserted before emitting CLI smoke output, and `run_smoke` currently pushes all six legs with only hybrid skip behavior. The invariant itself does not prove completeness or "hybrid-only skip" for arbitrary `SmokeReport`s, but the CLI path builds reports through `run_smoke`.
- The watchdog path does not call `run_catchup` or apply packages; aside from opening a writer handle for read primitives, I did not find a mutating watchdog call.
- I did not rerun the reported validation commands; this was a source review against `git diff main`.

VERDICT: FIXES_REQUIRED
