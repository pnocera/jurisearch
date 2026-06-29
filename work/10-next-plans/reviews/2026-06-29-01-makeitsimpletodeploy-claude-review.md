# Review: `work/10-next-plans/01-makeitsimpletodeploy.md`

Reviewer: Claude (Opus 4.8)
Date: 2026-06-29
Artifact: implementation-plan document "Make JuriSearch simple to deploy"
Scope: document/plan review (not a code review). Verified against the work/09 runtime, the checked-in
systemd units, and the CLI/syncd/client source.

The plan is well-aimed and largely faithful to the work/09 product boundary: it keeps the trusted-LAN /
Tailscale decision, the versioned JSONL site protocol, the local bge-m3 endpoint, and the
producer/syncd/serve-site/thin-client topology, and it correctly identifies the real operator pain
(config sprawl, manual unit copying, no DB-provisioning command, no readiness-gated acceptance). The
phase decomposition is sound and the `jurisearchctl` surface mostly lines up with the existing binaries.

The blocking problems are concentrated in two places: (1) the headline "happy path" starts and serves the
site before the trust/catch-up/readiness bootstrap that the plan's own Phase 5 (and the work/09 runbook)
require, which is exactly the false-green hole the review is meant to catch; and (2) the single config
schema cannot bootstrap the license token it promises to install, because license verification needs a
license-purpose trust anchor the schema does not model. Both are concrete, source-verified
contradictions. Details and fixes below.

---

## 1. Findings (ordered by severity)

### BLOCKER-1 — Headline "Site host" happy path serves the site before trust/catch-up/readiness; `site smoke` would false-fail or false-green

Section: "Target operator experience → Site host" (lines 25-31), contradicting "Phase 5" (lines 306-328)
and the work/09 runbook `work/09-jurisearch-cli/05-two-host-acceptance.md` (Prerequisites, steps 2-4).

The documented happy path is:

```
site init → site doctor → site install → systemctl enable --now (bge-m3, syncd, site) → site smoke --fetch-id <id>
```

This enables and starts `jurisearch-site` (and `jurisearch-syncd`) with **no** `bootstrap-trust`,
`catch-up`, or `readiness` step in between. That is inconsistent with the plan itself and with the repo:

- Phase 5 "Done when" (line 327) states readiness must be proven "**before** `jurisearch-site` is started
  for clients," and lists `bootstrap-trust` → `catch-up --wait` → `readiness` as the gating sequence.
- The work/09 two-host runbook (05-two-host-acceptance.md, Prerequisites step 3 then step 4) brings up
  `jurisearch-site` **last**, only after confirming `jurisearch-syncd` reached the producer head.
- On a genuinely fresh host the path cannot work: `jurisearch-syncd` started via `systemctl` will poll and
  reject every cycle (no package/license trust anchors installed yet — see BLOCKER-2 and
  `crates/jurisearch-syncd/src/main.rs` `Trust`/`Subscribe`), so no corpus becomes active, and
  `jurisearch-site` binds with an empty/unstamped DB. The final `site smoke --fetch-id '<known-id>'` then
  fails on the fetch leg (no such document) — or, if the smoke readiness check is weak, reports green
  against a site that cannot actually answer.

Recommended fix: rewrite the "Site host" happy path so the trust/data bootstrap precedes serving, and keep
`jurisearch-site` stopped until readiness passes, e.g.:

```sh
sudo jurisearchctl site init --config /etc/jurisearch/site.toml
sudo jurisearchctl site doctor --config /etc/jurisearch/site.toml
sudo jurisearchctl site install --config /etc/jurisearch/site.toml --no-start   # render+enable, do not start site
sudo jurisearchctl site bootstrap-trust --config /etc/jurisearch/site.toml      # package + license anchors, license token
sudo systemctl enable --now jurisearch-bge-m3 jurisearch-syncd                  # embedder + the single writer
sudo jurisearchctl site catch-up --config /etc/jurisearch/site.toml --wait      # apply >=1 corpus to producer head
sudo jurisearchctl site readiness --config /etc/jurisearch/site.toml            # prove active+stamped before serving
sudo systemctl enable --now jurisearch-site                                     # only now expose to clients
jurisearchctl site smoke --config /etc/jurisearch/site.toml --fetch-id '<known-id>'
```

