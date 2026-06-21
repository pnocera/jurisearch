# Claude Review — `cite --online` Légifrance Probe Wiring (Phase 1.4)

Reviewed: uncommitted Phase 1.4 `cite --online` wiring — CLI dependency on
`jurisearch-official-api` (`Cargo.toml`, `Cargo.lock`), shared-client OAuth + search
probe driven from `cite_payload` (`cli/src/main.rs`: `apply_online_citation_confirmation`,
`summarize_online_response`), upstream-error→exit-`5` mapping, online response metadata,
and CLI contract tests (`cli/tests/cli_contract.rs`: success probe + failing-server exit
`5` + `spawn_server` harness). Also re-checked the parser/test work landed for the prior
GO review's findings (L/R/D + `LO` article prefixes, code-name hint table, canonical NOR
shape, ingest-backed free-text cite test) and the `IMPLEMENTATION_PLAN.md` status update.
Reviewer: Claude (Opus 4.8), 2026-06-21.

The slice is correct, lint-clean, and well-tested. OAuth client-credentials acquisition,
the Bearer search call, and upstream-failure mapping route through the already-reviewed
`PisteClient`; errors land on `ErrorCode::Upstream` → `ProcessExit::Upstream` = exit `5`,
asserted end-to-end. The four prior-review parser findings are now addressed and covered.
Findings below are cleanliness/semantics/limitation notes, not blockers.

## What I verified

- **Build/lints/tests green.** `cargo clippy -p jurisearch-cli -p jurisearch-official-api
  --tests`: no warnings. `cargo test -p jurisearch-official-api`: 10/10 pass.
  `cargo test -p jurisearch-cli --test cli_contract cite_`: both
  `cite_resolves_local_statutory_citations_and_strict_states` and
  `cite_free_text_matches_ingested_legi_article_citation` pass (Postgres present; they
  self-skip via `discover_pg_config` otherwise).
- **Exit-code path.** `OfficialApiError::UpstreamStatus`/`Transport`/`InvalidResponse` →
  `to_error_object()` → `ErrorCode::Upstream` → `ProcessExit::Upstream = 5`
  (`core/src/error.rs`). The failing-server test asserts `.code(5)`, empty stderr, and a
  JSON error body containing `"official API returned HTTP status 500"`. `MissingCredential`
  deliberately maps to `DependencyUnavailable` → exit `4` instead, which is the right
  distinction (local config gap vs remote failure).
- **Probe wiring.** `apply_online_citation_confirmation` runs only under `--online`, after
  local classification; on success it overwrites `response["online"]` with
  `requested/checked/provider/state/response_summary/note`. The success test confirms
  `online.checked=true`, `provider="legifrance"`, the OAuth body
  (`grant_type=client_credentials`, `scope=openid`), the `Authorization: Bearer …` header
  on the search request, and the raw cite text as the `query`.
