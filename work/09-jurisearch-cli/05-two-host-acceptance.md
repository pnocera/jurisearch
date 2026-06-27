# work/09 P6 — Two-host acceptance runbook + ops evidence

The capstone acceptance for the producer → site-server → thin-client deployment. The protocol/render leg
is proven AUTOMATICALLY in CI (single host) by:

- `jurisearch-cli` `site::tests::the_thin_client_queries_the_site_over_tcp_with_one_shot_render_parity`
  — the thin client queries `serve-site` over loopback TCP and renders BYTE-IDENTICALLY to the in-process
  site path (which `…serves_the_full_operation_set…` proves equals the one-shot CLI). Chain: thin client
  over TCP == in-process site == one-shot CLI.
- `…the_thin_client_rejects_an_old_servers_unversioned_reply` — a bare (old-server) reply is a loud
  protocol-skew error, never silently accepted.
- `jurisearch-client` `dependency_cone::the_thin_client_has_a_clean_dependency_cone` — the thin artifact
  links NONE of the storage/embed/ingest/cli/postgres/tokenizers/ureq stack.
- `jurisearch-transport` response-envelope + `JsonlClient` skew tests (both directions).
- `jurisearch-package-build` `daemon_loop::*` — the producer → package → syncd catch-up that POPULATES the
  site (offline → head), the part this runbook does not re-exercise.

This document is the operator RUNBOOK for the genuine two-physical-host run — a FIELD step (two-host
hardware the dev/CI environment does not have), NOT a checked-in acceptance gate. The P6 acceptance is
layered: (a) the serve-site SERVICE path (handlers, dispatch, read gate, full op set, render parity) is
proven by the AUTOMATED in-process E2E listed above (which also proves protocol-version SKEW rejection);
(b) the shipped `jurisearch-client` operator surface (contract seam + connection/URL diagnostics) is
proven by the checked-in SINGLE-HOST capture below. The
shipped serve-site PROCESS run (bind + DB + embedder-from-env + answering status/fetch/search + a fetch
hash) and the two-host run are prerequisite-gated field steps; fill their `OBSERVED` blocks when operated.

**Single-host capture.** `scripts/single-host-acceptance.sh` collapses producer/site/client onto one host
and drives the SHIPPED `jurisearch serve-site` + `jurisearch-client` binaries. The legs that need no
server/DB (the client's contract seam + connection/URL diagnostics) ALWAYS run and are ASSERTED on the
contract's OWN diagnostic — each negative leg must exit non-zero AND emit the expected message (so a
regression that forwarded `index_dir`/`zone` to the server instead of rejecting it locally fails the run,
even though a dead-port connection error is also non-zero); a positive leg that fails likewise exits the
script non-zero — never a silent green. (Protocol-version SKEW is proven separately by the automated
`…rejects_an_old_servers_unversioned_reply` + transport response-envelope tests, not this capture.) The
serve-site data legs (`status`/`fetch`/bm25 `search`) run only when both preflight prerequisites are
present — a migrated + role-provisioned DB with an ACTIVE, well-formed, signature-matching
`query_readiness` stamp, and an embedder `serve-site` can start from env — and each is then required to
succeed (`serve-site` must bind; status/fetch/search must return zero). It NEVER fabricates a data-leg
result; a missing prerequisite is named exactly and the data legs are skipped. The §"Single-host OBSERVED"
block below is a real run of that script (here, the serve-site data legs were prerequisite-skipped).

## Topology

- **Host E1 (producer)** — builds + signs packages from the producer corpus; publishes a manifest +
  artifacts to a directory the site can read (or a mirror it pulls).
- **Host S (site server)** — system PostgreSQL (pgvector + pg_search), the local bge-m3 endpoint,
  `jurisearch-syncd` (the single writer, catching the site DB up to the producer head), and
  `jurisearch serve-site` (the read-only query service, bound to the trusted-LAN/Tailscale address).
- **Host C (thin client)** — runs ONLY `jurisearch-client`, addressed by the site URL.

## Prerequisites (host S)

1. PostgreSQL up with `pgvector` + `pg_search`; database `jurisearch`; roles provisioned
   (`jurisearch_owner` NOLOGIN, `jurisearch_write`, `jurisearch_read` SELECT-only) — see work/08 + P2.
2. `deploy/systemd/jurisearch-bge-m3.service` + `/etc/jurisearch/bge-m3.env` → `systemctl enable --now
   jurisearch-bge-m3`; confirm `curl -s http://127.0.0.1:8081/health` (or the embeddings route) responds.
3. `deploy/systemd/jurisearch-syncd.service` + `/etc/jurisearch/syncd.env` → `systemctl enable --now
   jurisearch-syncd`; confirm `journalctl -u jurisearch-syncd` shows a `corpus_cycle` reaching
   `up_to_date`, and `jurisearch-syncd … status --json` shows the corpus at the producer head sequence.
4. `deploy/systemd/jurisearch-site.service` + `/etc/jurisearch/site.env` (set `JURISEARCH_SITE_BIND` to
   the Tailscale/RFC1918 `address:port`) → `systemctl enable --now jurisearch-site`; the journal MUST show
   the loud `binding <addr> with NO CLIENT AUTHENTICATION — trusted LAN / Tailscale only` warning.

## Acceptance steps (host C — the thin client)

