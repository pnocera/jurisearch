# Claude Review: official API foundation

Verdict: GO

Scope reviewed: commit `7b30993` "Add official API client foundation".
Files: `Cargo.toml`, `Cargo.lock`, `crates/jurisearch-official-api/Cargo.toml`,
`crates/jurisearch-official-api/src/lib.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`.

Summary against the review focus:

- PISTE environment/base URL handling — correct. Production/sandbox API and OAuth
  hosts match PISTE conventions, env overrides handled.
- Judilibre `KeyId` auth — correct header, correct `/cassation/judilibre/v1.0/*`
  paths, covered by a mock test.
- Légifrance OAuth2 client-credentials — correct `/api/oauth/token` endpoint,
  `grant_type=client_credentials`, `scope=openid`, Bearer search against
  `/dila/legifrance/lf-engine-app/search`, with token cache + skew; fetch and reuse
  are covered by a mock test.
- Error mapping — 429 → `RateLimited` → `Upstream` (exit 5); other HTTP status /
  transport / bad JSON → `Upstream` (exit 5); missing credentials → `DependencyUnavailable`
  (exit 4). Consistent with the contract and the plan.
- Secret redaction — custom `Debug` on config and cached token; `PisteClient` `Debug`
  is safe by field composition; no logging anywhere; secrets travel only in headers/form,
  never in the URL or surfaced bodies.
- Tests/plan — mock-server tests pass (ran 5×, no flakiness), `clippy` clean, plan
  status section is accurate and honestly defers out-of-scope work.

No blocking findings. The remaining items below are low-severity / non-blocking.

## Findings

- **Low — `lib.rs:178-180`, `lib.rs:223-225`, `lib.rs:230-232` (missing-credential hint
  is prod-only).** The `MissingCredential { name }` is hardcoded to the production env
  names (`PISTE_API_KEY`, `PISTE_OAUTH_CLIENT_ID`, `PISTE_OAUTH_CLIENT_SECRET`) and is
  surfaced verbatim in the suggestion at `lib.rs:295-297`. In sandbox, `from_env`
  (`lib.rs:111-139`) actually reads `PISTE_SANDBOX_*` (or the unified `JURISEARCH_PISTE_*`)
  variables, so the suggestion points a sandbox user at a variable that will not be read.
  Rationale: misleading-but-harmless hint; the unified `JURISEARCH_PISTE_*` path still
  works. Non-blocking.

- **Low — `lib.rs:253-255` (token-lifetime arithmetic can panic).**
  `Instant::now() + Duration::from_secs(seconds).saturating_sub(LEGIFRANCE_TOKEN_SKEW)`
  will panic on `Instant` overflow if the OAuth response returns an absurd `expires_in`
  (e.g. near `u64::MAX`). Rationale: the token endpoint is trusted (PISTE), so this is a
  robustness nit rather than an exploitable path; `checked_add` would make it total.
  Non-blocking.

- **Low — `lib.rs:107-110` (empty base-URL env override is accepted).**
  `env::var(...).unwrap_or(config.api_base_url)` means a set-but-empty
  `JURISEARCH_PISTE_API_BASE_URL` / `..._OAUTH_BASE_URL` overrides the default with `""`,
  whereas credentials are guarded by `first_nonempty_env` (`lib.rs:352-356`). Inconsistent
  empty-string handling; an empty override would produce malformed request URLs. Non-blocking.

- **Low — `lib.rs:280-281` + `lib.rs:309-318` (raw upstream body in structured message).**
  `UpstreamStatus` includes the upstream response body in its `Display`, and
  `to_error_object` forwards it via `self.to_string()`. Unlike `RateLimited`
  (`lib.rs:299-308`), which deliberately drops `body` and only surfaces `retry_after`, a
  non-429 upstream error embeds the full upstream payload (potentially large or HTML) in
  the user-facing `ErrorObject.message`. No secret leak (PISTE responses don't echo the
  `KeyId`/`client_secret`), so this is a message-hygiene nit. Non-blocking.

## Suggestions

- Test coverage gaps worth filling in a follow-up (all currently green, but untested):
  - Légifrance missing `client_id` / `client_secret` → `MissingCredential`
    (`lib.rs:218-233`); only the Judilibre missing-key path is tested.
  - Token expiry-triggered refetch and the 30s skew (`lib.rs:253-255`, `lib.rs:345-350`);
    only the valid-token reuse path is exercised.
  - A non-429 upstream status (e.g. 500 → `UpstreamStatus`) and a transport failure;
    only 429 is exercised.
  - `from_env` precedence (unified vs sandbox/prod fallback, `lib.rs:96-141`).
  - `judilibre_transactional_history` (`lib.rs:168-170`) — thin wrapper, low risk.
- Strengthen `config_redacts_secrets_in_debug_output` (`lib.rs:400-411`) to also assert the
  absence of `client-id`; the `Debug` impl already redacts `legifrance_client_id`
  (`lib.rs:60-63`) but the test only checks `secret-key` and `client-secret`.
- Consider truncating the upstream body in the `UpstreamStatus` message and/or using
  `Instant::checked_add` for token expiry, per the Findings above.
- Optional: derive the `MissingCredential` `name` (or its suggestion) from the active
  environment so the sandbox hint names the variable that will actually be consulted.

## Verification Notes

- `git show --stat 7b30993` and `git show 7b30993 -- <manifests/plan>` — confirmed scope
  and diff content.
- `cargo test -p jurisearch-official-api` — 5 tests pass (redaction, KeyId header,
  Légifrance token fetch+reuse, 429 mapping, missing-credential mapping).
- Ran the test suite 5× consecutively — no flakiness, despite the keep-alive concern with
  the per-request mock listener (ureq reconnects cleanly; the reuse test's
  `request_count = 3` correctly pins "exactly one token fetch across two searches").
- `cargo clippy -p jurisearch-official-api --all-targets` — clean, no warnings.
- Cross-checked error mapping against `crates/jurisearch-core/src/error.rs`:
  `Upstream → ProcessExit::Upstream (5)`, `DependencyUnavailable → ProcessExit::Dependency (4)`
  — matches the plan's "process exit code 5" requirement for upstream failures.
- Confirmed `crates/jurisearch-official-api/Cargo.toml` declares exactly the deps used
  (`jurisearch-core`, `serde`, `serde_json`, `thiserror`, `ureq` with `json` feature) and
  that the new crate is registered in the workspace members list.
- Plan accuracy: the `0.8` status section (IMPLEMENTATION_PLAN.md) matches the code and
  correctly defers backoff/retry scheduling, keyring loading, CLI wiring, and Judilibre
  sandbox validation to later slices — i.e. the unimplemented "rate-limit handling,
  backoff, and retry policy" task line is not claimed as done.
