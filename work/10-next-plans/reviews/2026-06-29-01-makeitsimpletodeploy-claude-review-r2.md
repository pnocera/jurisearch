# Re-review (r2): `work/10-next-plans/01-makeitsimpletodeploy.md`

Reviewer: Claude (Opus 4.8)
Date: 2026-06-29
Artifact: implementation-plan document "Make JuriSearch simple to deploy" (revised)
Scope: document/plan review (not a code review). Re-verified independently against the work/09 runtime,
the checked-in systemd units, and the CLI/syncd/client/storage source.

## Summary

The revision resolves **every** finding from the prior review — both BLOCKERs, all six WARNs, and all
four NITs — and it does so substantively, not cosmetically. I re-verified the underlying source for each
claimed fix rather than trusting the prior summary; the resolutions hold against the code. In particular:
the headline "Site host" path now bootstraps trust → catch-up → readiness before `jurisearch-site` is
started (BLOCKER-1); the config now models a `[[trust.anchor]]` array with a `purpose` discriminant plus a
"license anchor required when `[license]` is set" rule (BLOCKER-2); `catch-up --wait` is now defined
against the verified manifest head sequence (WARN-1); `jurisearch-client` is in the server tarball
(WARN-3); the external-PG migration runner is named as new capability and owner-run (WARN-4); the demo
uses `demo url` + `--server` instead of `--local` (WARN-5); and the auto-skipped hybrid leg is now a hard
failure (WARN-6).

Two residual issues remain, both inside the review's stated focus areas (sequencing and embedder/env
rendering). Neither is a design contradiction of the work/09 boundary; both are correctness/precision gaps
in the operator path and the rendering spec. They are small to fix.

---

## 1. Findings (ordered by severity)

### WARN-1 (new) — The headline "Site host" happy path omits `site provision-db`; a fresh host cannot follow it

Section: "Target operator experience -> Site host" (lines 25-36), against "Phase 3 - Idempotent database
provisioning" (lines 293-322) and "Sequencing summary" (line 533, "The first useful milestone is phases
1-4").

The happy path is now correctly ordered for trust/data (`bootstrap-trust` -> `catch-up --wait` ->
`readiness` -> serve), which fixes the prior BLOCKER-1. But it never provisions the database. On a
genuinely fresh host the sequence breaks before it can succeed:

- `site doctor` (line 27) checks "PostgreSQL is reachable as the intended admin/owner, writer, and read
  identities," that `pgvector`/`pg_search` are installed, and that the roles exist (Phase 2, lines
  276-279). On a blank instance these checks fail — `doctor` cannot exit zero.
- Even if `doctor` is treated as advisory, `site catch-up --wait` (line 30) runs one-shot
  `jurisearch-syncd update` (Phase 5, line 369), which writes through the writer role into the migrated
  control/storage schema. With no database, roles, extensions, or migrations applied, the apply leg
  cannot run. I confirmed there is no implicit migration-on-apply path: in shared-server mode
  `jurisearch-syncd` deliberately does **not** migrate (`crates/jurisearch-syncd/src/main.rs::build_writer`),
  and `run_migrations` is a method on `ManagedPostgres` (`crates/jurisearch-storage/src/migrations.rs:1131`),
  i.e. it does not run against the operator's external PG by itself.

The plan does contain provisioning as Phase 3 and counts it in "milestone phases 1-4," so this is an
omission/under-specification in the canonical sequence, not a contradiction of the design — which is why
it is WARN rather than BLOCKER. There is also a plausible reading that `site install` performs it: line 38
says "`site install` is allowed to run all idempotent setup itself when privileges are available [...]
stop with the exact command or SQL file to run next." But Phase 4's explicit install-behavior list (lines
336-345) does **not** include database provisioning, and Phase 3 presents `provision-db` (with
`--dry-run-sql`) as a distinct operator command. So the document is ambiguous about who provisions the DB
on the happy path.