- **Parser follow-ups (prior review #1–#4).** `parse_article_number` now absorbs a
  separated `L`/`LO`/`R`/`D` prefix (`Code de la consommation article L. 121-1` →
  `l121-1`, tested); `detect_code_hint` carries an 11-entry code-name table disambiguating
  `code civil` vs `code des assurances` for the same article number (both tested);
  `looks_like_nor` is tightened to the canonical 4-alpha/7-digit/1-alpha shape; and
  `cite_free_text_matches_ingested_legi_article_citation` drives the **real** LEGI archive
  ingest pipeline then cites it, locking the citation/title text coupling against actual
  parser output rather than a fabricated string.
- **Plan honesty.** The `IMPLEMENTATION_PLAN.md` flip from "Remaining: wire `cite --online`"
  to "Done … probe … exit `5`" + a new "Remaining: enrich from reachability/probe to
  source-of-truth confirmation" accurately describes what shipped and what is deferred.

## Findings

1. **(Medium — semantics) `--online` makes upstream a hard dependency even when local
   resolution already succeeds.** The probe runs unconditionally under `--online`, and any
   upstream error propagates via `?` (exit `5`) *before* the response is returned. So a
   citation that resolves locally to `exact`/`normalized` now returns no answer at all when
   Légifrance is down — the local result is discarded. There is no best-effort/degraded
   mode (e.g. `online.checked=false` + a reachability error note while still emitting the
   local resolution). Defensible as a deliberate "probe must succeed" contract, but it is a
   behavioral change worth an explicit decision: `--online` converts upstream availability
   into a gating dependency for otherwise-locally-answerable citations. It also reorders
   `--strict --online` failure precedence (upstream `5` wins over strict `2`).

2. **(Medium — placeholder, partially disclosed) The probe request body is not the real
   Légifrance search schema.** `apply_online_citation_confirmation` sends
   `{"query": <raw cite>, "pageSize": 1}` to `/dila/legifrance/lf-engine-app/search`. The
   real `lf-engine-app` search endpoint expects a structured `recherche` envelope, so this
   payload will likely be rejected (4xx) by the production API — meaning `--online` against
   real Légifrance would currently tend to exit `5` rather than confirm anything. The plan's
   "Remaining" note defers the *response* shape but does not call out that the *request*
   shape is also a placeholder; the mock servers accept any body, so tests can't surface
   this. Within the disclosed deferral spirit, but the request-shape gap should be named
   explicitly so it isn't mistaken for a working production path.

3. **(Low — dead/misleading source) The pre-probe `online` sub-object is unreachable when
   online is requested.** In `cite_payload` the initial `response.online` builds
   `checked:false` plus the stale note *"Online Légifrance confirmation is not wired yet…"*
   and `state:"source_unavailable"` guarded on `if args.online`. But whenever `args.online`
   is true that object is immediately overwritten by `apply_online_citation_confirmation`
   (success) or the whole response is discarded (error), so those `Some(...)` arms are
   computed and thrown away, and the "not wired yet" text now contradicts the shipped
   behavior. Simplify to build the inert object only for the non-online branch (or drop the
   stale note), so the source reflects reality.

4. **(Low — by design, pre-existing) `state:"source_unavailable"` is emitted even when the
   online probe succeeds.** For a local miss + `--online`, top-level `state` is
   `source_unavailable` while `online.checked=true` and the search returned (empty) results
   — i.e. the source *was* reachable. This inherits the prior slice's local-only state logic
   (prior review Info #5) and is explained by the `online.note`, but the field naming reads
   as a contradiction once a real probe runs. It resolves naturally once response-shape
   matching (the deferred work) lets the online result feed the state.

5. **(Low — efficiency) Unconditional probe + per-call client.** The probe fires even for
   `Malformed` citations (a junk query still triggers OAuth + a network round-trip), and a
   fresh `PisteClient` is built per `cite` call, so the token cache never survives across
   calls — every `--online` cite in a JSONL session re-authenticates. Both are minor;
   short-circuiting the malformed case and/or reusing a client in session mode would help.

6. **(Nit) Test-harness duplication.** `spawn_server`/`read_http_request`/
   `request_is_complete`/`ok_json` in `cli_contract.rs` are byte-for-byte copies of the
   helpers in `official-api`'s test module. Cross-crate test code can't easily share, so
   this is acceptable, but a shared `dev-dependency` test-support crate would remove the
   drift risk if more HTTP-mocking tests land.

## Recommendations

- Decide and document the `--online` failure contract (Finding #1): keep hard-fail, or add
  a best-effort mode that still returns the local resolution with a reachability note.
- Add a one-line plan note that the probe **request** body is a placeholder, not just the
  response shape (Finding #2); pin it with a fixture/opt-in smoke test when the real schema
  is wired.
- Remove the now-unreachable `checked:false` / "not wired yet" online shell for the online
  branch (Finding #3) — pure cleanup, no behavior change.
- Optionally short-circuit the probe for `Malformed` inputs and reuse the client in session
  mode (Finding #5).

None of the above blocks merge: the dependency wiring is correct, the OAuth/search/error
path is exercised end-to-end (including the failing-server exit-`5` case), the prior
review's parser findings are resolved and tested (notably the ingest-backed free-text
test), clippy is clean, and the plan discloses the remaining probe→confirmation gap.

Verdict: GO