State the rule explicitly (the plan already implies it in Phase 4's `--no-start` and Phase 5): `site
install` may render and enable units, but the query service must not be started until `site readiness`
exits zero.

### BLOCKER-2 — The single config schema cannot install/verify the license token; it models only a package trust anchor

Section: "Product decisions → One config file" (lines 153-160), driving "Phase 5 → bootstrap-trust"
(lines 315-316).

The config models exactly one trust anchor, `[trust.package]` (lines 153-158), plus `[license]
token_json` (lines 159-160). Phase 5 promises to "Install package trust anchors from config if absent"
and "Install license token if configured and absent." But installing a license token is verified, not
blind: `install_verified_license_token` builds a verifier from a **license-purpose** anchor
(`crates/jurisearch-syncd/src/trust.rs`: `build_verifier(client, LICENSE_PURPOSE)`), and the syncd CLI
exposes two distinct anchor purposes (`crates/jurisearch-syncd/src/main.rs`: `--purpose package|license`,
backed by `PACKAGE_PURPOSE`/`LICENSE_PURPOSE` in `jurisearch-storage/src/trust.rs`). With no
license-purpose anchor in the config, `bootstrap-trust` cannot verify or install the license token — the
documented Phase 5 flow is unimplementable from the "minimum shape" it presents.

A secondary defect of the same shape: a single `[trust.package]` table cannot express more than one key
during a key rotation (two epochs overlapping), yet Phase 5 line 323 promises "key rotation requires an
explicit command" — implying multiple anchors must be representable.

Recommended fix: replace the single `[trust.package]` table with an anchors **array** that carries a
`purpose` discriminant, and require at least one `package` and (when `[license]` is set) one `license`
anchor. For example:

```toml
[[trust.anchor]]
purpose = "package"
key_id = "producer-k1"
key_epoch = 1
public_key_hex = "<hex>"
algorithm = "ed25519"

[[trust.anchor]]
purpose = "license"
key_id = "license-k1"
key_epoch = 1
public_key_hex = "<hex>"
algorithm = "ed25519"

[license]
token_json = "/etc/jurisearch/license-token.json"
```

Add a Phase 1 validation rule: if `[license]` is configured, a `license`-purpose anchor must be present.

---

### WARN-1 — `catch-up --wait` "until status reports every corpus at producer head" is underspecified; `status --json` does not know the producer head

Section: "Phase 5 → Behavior" (lines 320-321).

`jurisearch-syncd status --json` reports only the client's local cursor authority — `corpus`,
`active_generation`, `sequence`, `applied_at`, etc. (`crates/jurisearch-syncd/src/main.rs::corpus_status`
/ `Command::Status`). It does **not** contact the producer, so it cannot by itself report "at producer
head." Determining head requires fetching and verifying the producer manifest (what `update`/`run` do via
`fetch_verify_manifest`) and comparing its head sequence to the cursor sequence. The work/09 runbook makes
this explicit by having the operator compare `status --json` against the separately-known producer head
(05-two-host-acceptance.md, Prerequisites step 3). As written, `--wait`'s green condition is ambiguous and
invites a false-green (e.g. treating "no error / non-empty status" as "caught up").

Recommended fix: specify that `catch-up --wait` reads the producer head from the verified manifest itself
and polls until `cursor.sequence == manifest.head_sequence` for every configured corpus (or a timeout),
and that the authoritative serve gate remains the `query_readiness` stamp checked by `site readiness`.

### WARN-2 — The `[embedder]` config does not carry the fields needed to render the site's embed env block deterministically

Section: "Product decisions → One config file `[embedder]`" (lines 162-168) and "Generated runtime files"
(lines 176-186); invariant "Rendering is deterministic / golden env files" (Phase 1, lines 201, 213).

`serve-site` builds its embedder from `PreparedQueryEmbedder::from_env()`, which reads the
`JURISEARCH_EMBED_*` family — the checked-in `deploy/systemd/jurisearch-site.service` shows the required
set: `JURISEARCH_EMBED_PROVIDER=openai_compatible`, `JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:<port>`,
`JURISEARCH_EMBED_MODEL=bge-m3` (the served model **name**, not the GGUF path),
`JURISEARCH_EMBED_DIMENSION=1024`, `JURISEARCH_EMBED_TOKENIZER_JSON=...`. The bge-m3 unit additionally
needs `JURISEARCH_BGE_M3_MODEL` (the GGUF path) and `JURISEARCH_BGE_M3_PORT`. The config's `[embedder]`
section provides `llama_server`, `model` (GGUF path), `tokenizer_json`, `port`, and a composite
`fingerprint = "bge-m3:1024:cls:normalize=true"` — but it does **not** directly provide the served model
name, the dimension, the provider, or the base URL. Rendering `site.env` deterministically therefore
depends on hardcoded defaults and on parsing the dimension/model out of the `fingerprint` string, which
the plan never specifies. This contradicts the "golden env files / deterministic rendering" invariant.

Recommended fix: either add explicit `dimension`, `model_name` (default `bge-m3`), and `provider`
(default `openai_compatible`) fields to `[embedder]`, or state the exact derivation from `fingerprint`
and `port` (and that `base_url` = `http://127.0.0.1:<port>`). Add a Phase 1 golden test asserting the full
rendered embed block of `site.env`, not just the bge-m3 unit.

### WARN-3 — `site doctor` and `site smoke` require `jurisearch-client` on the site host, but the Phase 9 server tarball omits it

Sections: "Phase 2 → Checks" (line 231, required binaries include `jurisearch-client`); "Phase 8 → Smoke
legs" (lines 385-388, status/fetch/search "through the thin client"); "Phase 9 → Deliverables" (line 407,
server tarball = `jurisearch`, `jurisearch-syncd`, `jurisearchctl`, templates, checksums).

