# Verdict: GO with adjustments

The P6 direction is sound: ship a structurally separate thin artifact, make LAN exposure an explicit operator act, and preserve the local heavy CLI as the rollback path. The main adjustment is protocol skew: now is the right time to make the **site response** versioned too. The local `session`/`batch`/`serve` protocol should remain bare and version-free.

1. **Q1 - Thin Client Crate / `JsonlClient` / URL Shape**

   Add `jurisearch-client` as a new workspace crate and binary. Keep it structurally separate from `jurisearch-cli`; do not make it a feature flag on the heavy CLI.

   Split the client pieces this way:

   - `jurisearch-transport`: owns the protocol-level `JsonlClient` over an already-open `Read + Write` stream, plus all encode/decode functions. It should know framing, max-line, request envelope, response envelope, and `SessionResponse` decoding. It should not know CLI flags, config files, or service discovery.
   - `jurisearch-client`: owns endpoint parsing, TCP/UDS dialing, command-line UX, env/config defaults, and rendering via `jurisearch-render`.

   That keeps transport reusable by tests/tools without pushing URL or CLI policy into the codec crate.

   Accept explicit URL schemes:

   - `tcp://host:port`
   - `unix:///absolute/path`

   Do not accept ambiguous bare `host:port` in P6. The point of "addressed by service URL" is that the transport is explicit and copyable in configs/runbooks. Add `JURISEARCH_SITE_URL` as the environment default if useful.

   If you keep the dependency rule literally to only `core + transport + render`, parse these two URL forms manually with `std`; otherwise `url` is lightweight but should be listed as an intentional exception. The architectural invariant is no storage/embed/ingest/CLI stack, not "no helper crate ever."

2. **Q2 - Protocol Skew**

   `decode_site_envelope_line` already rejects both higher and lower request versions: it compares `envelope.proto != PROTOCOL_VERSION` and returns `UnsupportedVersion { got, supported }`. So a skewed request fails loudly today.

   Still, I would implement **(ii): version the site response too**. P6 is the first thin-client release, so changing the site wire format now is cheap, and it makes the protocol contract symmetric:

   - client validates the server's response version on every reply;
   - a response from an old/bare server is not accidentally accepted as a valid site reply;
   - future response-envelope changes have a clear compatibility gate.

   Add a response-specific frame type rather than overloading the request envelope:

   ```rust
   pub struct ProtocolResponseEnvelope {
       pub proto: ProtocolVersion,
       pub response: SessionResponse,
   }
   ```

   Then add transport functions such as:

   ```rust
   encode_site_response_envelope_line(&ProtocolResponseEnvelope)
   decode_site_response_envelope_line(line) -> Result<ProtocolResponseEnvelope, TransportError>
   ```

   Update `serve_site_connection` to write the versioned site response for every site reply, including framing/protocol errors where the request id is not recoverable. The thin client should decode only the site response envelope. If it receives a bare response, surface a clear "unversioned response from site service; protocol skew or old server" error. You may optionally try to decode a bare error response only to enrich the diagnostic, but never accept a bare success on the site path.

   Keep the local path untouched:

   - local `session`/`batch`/`serve` still use bare request and bare response;
   - `encode_bare_response_line` / `decode_bare_response_line` remain for local session compatibility and tests;
   - the new site response envelope is used only by `serve-site` and `jurisearch-client`.

   The transport crate docs should be updated; they currently say the bare site response is sufficient for skew detection.