Recommended fix: pick one and state it. Either (a) insert `sudo jurisearchctl site provision-db --config
...` (or `--dry-run-sql` for a DBA) into the Site-host happy path between `doctor` and `install`, or (b)
state in Phase 4 that `site install` runs `provision-db` idempotently and stops with the exact SQL when a
privileged step (e.g. `CREATE EXTENSION`) is unavailable — and reconcile that with Phase 3 so the two
sections do not disagree.

### WARN-2 (residual of prior WARN-2) — `[embedder].fingerprint` example does not match the code's real fingerprint format, and `normalize`/`pooling` are not modeled, so the "full embed block" cannot be rendered deterministically for non-default embedders

Section: "Product decisions -> One config file `[embedder]`" (lines 180-190), "Generated runtime files"
(lines 215-219), Phase 1 invariant "Golden rendering pins [...] the full `JURISEARCH_EMBED_*` /
`JURISEARCH_BGE_M3_*` blocks" (lines 252-253), and Phase 6 sub-check "the effective fingerprint matches
the corpus package fingerprint" (lines 398-400).

The prior WARN-2 was largely addressed: `[embedder]` now carries `provider`, `base_url`, `model_name`, and
`dimension` explicitly, so the `JURISEARCH_EMBED_*` block no longer needs to be parsed out of the
fingerprint. Two precise problems remain, both source-verified:

1. The example `fingerprint = "bge-m3:1024:cls:normalize=true"` is not the format the code produces. The
   persisted fingerprint is `format!("{model}:{dimension}:normalize:{normalize}")` ->
   `bge-m3:1024:normalize:true` (`crates/jurisearch-embed/src/fingerprint.rs:17-22`,
   `EmbeddingFingerprint::storage_embedding_fingerprint`). The example invents a different shape (it
   inserts `cls`, and uses `normalize=true` rather than `:normalize:true`). If Phase 6 compares this TOML
   string against the corpus's stored `embedding_fingerprint` (the column returned by
   `corpus_status`, `crates/jurisearch-syncd/src/status.rs:19,50`), it will never match -> a false-red. If
   the field is treated as opaque (the plan says "without parsing operational meaning out of the
   fingerprint string," line 245), then it is unused config that still ships a wrong example.

2. `normalize` and `pooling` are first-class embedder parameters that feed the fingerprint and are
   independently configurable via `JURISEARCH_EMBED_NORMALIZE` / `JURISEARCH_EMBED_POOLING`
   (`crates/jurisearch-cli/src/embedding_runtime/config.rs:281-286`; the full fingerprint includes
   `normalize` and `pooling`, `crates/jurisearch-embed/src/config.rs:130-145`). The schema models neither.
   For the default bge-m3 (`normalize=true`, default pooling) the renderer happens to match because
   `serve-site` falls back to its built-in defaults — but the plan also advertises a general
   `provider`/`model_name`/`dimension` surface, and for any embedder whose `normalize`/`pooling` differ
   from the default, the "deterministic golden render of the full `JURISEARCH_EMBED_*` block" cannot
   reproduce the operator's intent (the values live only inside the opaque fingerprint string the plan
   refuses to parse).

Recommended fix: drop the hand-typed `fingerprint` field in favor of derivation from the explicit fields,
or fix its example to the real format (`<model>:<dimension>:normalize:<bool>`) and add discrete
`normalize` (and, if it must be controllable, `pooling`) fields. Then define the Phase 6 comparison in
code terms: "effective fingerprint" = `serve-site`'s computed `storage_embedding_fingerprint()`; "corpus
package fingerprint" = `corpus_state.embedding_fingerprint`. Keep the golden test asserting the full
`JURISEARCH_EMBED_*` block including whichever of `NORMALIZE`/`POOLING` the schema renders.

---

### NIT-1 — Redundant embedder fields can silently disagree

