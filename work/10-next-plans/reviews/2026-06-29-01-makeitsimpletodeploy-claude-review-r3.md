# Re-review (r3): `work/10-next-plans/01-makeitsimpletodeploy.md`

Reviewer: Claude (Opus 4.8)
Date: 2026-06-29
Artifact: implementation-plan document "Make JuriSearch simple to deploy" (revision r3)
Scope: document/plan review (not a code review). Re-verified independently against the work/09 runtime,
the checked-in systemd units, and the CLI/syncd/client/storage/embed source.

## Summary

The r3 revision resolves all four r2 focus items, and it does so substantively (I re-verified each
against source, not against the prior summary):

- **Explicit `site provision-db`** is now in the headline Site-host happy path (line 27), and the
  Phase 3/Phase 4 ownership split is stated outright — `provision-db` owns DB creation/extensions/roles/
  migrations, `install` owns derived files and systemd lifecycle (lines 39-40; Phase 4 lines 345-347).
  This closes r2's WARN-1 (DB-provisioning omission/ambiguity).
- **Embedder fingerprint derivation**: the hand-typed `fingerprint = "..."` field is gone; the schema now
  carries `provider`, `base_url`, `model_name`, `dimension`, `normalize`, `pooling`, `model_path`,
  `tokenizer_json`, `port` (lines 184-194), and Phase 6 pins the comparison to the embedder's
  `storage_embedding_fingerprint()` vs `corpus_state.embedding_fingerprint` (lines 409-410). This closes
  the structural half of r2's WARN-2.
- **normalize/pooling modeled**: both are first-class fields (lines 189-190), rendered to
  `JURISEARCH_EMBED_NORMALIZE`/`JURISEARCH_EMBED_POOLING`, and the golden render pins them (line 262).
- **admin/bootstrap DB identity**: `[database]` now models `admin_user`/`admin_database` (lines 157-158),
  with a Phase 1 validation rule requiring them (lines 248-249) and Phase 2/Phase 3 consistently routing
  reachability and migration execution through the admin/bootstrap identity (lines 283, 286-287, 311,
  321-322). This closes r2's NIT-2.

Other r2 items also hold: redundant `dimension` is gone and `base_url`/`port` now have a cross-field
consistency rule (lines 253-254, closing r2 NIT-1); the Phase 2/Phase 3 migration-identity disagreement
from the original review is reconciled (writer no longer "applies migrations"); no stale schema wording or
non-ASCII remains (independently re-scanned).

One residual issue remains, and it is squarely inside the prompt's named focus area (embedder fingerprint
derivation, normalize/pooling). It is source-falsifiable and carries a narrow false-green consequence, so
it is a WARN rather than cosmetic. The fix is one or two lines of prose plus a small rendering
clarification. Details below.

---

## 1. Findings (ordered by severity)

### WARN-1 — The plan claims the *storage* fingerprint is derived from `pooling`; the code's `storage_embedding_fingerprint()` excludes pooling, so the Phase 6 fingerprint check cannot detect a pooling mismatch (a narrow false-green)