Doctor lists `jurisearch-client` as a required binary on the site host, and the smoke legs explicitly
drive `jurisearch-client` locally on the site host (matching the work/09 `single-host-acceptance.sh`,
which builds and runs `jurisearch-client` on the same host). But the Phase 9 server bundle ships only
`jurisearch`, `jurisearch-syncd`, and `jurisearchctl`. A site installed purely from the server tarball
would fail `site doctor`'s binary check and could not run `site smoke`.

Recommended fix: add `jurisearch-client` to the server tarball (it is the artifact smoke depends on), or
have `jurisearchctl site smoke` use the in-process client library (`jurisearch-client` crate) for its
client legs and drop `jurisearch-client` from the doctor required-binary list for the site host. State
which one explicitly.

### WARN-4 — `provision-db` "Run storage migrations" against a shared/external PostgreSQL is new capability, not a work/09 "build-on"

Section: "Phase 3" — Goal/Builds-on (lines 252-255) and Responsibilities "Run storage migrations" (line
262).

Role/grant provisioning already exists over a plain connection (`jurisearch-storage/src/backend.rs::provision_roles`,
"provision the least-privilege roles + grants on a freshly-migrated database"), so that part is a fair
build-on. Migration **execution**, however, is currently coupled to the self-managed instance:
`run_migrations()` is a method on `ManagedPostgres` and applies via `execute_sql` against the
`pg_ctl`-owned PG (`jurisearch-storage/src/migrations.rs`). In shared-server mode `jurisearch-syncd`
explicitly does **not** run migrations (`crates/jurisearch-syncd/src/main.rs::build_writer`), and the
work/09 tests that exercise the "shared server" actually point at a `ManagedPostgres`. There is no
connection-based migration runner today that can migrate an operator-owned system PostgreSQL.

Recommended fix: name this as an explicit Phase 3 deliverable/prerequisite — a migration applier that runs
the static `MIGRATIONS` SQL over an admin connection to an external PG (independent of `ManagedPostgres`)
— and specify which identity runs it (owner/admin with DDL + `CREATE EXTENSION`, not the writer). This
also reconciles with Phase 2's check that "writer role can apply migrations" (line 233): clarify whether
migrations are owner-run (recommended) or writer-run, since the two checks currently disagree.

### WARN-5 — The demo path's `jurisearch-client --local` socket location will not match a sudo-launched demo site

