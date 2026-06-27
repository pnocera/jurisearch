# P6 working notes — codex design GO-with-adjustments (qa/20260627-181541)

P6 (LAST phase) = thin client + LAN exposure + protocol skew + ops + two-host acceptance. Ship a
STRUCTURALLY-separate thin artifact; LAN exposure is an explicit operator act; the local heavy CLI stays
the rollback path.

## Binding adjustments

**Q1 — Crate shape.** New `jurisearch-client` crate (lib+bin), separate from `jurisearch-cli` (NOT a CLI
feature flag). Split: `jurisearch-transport` owns the protocol-level `JsonlClient` (over an already-open
`Read+Write`) + ALL encode/decode incl. the new response envelope (no CLI/config/URL policy);
`jurisearch-client` owns URL parsing, TCP/UDS dialing, CLI UX, env/config defaults, render via
`jurisearch-render`. URL schemes: `tcp://host:port` + `unix:///absolute/path` (NO bare host:port).
`JURISEARCH_SITE_URL` env default. Parse the two URL forms manually with std (stay on core+transport+render;
`url` would be an allowed exception but prefer std). Invariant = no storage/embed/ingest/cli stack.

**Q2 — Protocol skew: VERSION THE SITE RESPONSE TOO (option ii).** Add (core::envelope)
`ProtocolResponseEnvelope { proto: ProtocolVersion, response: SessionResponse }` (response-specific, NOT
overloading the request envelope). Transport: `encode_site_response_envelope_line` /
`decode_site_response_envelope_line` (reject missing proto → Unversioned; wrong proto → UnsupportedVersion,
mirroring the request decoder). Update `serve_site_connection` to write the VERSIONED site response for
EVERY reply (incl. framing/protocol errors where the id isn't recoverable). The thin client decodes ONLY
the site response envelope; a bare response → a clear "unversioned response from site service; protocol
skew or old server" error (never accept a bare SUCCESS on the site path). KEEP THE LOCAL PATH BARE: local
session/batch/serve still bare req+resp; encode/decode_bare_response_line stay for local + tests; the site
response envelope is ONLY for serve-site + jurisearch-client. Update transport docs (the
bare-site-response-sufficient comment is now wrong). NOTE: `decode_site_envelope_line` already rejects
both higher AND lower request `proto` (!= PROTOCOL_VERSION), so request-direction skew already fails loud.

**Q3 — LAN exposure.** Explicit opt-in: keep `--tcp <ip:port>` + add `--allow-lan` (permit non-loopback)
+ a LOUD stderr startup warning EVERY off-loopback bind ("no client authentication; trusted LAN/Tailscale
only"). Refuse WILDCARD binds (0.0.0.0, ::) unless `--allow-wildcard-lan`. Allowlist under --allow-lan:
loopback always; RFC1918 (10/8, 172.16/12, 192.168/16); CGNAT/Tailscale 100.64/10; IPv6 ULA fc00::/7.

**Q4 — Two-host acceptance.** CI = SINGLE-host integration test proving the topology in process:
build/publish a package → catch the site PG up via the syncd substrate → `serve-site` on a loopback TCP
port in a thread → thin client over `tcp://127.0.0.1:<port>` → assert rendered bytes match the one-shot
CLI for ≥1 non-embedding op (fetch/status) + a query path if a stable embedder is available. Env-gate on
the managed-PG harness; NO real 2nd host in CI. The REAL two-host run = DOCUMENTED ops evidence (runbook +
captured output/checksum) checked into work/09 or deploy/.

**Q5 — Dependency cone.** Reuse the `cargo tree -e normal --prefix none` pattern (catches transitive).
Forbidden for jurisearch-client: jurisearch-{storage,embed,ingest,cli,official-api,package-build,syncd},
postgres, tokenizers, ureq. (Optional 2nd direct-dep allowlist test.) Existing pattern:
crates/jurisearch-transport/tests/dependency_cone.rs.

**Q6 — `--local`.** Means "connect to a LOCAL serve-site over a UDS" (NEVER the heavy in-process path).
`--server <url>` explicit; `--local` = shorthand for `unix://$XDG_RUNTIME_DIR/jurisearch-site.sock`;
`JURISEARCH_SITE_URL` default. XDG_RUNTIME_DIR unset → clear error asking for `--server unix:///path`.
Server-side operated socket: `unix:///run/jurisearch/jurisearch-site.sock`.

## Risks
Site client uses site req + site response envelopes ONLY (bare codecs stay local-only). First client slice
= `command + JSON args` (a friendlier flag CLI can layer later WITH parity tests). Skew tests BOTH
directions (client-new/server-old bare response; client-old/server-new) + unit-test the response decoder
rejecting missing/wrong proto. LAN warning to stderr, unmissable. Ops evidence CHECKED IN.

## Build order
core ProtocolResponseEnvelope → transport response codecs + JsonlClient + doc update → site listener
versioned responses (+ fix site tests to decode the envelope) → serve-site --allow-lan/--allow-wildcard-lan
+ allowlist + warning → jurisearch-client crate (URL parse, dial, JsonlClient, render, CLI command+JSON
args, --server/--local/env) + dependency-cone test → single-host two-host topology test + skew tests →
ops: systemd units (site-server, bge-m3) + two-host runbook/evidence → codex review → commit on main.