Sections: "Generated runtime files" lines 223-225 ("The effective storage fingerprint is derived by the
embedder from the explicit model, dimension, normalization, **and pooling** settings"); Phase 1 golden
invariant line 262 ("the full `JURISEARCH_EMBED_*` / `JURISEARCH_BGE_M3_*` blocks, including
normalize/pooling when rendered"); Phase 6 sub-check lines 409-410 ("the effective fingerprint computed by
the embedder's `storage_embedding_fingerprint()` matches the `corpus_state.embedding_fingerprint` package
value").

Source contradicts the line 225 claim. `EmbeddingFingerprint::storage_embedding_fingerprint()` is:

```rust
// crates/jurisearch-embed/src/fingerprint.rs:16-22
format!("{}:{}:normalize:{}", self.model, self.dimension, self.normalize)
```

i.e. `<model>:<dimension>:normalize:<bool>` — it contains **only** model, dimension, and normalize.
Pooling (and provider, and base_url_class) are members of the broader `EmbeddingFingerprint` struct and of
`EmbeddingConfig::fingerprint()` (`crates/jurisearch-embed/src/config.rs`), but they are **not** part of
the storage form. `JURISEARCH_EMBED_POOLING` does set `embedding_config.pooling`
(`crates/jurisearch-cli/src/embedding_runtime/config.rs:284-286`), which feeds the full fingerprint — but
not the storage fingerprint that Phase 6 compares against the corpus.

Consequences:

1. **Factually wrong prose.** Line 225 says the *storage* fingerprint is derived from pooling. It is not.
   The qualifier "storage" makes the statement falsifiable, and it is false. (Provider/base_url_class are
   likewise excluded, which is fine to exclude — but then pooling should not be singled in.)
2. **Implied-but-undeliverable check / false-green.** Phase 6 correctly pins the comparison to
   `storage_embedding_fingerprint()` (line 410), so an implementer who follows Phase 6 literally writes a
   *correct* check — one that silently does not cover pooling. Combined with line 225's claim that pooling
   is part of the fingerprint identity, an operator who configures a pooling that differs from the pooling
   the corpus was embedded with gets green `embed doctor`/`site readiness` **and** wrong dense-search
   results, with nothing surfacing the mismatch. That is exactly the false-green class this review series
   is meant to gate. (Note this is partly a work/09 design property: `corpus_state.embedding_fingerprint`
   is also the storage form, so neither side encodes pooling — the plan cannot rely on the storage
   fingerprint to validate pooling and should not imply that it does.)
3. **Rendering gap for non-default pooling.** The golden invariant (line 262) says the rendered
   `JURISEARCH_BGE_M3_*` block pins pooling, but the checked-in `deploy/systemd/jurisearch-bge-m3.service`
   hardcodes `--pooling cls` in `ExecStart` and carries only `JURISEARCH_BGE_M3_MODEL`/`_PORT` as env —
   pooling is **not** an env var on the server side. So for any `pooling != "cls"`, rendering the env
   block alone does not change the server's actual pooling; the renderer would also have to rewrite the
   `--pooling` flag in the generated unit. The plan does not state this.

Recommended fix (small): correct line 225 to state the storage fingerprint is derived from
`model`, `dimension`, and `normalize` only (matching `storage_embedding_fingerprint()`), and add one
sentence that `pooling` (and `provider`) configure the embedder/server but are **not** part of the
storage-fingerprint comparison — so a pooling mismatch is operator-asserted, not fingerprint-guarded.
If pooling is meant to be operator-controllable (the schema offers it), state that rendering threads it
into the generated bge-m3 unit's `--pooling` flag, not just the `JURISEARCH_EMBED_*` block, and consider
either dropping `pooling` from the schema (defaulting to `cls`, matching the unit) or adding an explicit
non-fingerprint pooling-consistency check. Keep Phase 6's `storage_embedding_fingerprint()` vs
`corpus_state.embedding_fingerprint` wording — that part is correct.

---

### NIT-1 — `site doctor` runs at happy-path step 3 (before trust bootstrap and catch-up); reconcile with Phase 2 "Done when ... a provisioned site exits zero"

Section: happy path line 28 (`site doctor` runs after `provision-db`, before `bootstrap-trust`/`catch-up`),
against Phase 2 checks (lines 288, 291-292) and "Done when" (lines 297-298).

At step 3 the DB is provisioned but trust anchors are not yet installed and no corpus is active. Phase 2
already treats the readiness check as advisory ("not yet caught up" with the exact command, lines
291-292), and the trust check most naturally reads as "present in config / token parseable" (line 288) —
so doctor *should* be able to proceed. But Phase 2's "Done when" says doctor "on a provisioned site exits
zero" without distinguishing "DB-provisioned" from "fully bootstrapped." It would help to state explicitly
that doctor at this position reports advisory not-yet-bootstrapped status (trust not installed, no active
corpus) without a non-zero exit, so the happy-path placement is unambiguous.

### NIT-2 — `site doctor` (step 3) and `embed doctor` (step 8) both transiently bind the loopback port that the bge-m3 systemd unit will own (step 9)

Sections: happy path lines 28, 33, 34; Phase 2 line 290 and Phase 6 line 410 ("can be started or reached
on loopback").

Both doctors may start `llama-server` on the configured loopback port to probe dimension/fingerprint,
and then `systemctl enable --now jurisearch-bge-m3` (line 34) binds the same port. If a doctor leaves its
probe endpoint running, the unit fails to bind. The plan should state that any doctor-started endpoint is
released before exit (or is skipped when the managed unit is already active). Minor, but it is on the
happy path twice.

### NIT-3 — The pre-serve gate names only `site readiness`; `embed doctor` is sequenced before serving but not stated as a gate

Section: lines 42-43 ("The query service must not be started for clients until `site readiness` exits zero
...") vs the happy path placing `embed doctor` (line 33) before `systemctl enable --now jurisearch-site`
(line 35).

The sequence is right, and the Phase 8 hybrid leg (now a hard failure) is a backstop. But the stated rule
gates serving on `readiness` alone, so an operator could pass readiness, skip/ignore a failing
`embed doctor`, and still start the site. If embedder health is intended to gate serving for an embedder-
configured site, say so; otherwise it is fine to rely on smoke as the end-to-end catch.

---

## 2. Open questions / residual risks

- **Pooling has no fingerprint guard on either side (work/09 property).** Because both
  `storage_embedding_fingerprint()` and `corpus_state.embedding_fingerprint` use the storage form
  (model:dimension:normalize), no fingerprint comparison can detect a pooling mismatch. This is larger
  than the prose fix in WARN-1; worth one line acknowledging the limitation so the deploy story does not
  over-promise embedder/corpus equivalence.
- **Admin/bootstrap authentication mechanism still unstated.** `[database]` now names `admin_user`/
  `admin_database`, and secrets are kept in a separate `0600` file (lines 197-198), but the plan still
  does not say how `provision-db` authenticates the privileged identity (password file, peer/ident,
  `.pgpass`, systemd credential). One line would make Phase 3 unambiguous. (Carried from r2 NIT-2; the
  identity is now modeled, the auth path is not.)
- **Demo coverage.** The demo example still exercises only `status` (lines 64-65). Since "this is not a
  fake in-memory mode" and the goal is "proving the product," state whether `demo up` applies at least one
  corpus so a `search`/`fetch` leg is demonstrable, or that the demo proves transport + status only.
  (Carried from r2.)
- **`embed doctor` after `catch-up`.** Placing `embed doctor` (step 8) after `catch-up`/`readiness` means
  an active corpus exists, so the Phase 6 fingerprint sub-check runs rather than degrading — good. Just
  confirm the temp-endpoint/port concern in NIT-2 does not collide with the about-to-be-enabled unit.

## 3. Verification notes (what I inspected)

- The artifact in full: `work/10-next-plans/01-makeitsimpletodeploy.md` (r3).
- Prior reviews for issue context: `.../2026-06-29-01-makeitsimpletodeploy-claude-review.md` and
  `...-review-r2.md`.
- Embedder fingerprint (WARN-1): `crates/jurisearch-embed/src/fingerprint.rs` (`storage_embedding_fingerprint`
  = `"{model}:{dimension}:normalize:{normalize}"`, pooling absent), `crates/jurisearch-embed/src/config.rs`
  (`EmbeddingFingerprint` / `fingerprint()` include provider, base_url_class, model, dimension, normalize,
  pooling), `crates/jurisearch-cli/src/embedding_runtime/config.rs` (the `JURISEARCH_EMBED_PROVIDER/
  BASE_URL/MODEL/DIMENSION/NORMALIZE/POOLING/TOKENIZER_JSON` family; `JURISEARCH_EMBED_POOLING` sets
  `embedding_config.pooling`), and `deploy/systemd/jurisearch-bge-m3.service` (`--pooling cls` hardcoded in
  `ExecStart`; env carries only `JURISEARCH_BGE_M3_MODEL`/`_PORT`).
- Migrations/provisioning (r2 WARN-4/WARN-1): `crates/jurisearch-storage/src/migrations.rs`
  (`run_migrations` is a method on `ManagedPostgres`), `crates/jurisearch-syncd/src/main.rs` (shared-server
  mode = "no `pg_ctl`, no migrations"; self-managed runs `postgres.run_migrations()`). Confirms external-PG
  migration is genuinely new capability, as Phase 3 states, and that the happy path needs the explicit
  `provision-db` step it now has.
- Thin client / demo (r2 WARN-5): `crates/jurisearch-client/src/lib.rs` (`LOCAL_SOCKET_NAME =
  "jurisearch-site.sock"`, `resolve_endpoint`, `--local`/`--server` mutually exclusive),
  `crates/jurisearch-client/src/main.rs` (`--local` doc). Confirms the plan's
  `$XDG_RUNTIME_DIR/jurisearch-site.sock` path and the `demo url` + `--server` fix are accurate.
- Trust/license model (r2 BLOCKER-2): re-confirmed the `[[trust.anchor]]` array + purpose discriminant and
  the "license anchor required when `[license]` is set" rule (config lines 167-179, prose 200-203,
  validation 250-251) match the package/license purpose split in the syncd/storage trust code.
- Mechanical scans: `LC_ALL=C rg -n '[^ -~]' ...` (no non-ASCII) and a stale-schema scan for
  `trust.package` / `fingerprint =` / `:cls:` / `normalize=true"` (no matches) — the old hand-typed
  fingerprint example and single-table trust form are fully gone.

I did not run code or tests (plan-only artifact). I did not edit any files.

Constraint check: the revision still honors the non-negotiables — no Kubernetes/Helm/Compose/HTTP/gRPC,
no internet exposure, no new client auth; the trusted-LAN/Tailscale boundary and the versioned JSONL site
protocol are preserved (Non-goals, lines 78-86; bind/`allow_lan` rules, lines 243-245). All findings above
are correctness/precision items within those constraints, not violations of them.

## 4. Disposition of prior (r2) findings

- r2 WARN-1 (happy path omits `provision-db` / install-vs-provision ambiguity): **resolved** — explicit
  `provision-db` step (line 27) and explicit ownership split (lines 39-40; Phase 4 lines 345-347).
- r2 WARN-2 (wrong fingerprint example; normalize/pooling unmodeled): **structurally resolved** — hand-typed
  fingerprint removed; normalize/pooling are explicit fields; Phase 6 pinned to
  `storage_embedding_fingerprint()`. **Residual:** the line 225 prose still mis-attributes pooling to the
  *storage* fingerprint, and the rendering of non-default pooling to the bge-m3 unit is unspecified — see
  WARN-1.
- r2 NIT-1 (redundant embedder fields can disagree): **resolved** — `dimension` no longer duplicated;
  `base_url`/`port` cross-field consistency rule added (lines 253-254).
- r2 NIT-2 (admin/bootstrap identity unmodeled): **resolved for identity** (admin_user/admin_database +
  validation + Phase 2/3 routing); auth *mechanism* still unstated (open question above).
- r2 open questions (demo coverage, Phase 6 independence, verified manifest head): Phase 6 independence is
  now correctly handled by placing `embed doctor` after catch-up; demo coverage still status-only (open).

VERDICT: FIXES_REQUIRED
