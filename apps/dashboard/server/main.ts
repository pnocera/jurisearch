/**
 * jurisearch-dashboard — entrypoint (M0 STUB).
 *
 * M0 only wires the `--version` build-id contract (parity with the Rust binaries). The real
 * Bun.serve HTTP server + providers land in M2/M3. Do not start a server here.
 */
import { DASHBOARD_NAME } from "@jurisearch-dashboard/shared";
import { BUILD_COMMIT, BUILD_TARGET, BUILD_VERSION } from "./buildinfo";
import { formatVersionLine } from "./version";

// HARD CONTRACT: this exact line must match dist.sh's release audit (dist.sh:274-293) and
// deploy.sh's compare (deploy.sh:165-170,:502-507): `<bin> <version> (<commit>, <target>)`.
const VERSION_LINE = formatVersionLine(BUILD_VERSION, BUILD_COMMIT, BUILD_TARGET);

function main(argv: readonly string[]): number {
  if (argv.includes("--version")) {
    // EXACTLY the contract line, nothing else.
    console.log(VERSION_LINE);
    return 0;
  }
  console.log(`${DASHBOARD_NAME} — ${VERSION_LINE} — scaffold (M0); HTTP server lands in M3.`);
  return 0;
}

process.exit(main(process.argv.slice(2)));