```sh
SITE_URL="tcp://<host-S-tailscale-ip>:8099"     # the JURISEARCH_SITE_BIND of host S

# 1. Health: the site reports its served topology + readiness.
jurisearch-client --server "$SITE_URL" status

# 2. A real fetch of a known document id from the producer corpus.
jurisearch-client --server "$SITE_URL" fetch '{"ids":["<a-known-document-id>"]}'

# 3. A hybrid search.
jurisearch-client --server "$SITE_URL" search '{"query":"<a real query>","kind":"decision"}'

# 4. Render parity: the SAME op on host S via the one-shot CLI (against the same site DB / a local
#    index) must print byte-identical output. Capture both and diff:
jurisearch-client --server "$SITE_URL" fetch '{"ids":["<id>"]}' | sha256sum   # host C
# (on host S, the one-shot CLI fetch of the same id) | sha256sum               # host S
```

### Negative checks

```sh
# Unreachable: a wrong port → a clear "cannot reach the site service" error, non-zero exit.
jurisearch-client --server "tcp://<host-S>:1" status ; echo "exit=$?"

# Protocol skew: against an OLD server (or a non-jurisearch TCP service) → a loud protocol-skew error.
# Bad URL: a bare host:port is rejected (use tcp:// / unix://).
jurisearch-client --server "<host-S>:8099" status ; echo "exit=$?"
```

## Single-host OBSERVED (real run of `scripts/single-host-acceptance.sh`)

```
date / operator:        2026-06-27 / pnocera
topology:               single host (producer/site/client collapsed); workstation
binaries:               jurisearch 0.1.0 + jurisearch-client (both built from this tree)

--- client legs that need NO server (SHIPPED jurisearch-client binary; NORMALIZED excerpt: the real
    script prints `+ <absolute target/debug binary> …` then the diagnostic then `exit=<code>` on its own
    line; messages with `…` are elided here. Re-run the script for the exact bytes.) ---
$ jurisearch-client --server tcp://127.0.0.1:1 bogus-op '{}'
jurisearch-client: `bogus-op` is not a site query operation                                  exit=2

$ jurisearch-client --server tcp://127.0.0.1:1 search '{"query":"x","index_dir":"/tmp"}'
jurisearch-client: invalid search args: unknown field `index_dir`, expected one of `query`,
  `kind`, `mode`, `format`, `group_by`, `top_k`, `cursor`, `as_of`, `rrf_lexical_weight`,
  `rrf_dense_weight`, `probes`, `court`, `formation`, `publication`, `decided_from`,
  `decided_to`, `authority_weight`                                                           exit=2
  # the strict contract DTO (Operation::parse_args) rejects a server-owned field at the CLIENT,
  # before any round-trip — the SAME validation the site handler applies (the shared seam).

$ jurisearch-client --server tcp://127.0.0.1:1 search '{"query":"x","zone":"motivations"}'
jurisearch-client: invalid search args: unknown field `zone`, expected one of `query`, ...     exit=2
  # `zone` (a Cassation-only client/online concern) is not on the site search contract.

$ jurisearch-client --server tcp://127.0.0.1:1 status
jurisearch-client: cannot reach the site service at tcp://127.0.0.1:1: Connection refused ...   exit=2

$ jurisearch-client --server 127.0.0.1:8099 status
jurisearch-client: unsupported site URL `127.0.0.1:8099`: use tcp://host:port or unix://...     exit=2

--- data legs (status / fetch / bm25 search against a live serve-site) ---
PREREQ-DB    MISSING   cannot reach a migrated DB at 127.0.0.1:5432/jurisearch (no
                       jurisearch_control/index_manifest) — the workstation DB is a bare client DB.
                       The preflight validates an ACTIVE corpus AND a query_readiness stamp whose
                       signature matches the active topology (the same active_read_signature the read
                       gate keys on) AND a well-formed report, so active-but-unstamped/stale/malformed is
                       also caught. A site DB is
                       provisioned by syncd catching up to the producer head; see Prerequisites step 3.
PREREQ-EMBED MISSING   no JURISEARCH_EMBED_* + local bge-m3 tokenizer on this host. serve-site
                       builds its embedder at startup (status/fetch/bm25 never call it, but it
                       must construct). OpenRouter baai/bge-m3 + a cached tokenizer satisfies it.
RESULT: data legs SKIPPED (not fabricated). Re-run the script on a host S with both prerequisites
        to capture status/fetch/bm25-search + the fetch sha256 here.
```

What this single-host run proves, and what it does NOT: it proves the SHIPPED `jurisearch-client` operator
surface and the contract seam end-to-end (the WARN-1 work) — the strict, contract-owned request DTOs
reject typos / server-owned / local-only fields at the client with the contract's own message, and
connection/URL failures are clear exit-2 diagnostics. It does NOT exercise a shipped `serve-site` PROCESS
here (the two prerequisites are absent on this workstation), so the data legs were skipped, not faked. The
serve-site SERVICE path itself (handlers, dispatch, the writer-owned read gate, the full operation set,
and one-shot render parity) IS proven — IN-PROCESS — by the automated E2E listed at the top of this
document: `site::tests::the_site_service_serves_the_full_operation_set_through_the_read_role` (full op set
through the read role, render parity) and
`…the_thin_client_queries_the_site_over_tcp_with_one_shot_render_parity` (thin client over real TCP ==
in-process site == one-shot CLI). The shipped serve-site PROCESS run (bind + DB + embedder + a real
status/fetch/search + fetch-hash) is the prerequisite-gated data-leg path of the script, captured here
when operated on a host that has a provisioned site DB + embedder.

## Two-host OBSERVED (fill in when operated on two physical hosts)

```
date / operator:
host E1 (producer):           hostname=                ip=
host S (site server):         hostname=                bind=                tailscale-ip=
host C (thin client):         hostname=                ip=
producer head sequence:                 site cursor sequence (syncd status):
package id / digest applied:

status output (host C):
<paste>

fetch <id> sha256 — host C:                 host S one-shot:                 MATCH? (y/n)

unreachable check exit code:            skew check error:            bad-url check error:
notes:
```