`[embedder]` now encodes the dimension twice (`dimension = 1024` and inside `fingerprint`) and the port
twice (`base_url = "http://127.0.0.1:8081"` and `port = 8081`). The validation rule (lines 243-245) asks
for fields "internally consistent enough to render both env families" but does not say the renderer
*checks* that the fingerprint's dimension equals `dimension`, or that `base_url`'s host:port equals
loopback:`port`. With WARN-2's fix this mostly dissolves (fewer redundant sources of truth); if any
redundancy is kept, add an explicit cross-field consistency check so a mismatched pair fails validation
rather than rendering an inconsistent unit.

### NIT-2 — `provision-db` admin/superuser connection identity is unmodeled in `[database]`

`[database]` lists role *names* (`writer_user`, `read_user`, `owner_role`) but no admin/owner connection
identity, while Phase 2 checks reachability "as the intended admin/owner" (line 276) and Phase 3 runs
migrations "as the owner/admin identity" (line 311) and may emit a DBA command for `CREATE EXTENSION`
(lines 309-310). The plan keeps passwords in a separate `0600` file (lines 192-194), which is good, but it
never says how `provision-db` authenticates the privileged role used to create roles/run DDL. Worth one
line pinning the bootstrap identity so Phase 3 is unambiguous.

---

## 2. Open questions / residual risks

- Demo coverage: the demo example (lines 60-65) exercises only `status`. Since "this is not a fake
  in-memory mode" and the goal is "proving the product," consider whether `demo up` is expected to apply
  at least one corpus so a `search`/`fetch` leg is demonstrable, or state that the demo proves transport +
  status only.
- Phase 6 independence vs Phase 5: the plan now correctly degrades the fingerprint sub-check to "no active
  corpus to compare" before catch-up (lines 398-400). Confirm the same applies when `embed doctor` runs in
  the happy path (line 32) *after* `catch-up`/`readiness` but *before* the bge-m3 service is enabled — i.e.
  that `embed doctor` starting the endpoint itself (Phase 6 "can be started or reached," line 397) is the
  intended behavior and does not collide with the about-to-be-enabled systemd unit.
- "Manifest head sequence" (line 371): the plan relies on a verified producer-manifest head sequence to
  define caught-up. `fetch_verify_manifest` returns the signed `RemoteManifest`
  (`crates/jurisearch-syncd/src/planner.rs:413-427`) and `corpus_status.sequence` is the local cursor, so
  the comparison is implementable; just ensure the implementation reads the head sequence from the
  *verified* payload, never from an unverified manifest field.

## 3. Verification notes (what I inspected)

- The artifact in full: `work/10-next-plans/01-makeitsimpletodeploy.md`.
- Prior review for issue context: `.../reviews/2026-06-29-01-makeitsimpletodeploy-claude-review.md`.
- Trust/license (BLOCKER-2): `crates/jurisearch-syncd/src/trust.rs` (`install_verified_license_token` and
  `check_entitlement` both call `build_verifier(client, LICENSE_PURPOSE)`), `jurisearch-storage/src/trust.rs`
  (`PACKAGE_PURPOSE`/`LICENSE_PURPOSE`, `install_trust_anchor(... purpose)`). Confirms the new
  `[[trust.anchor]]` array with `purpose` and the "license anchor required" rule is necessary and
  implementable, and that anchors-before-token ordering in Phase 5 is correct.
- Migrations/provisioning (WARN-1, WARN-4): `crates/jurisearch-storage/src/migrations.rs`
  (`run_migrations` is on `ManagedPostgres`, applies via `execute_sql`/`psql`), `backend.rs`
  (`ConnectionConfig::connect`, `provision_roles` over a connection), `jurisearch-syncd/src/main.rs`
  (`build_writer` does not migrate in shared-server mode). Confirms external-PG migration is genuinely new
  capability and that the happy path needs an explicit provisioning step or an install that performs it.
