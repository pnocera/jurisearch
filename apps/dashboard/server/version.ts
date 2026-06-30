/**
 * The EXACT `--version` contract line, in ONE place. Must match dist.sh's release exact-match audit
 * (dist.sh:274-293) and deploy.sh's compare (deploy.sh:165-170,:502-507):
 *
 *   jurisearch-dashboard <version> (<commit>, <target>)
 *
 * Tested in server/version.test.ts; the compiled binary is asserted against it in
 * server/compile-smoke.test.ts.
 */
export function formatVersionLine(version: string, commit: string, target: string): string {
  return `jurisearch-dashboard ${version} (${commit}, ${target})`;
}