3. **Q3 - LAN Exposure**

   Yes, require an explicit opt-in flag. A non-loopback bind must never happen just because an operator typed the wrong address.

   Minimal P6 shape:

   - keep `--tcp <ip:port>` for the address;
   - add `--allow-lan` to permit non-loopback;
   - print a loud startup warning: no client authentication, trusted LAN/Tailscale only.

   I would add one more guard: refuse wildcard binds (`0.0.0.0` and `::`) unless a second flag is provided, for example `--allow-wildcard-lan`. Binding all interfaces with no auth is a different risk from binding a specific Tailscale/RFC1918 address.

   Reasonable default allowlist under `--allow-lan`:

   - loopback: always allowed;
   - RFC1918 IPv4: `10/8`, `172.16/12`, `192.168/16`;
   - CGNAT/Tailscale: `100.64/10`;
   - IPv6 ULA: `fc00::/7`;
   - optionally IPv6 link-local if the operator explicitly binds an interface-scoped address.

   If you do not want an IP-range policy yet, at least require `--allow-lan` and special-case wildcard refusal. The no-auth decision is acceptable only if the bind act is explicit and noisy.

4. **Q4 - Two-Host Acceptance**

   Your proposed automated slice is right: a single-host integration test should prove the topology in CI, while the true two-physical-host run is recorded as ops evidence/runbook output.

   Automated test scope:

   - build/publish a package from a producer-managed PG or use the existing package-build harness;
   - catch the site PG up through the syncd substrate;
   - start `serve-site` on loopback in a thread or use the listener/connection test harness;
   - run the thin client against `tcp://127.0.0.1:<port>`;
   - assert rendered bytes match the one-shot CLI for at least one non-embedding operation such as `fetch` or `status`, plus a query path if the test harness can provide a stable embedder.

   Keep the automated test env-gated if it needs the managed PG extension harness. Do not require a real second host in CI. The real two-host run should be a documented operated acceptance artifact: commands, hostnames/IPs, service URLs, package id/sequence, and thin-client output checksum or captured output.

5. **Q5 - Dependency Cone Enforcement**

   Use the existing `cargo tree -e normal --prefix none` pattern. It catches transitive dependencies, which a direct `[dependencies]` check does not.

   Add `jurisearch-client` to a cone test with a forbidden list including at least:

   - `jurisearch-storage`
   - `jurisearch-embed`
   - `jurisearch-ingest`
   - `jurisearch-cli`
   - `jurisearch-official-api`
   - `jurisearch-package-build`
   - `jurisearch-syncd`
   - `postgres`
   - `tokenizers`
   - `ureq`

   If you want the stronger "only approved lightweight direct deps" rule, add a second `cargo metadata` or `Cargo.toml` allowlist test. The tree test is the important invariant; the direct-dep allowlist is a policy guard.

6. **Q6 - `--local` Dev Fallback**

   Confirmed: `--local` must mean "connect to a local `serve-site` process," not "call the heavy in-process one-shot path." The thin client should never link the heavy CLI.

   Recommended convention:

   - `--server <url>` uses the explicit service URL;
   - `--local` is shorthand for a Unix socket URL;
   - `JURISEARCH_SITE_URL` can provide the default.

   For the local socket, use:

   ```text
   unix://$XDG_RUNTIME_DIR/jurisearch-site.sock
   ```

   If `XDG_RUNTIME_DIR` is not set, fail with a clear message asking for `--server unix:///path` rather than guessing a shared `/tmp` path. For systemd/site-host docs, use a separate operated path such as `unix:///run/jurisearch/jurisearch-site.sock` if you want a local server-side socket.

## Additional P6 Risks

- **Do not accidentally reuse local codecs on the site client.** The thin client should use site request + site response envelopes only. Bare codecs remain for local session compatibility and low-level tests.
- **Request DTO/default parity still matters.** If the thin client parses friendly CLI flags, make sure defaults and validation match the server/site handlers. A safer first slice is `command + JSON args`; a friendlier CLI can layer on top once parity tests exist.
- **Version-skew tests need both directions.** Test client-new/server-old with a bare response from the old server, and client-old/server-new if feasible. At minimum, unit-test response decoder rejection of missing/wrong `proto`.
- **LAN warning should be stderr and unmissable.** The service is intentionally unauthenticated; operators need a clear startup line every time it binds off-loopback.
- **Ops evidence should be checked in.** The final phase is not done just because the code works on one machine; record the two-host commands and observed output in `work/09-jurisearch-cli/` or `deploy/`.