Section: "Target operator experience → Local-only demo mode" (lines 51-61).

`--local` resolves `unix://$XDG_RUNTIME_DIR/jurisearch-site.sock`
(`crates/jurisearch-client/src/lib.rs::resolve_endpoint`). `sudo jurisearchctl demo up` runs as root; if
the demo brings up `serve-site` as a system unit it binds under `RuntimeDirectory=jurisearch`
(`/run/jurisearch/...`, per `deploy/systemd/jurisearch-site.service`), while the user's later
`jurisearch-client --local status` looks in `/run/user/<uid>/jurisearch-site.sock`. The two do not agree,
so the demo's client leg cannot connect. (Note the site unit's own comment claiming `--local` resolves
`/run/jurisearch/jurisearch-site.sock` is already inconsistent with the client code — the plan inherits
that latent mismatch.)

Recommended fix: have `demo up` bind the site UDS at exactly the path `--local` resolves for the invoking
user (run as that user, not root, with their `$XDG_RUNTIME_DIR`), or have `demo up` print the precise
`jurisearch-client --server unix:///<path> ...` command instead of relying on `--local`. Pin the demo
socket path in the plan.

### WARN-6 — Phase 8's auto-skipped hybrid leg contradicts the "no silent skip" invariant and is a potential false-green

Section: "Phase 8 → Smoke legs" (line 389) vs "Invariants under test" (line 394).

The smoke spec says "If embedder is configured and corpus fingerprint matches, run a hybrid search"
(conditional, self-skipping), but the invariant says "A skipped leg is only allowed under an explicit
`--allow-skip <reason>` flag." A dense/hybrid path that silently does not run when the fingerprint does
not match is exactly the kind of unannounced gap the acceptance is meant to prevent.

Recommended fix: when the embedder is configured, make the hybrid leg required (fail if the fingerprint
does not match — that is itself a real deployment defect to surface), or, if it is to be skipped, emit an
explicit, recorded `SKIPPED(hybrid): <reason>` line in the smoke output rather than omitting it silently.

---

### NIT-1 — Thin-client `configure`/`doctor` require restructuring the current flat positional `command`

Section: "Phase 7 → Deliverables" (lines 361-363).

Today `jurisearch-client` parses a single positional `command` passed straight to
`Operation::parse_command` (`crates/jurisearch-client/src/main.rs`). Introducing `configure` and `doctor`
makes them reserved subcommand names that can no longer be server operations. They do not collide with the
current site ops (`status/search/fetch/cite/related/context/compare`), so this is safe — but the plan
should note the parser changes from a flat positional to a subcommand structure, and that `configure`/`doctor`
are reserved client-local verbs.

### NIT-2 — Endpoint resolution order wording vs the existing precedence

Section: "Phase 7 → Deliverables" (line 362): "Endpoint resolution order: `--server`, `--local`,
`JURISEARCH_SITE_URL`, client config file."

In the current code `--local` is checked before `--server` and the two are mutually exclusive
(`conflicts_with = "server"`); env is the existing fallback (`resolve_endpoint`). The stated order is
operationally fine (the only new rule that matters is "config file is the lowest-priority fallback"), but
to avoid implying a behavior change, note that `--server`/`--local` remain mutually exclusive and that the
config file slots in strictly below `JURISEARCH_SITE_URL`.

### NIT-3 — TCP bind rendering must strip the `tcp://` scheme; UDS binds use `--socket`

Section: "One config file `[site] bind`" (line 136) and Phase 1 validation (line 203).

`site.bind = "tcp://host:port"` carries a scheme, but `serve-site --tcp` and the existing
`JURISEARCH_SITE_BIND` expect a bare `host:port` (`deploy/systemd/jurisearch-site.service`,
`ServeSiteArgs.tcp`). The plan should state that rendering strips the scheme for the TCP form and switches
the unit to `--socket <abs-path>` for the `unix://` form (Phase 4 line 299 already implies two unit
shapes; make the bind translation explicit so the golden tests pin it).

### NIT-4 — "logs have no startup failures" is a fragile, false-green/false-red check

