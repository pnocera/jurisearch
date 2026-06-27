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

This document is the GENUINE two-physical-host operated run (the evidence CI cannot capture). Fill in the
`OBSERVED` blocks when you operate it.

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

## OBSERVED (fill in when operated)

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