- Catch-up/readiness (WARN-1 prior): `crates/jurisearch-syncd/src/status.rs` (`corpus_status` reads only
  `jurisearch_control.corpus_state`; it does not contact the producer) and
  `crates/jurisearch-syncd/src/planner.rs` (`fetch_verify_manifest`, `DirectoryCatchupSource` reads
  `<root>/<corpus>/manifest.json`). Confirms `catch-up --wait` correctly relies on the verified manifest
  head, not `status --json`.
- Bind translation (NIT-3 prior): `crates/jurisearch-cli/src/args.rs` (`ServeSiteArgs`: `--socket <path>`,
  `--tcp <host:port>` bare, `--allow-lan`, `--allow-wildcard-lan`). Confirms the documented
  `tcp://`/`unix://` -> flag translation is accurate.
- Thin client (WARN-5, NIT-1/2 prior): `crates/jurisearch-client/src/lib.rs` (`resolve_endpoint`:
  `--local` resolves `$XDG_RUNTIME_DIR/<LOCAL_SOCKET_NAME>`, then `--server`, then `JURISEARCH_SITE_URL`;
  `parse_endpoint` requires explicit `tcp://`/`unix:///absolute`). Confirms the demo fix and the
  resolution-order wording.
- Embedder (WARN-2): `crates/jurisearch-cli/src/embedding_runtime/mod.rs` (`PreparedQueryEmbedder::from_env`),
  `.../config.rs` (`JURISEARCH_EMBED_PROVIDER/BASE_URL/MODEL/DIMENSION/NORMALIZE/POOLING/TOKENIZER_JSON`),
  `crates/jurisearch-embed/src/config.rs` (`EmbeddingConfig`, `fingerprint()` includes provider, base-url
  class, model, dimension, normalize, pooling), `crates/jurisearch-embed/src/fingerprint.rs`
  (`storage_embedding_fingerprint` = `"<model>:<dimension>:normalize:<bool>"`). Confirms the example
  fingerprint format is wrong and that `normalize`/`pooling` are unmodeled.

I did not run code or tests (plan-only artifact). I did not edit any files.

Constraint check: the revision still honors the non-negotiables — no Kubernetes/Helm/Compose/HTTP/gRPC,
no internet exposure, no new client auth; the trusted-LAN/Tailscale boundary and the versioned JSONL site
protocol are preserved (Non-goals, lines 76-84; bind/`allow_lan` rules, lines 237-238). The findings above
are correctness/sequencing/rendering-precision items within those constraints, not violations of them.

## 4. Disposition of prior findings (all resolved)

- BLOCKER-1 (serve-before-readiness): resolved. Happy path now runs `bootstrap-trust` -> `catch-up --wait`
  -> `readiness` before `systemctl enable --now jurisearch-site` (lines 25-36), and the rule is stated
  explicitly (lines 40-42) and in Phase 4 (lines 343-344) and Phase 5 "Done when" (lines 380-381).
- BLOCKER-2 (license anchor unmodeled): resolved. `[[trust.anchor]]` array with `purpose`
  (lines 163-176), prose at lines 196-198, validation rule at line 242.
- WARN-1 (catch-up head ambiguity): resolved (lines 370-372, invariant line 381).
- WARN-2 (embed render determinism): largely resolved; residual precision issues above.
- WARN-3 (client missing from server tarball): resolved (line 469).
- WARN-4 (migrations on external PG): resolved (lines 296-303, 311, 319; Phase 2 line 277 reconciled).
- WARN-5 (demo `--local` mismatch): resolved (lines 60-71).
- WARN-6 (silent hybrid skip): resolved (lines 447-448, 454).
- NIT-1..4 (parser restructure, resolution order, bind scheme, log-absence check): resolved (lines
  417-420, 215-217, 442-444).
- Prior open questions (extension privilege, Phase 6 degrade, anchor provenance, uninstall data-safety):
  all addressed (lines 309-310, 398-400, 196-198, 352-353).

VERDICT: FIXES_REQUIRED