Section: "Phase 8 → Smoke legs" (line 384).

Asserting the absence of failure strings in the journal is brittle: it can pass against a broken start, and
it can wrongly fail on the **expected** loud `binding ... with NO CLIENT AUTHENTICATION` warning the site
prints on every off-loopback bind. Prefer asserting positive readiness signals (the `listening on` bind
line the existing acceptance script greps for, plus a green `site readiness`) over scanning for the
absence of errors.

---

## 2. Open questions / residual risks

- Who owns `CREATE EXTENSION pgvector`/`pg_search` — a superuser bootstrap step, or a role with the
  extension-create privilege? The plan says the writer "does not need superuser after extensions are
  installed" (Phase 3, line 271), which implies a separate privileged step the operator UX should name in
  the happy path (today it is invisible).
- Phase 6's "effective fingerprint matches the corpus package fingerprint" (line 346) requires an already
  caught-up corpus to compare against, yet Phase 6 is positioned as runnable independently of Phase 5.
  Clarify that this sub-check degrades to "no active corpus to compare" before catch-up.
- The plan never states where the license-purpose anchor and license token come from operationally (the
  producer/issuer hand-off). Worth a one-line pointer so `bootstrap-trust` inputs are unambiguous.
- `site uninstall` (Phase 4, line 285) "removes generated services only after confirmation" — confirm it
  never drops the operator DB, corpus data, or the operator-owned `site.toml`; the plan implies this but
  does not state a data-safety invariant for uninstall the way it does for rollback (Phase 9, line 422).

## 3. Verification notes (what I inspected)

- The artifact: `work/10-next-plans/01-makeitsimpletodeploy.md` (full).
- Systemd units: `deploy/systemd/jurisearch-site.service`, `jurisearch-syncd.service`,
  `jurisearch-bge-m3.service` — confirmed env-var families, the `ReadOnlyPaths` non-expansion note, and
  the no-client-auth warning.
- CLI surfaces: `crates/jurisearch-cli/src/args.rs` (`ServeSiteArgs`, command set),
  `crates/jurisearch-syncd/src/main.rs` (`Trust`/`Subscribe`/`Update`/`Run`/`Status`, shared-server vs
  self-managed `build_writer`), `crates/jurisearch-client/src/main.rs` + `lib.rs` (positional `command`,
  `resolve_endpoint` precedence, `--local` socket path).
- Trust model: `crates/jurisearch-syncd/src/trust.rs` and `jurisearch-storage/src/trust.rs`
  (`PACKAGE_PURPOSE`/`LICENSE_PURPOSE`, `install_verified_license_token` builds a license-purpose
  verifier).
- Migrations/provisioning: `crates/jurisearch-storage/src/migrations.rs` (`ManagedPostgres::run_migrations`),
  `crates/jurisearch-storage/src/backend.rs` (`provision_roles` over a connection), and
  `crates/jurisearch-package-build/tests/shared_writer_loopback.rs` (the "shared server" is a
  `ManagedPostgres`).
- Embedder: `crates/jurisearch-cli/src/embedding_runtime/mod.rs` (`PreparedQueryEmbedder::from_env`) and
  `crates/jurisearch-cli/src/site/serve.rs` usage.
- Acceptance/readiness semantics: `work/09-jurisearch-cli/scripts/single-host-acceptance.sh` and
  `work/09-jurisearch-cli/05-two-host-acceptance.md` (the `query_readiness` stamp and the bge-m3 → syncd →
  site ordering).
- Workspace binary inventory: `Cargo.toml` members + per-crate `[[bin]]` targets (confirmed
  `jurisearch-package` binary lives in `jurisearch-package-build`; `jurisearch-deploy` is a new crate, as
  the plan states).

I did not run code or tests (plan-only artifact). I did not edit any files.

I confirmed the plan honors the non-negotiable constraints: no Kubernetes/Helm/Compose/HTTP/gRPC/internet
exposure/new client auth is requested; the trusted-LAN boundary and versioned JSONL protocol are preserved
(Non-goals, lines 66-74). The findings above are about correctness/sequencing/packaging within those
constraints, not violations of them.

VERDICT: FIXES_REQUIRED
