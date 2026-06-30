/**
 * Fail-closed bind guard (design §5.4 / §10): the dashboard has NO auth — the tailnet ACL is the only
 * boundary — so it must NEVER listen on a wildcard/unspecified address where a non-tailnet interface
 * could reach it. This is the load-bearing safety guarantee, so the guard does NOT string-match a
 * deny-list (Codex M3 BLOCKER: `Bun.serve` accepts many all-interface spellings — `0.0.0.0`,
 * `000.000.000.000`, `0x0`, `0`, `0.0`, `::`, `0:0:0:0:0:0:0:0`, `[::]`, `::ffff:0.0.0.0`,
 * `::ffff:0:0`, …). Instead it NORMALIZES and CLASSIFIES the literal: strip IPv6 brackets, lower-case,
 * parse IPv4 (incl. zero-padded/short/hex forms) and IPv6 (incl. `::` compression, embedded IPv4, and
 * the IPv4-mapped unspecified `::ffff:0.0.0.0`), and REJECT every all-zero/unspecified address. A
 * concrete loopback, an explicit unicast address, or a hostname is allowed; anything that resolves to
 * "all interfaces" throws BEFORE `Bun.serve` so the process stops instead of silently exposing itself.
 */

/** A thrown bind-guard violation (distinct type so callers/tests can assert it precisely). */
export class BindGuardError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "BindGuardError";
  }
}

/**
 * Parse an IPv4-shaped literal into its 32-bit value, or `null` if it is not an IPv4 literal at all
 * (e.g. a hostname). Accepts 1–4 dot-separated parts, each decimal or `0x`-hex (mirroring the
 * permissive `inet_aton` forms the OS/Bun accept: `0`, `0.0`, `000.000.000.000`, `0x0`). We only need
 * the all-zero test, so an out-of-range/partial part makes it "not an IPv4 literal" (→ allowed, Bun
 * will reject a genuinely bad address) rather than silently zero.
 */
function parseIpv4Value(text: string): number | null {
  const parts = text.split(".");
  if (parts.length < 1 || parts.length > 4) {
    return null;
  }
  let acc = 0;
  for (const part of parts) {
    let value: number;
    if (/^0x[0-9a-f]+$/.test(part)) {
      value = Number.parseInt(part.slice(2), 16);
    } else if (/^\d+$/.test(part)) {
      value = Number.parseInt(part, 10);
    } else {
      return null; // not numeric → not an IPv4 literal (treat as hostname → allowed)
    }
    if (!Number.isInteger(value) || value < 0) {
      return null;
    }
    // OR the parts together: the result is 0 iff every part is 0 (all we test for).
    acc |= value;
  }
  return acc;
}

/**
 * Expand an IPv6 literal (lower-cased, brackets stripped) into its 8 hextets, handling `::`
 * compression and an embedded IPv4 tail. Returns `null` if it is not a valid IPv6 literal.
 */
function expandIpv6(text: string): number[] | null {
  let head = text;
  let tail: number[] = [];

  const dot = text.indexOf(".");
  if (dot >= 0) {
    // Embedded IPv4 in the last group (e.g. `::ffff:0.0.0.0`).
    const lastColon = text.lastIndexOf(":");
    if (lastColon < 0) {
      return null;
    }
    const v4 = text.slice(lastColon + 1).split(".");
    if (v4.length !== 4) {
      return null;
    }
    const bytes: number[] = [];
    for (const part of v4) {
      if (!/^\d+$/.test(part)) {
        return null;
      }
      const n = Number.parseInt(part, 10);
      if (n > 255) {
        return null;
      }
      bytes.push(n);
    }
    tail = [((bytes[0] ?? 0) << 8) | (bytes[1] ?? 0), ((bytes[2] ?? 0) << 8) | (bytes[3] ?? 0)];
    head = text.slice(0, lastColon); // drop the `:ipv4` suffix; the colon boundary stays as `::`/`:`
  }

  const parseGroups = (group: string): number[] | null => {
    if (group === "") {
      return [];
    }
    const out: number[] = [];
    for (const hextet of group.split(":")) {
      if (!/^[0-9a-f]{1,4}$/.test(hextet)) {
        return null;
      }
      out.push(Number.parseInt(hextet, 16));
    }
    return out;
  };

  const halves = head.split("::");
  if (halves.length > 2) {
    return null;
  }
  if (halves.length === 2) {
    const left = parseGroups(halves[0] ?? "");
    const right = parseGroups(halves[1] ?? "");
    if (left === null || right === null) {
      return null;
    }
    const fill = 8 - tail.length - left.length - right.length;
    if (fill < 0) {
      return null;
    }
    return [...left, ...new Array<number>(fill).fill(0), ...right, ...tail];
  }
  const groups = parseGroups(halves[0] ?? "");
  if (groups === null) {
    return null;
  }
  const all = [...groups, ...tail];
  return all.length === 8 ? all : null;
}

/** Whether the 8-hextet IPv6 address binds all interfaces: `::` (all-zero) or `::ffff:0:0` (mapped). */
function isUnspecifiedIpv6(hextets: number[]): boolean {
  const allZero = hextets.every((h) => h === 0);
  const mappedUnspecified =
    hextets[0] === 0 &&
    hextets[1] === 0 &&
    hextets[2] === 0 &&
    hextets[3] === 0 &&
    hextets[4] === 0 &&
    hextets[5] === 0xffff &&
    hextets[6] === 0 &&
    hextets[7] === 0;
  return allZero || mappedUnspecified;
}

/** Classify a normalized address as "binds all interfaces" (unspecified/all-zero/wildcard). */
function isWildcardAddress(address: string): boolean {
  if (address === "" || address === "*") {
    return true;
  }
  if (address.includes(":")) {
    const hextets = expandIpv6(address);
    return hextets !== null && isUnspecifiedIpv6(hextets);
  }
  return parseIpv4Value(address) === 0;
}

/**
 * Throw unless `bind` is an explicit, non-wildcard address. Returns the trimmed address on success.
 */
export function assertExplicitBind(bind: string): string {
  const trimmed = bind.trim();
  // Normalize for classification only: strip surrounding IPv6 brackets and lower-case.
  const normalized = trimmed.replace(/^\[(.*)\]$/, "$1").toLowerCase();
  if (isWildcardAddress(normalized)) {
    throw new BindGuardError(
      `refusing to start: bind address "${bind}" resolves to a wildcard/unspecified (all-interfaces) ` +
        "address. Configure an explicit tailnet address (loopback is allowed for dev/test); never bind 0.0.0.0/::.",
    );
  }
  return trimmed;
}
