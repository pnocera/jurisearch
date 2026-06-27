//! work/09 P6 — dependency-cone guard for the THIN CLIENT. `jurisearch-client` is a structurally
//! separate artifact a second host runs to query the site; it must link ONLY the three dependency-light
//! base crates (core/transport/render) and never the heavy stack (storage / embed / ingest / cli /
//! official-api / package(-build) / syncd / postgres / tokenizers / ureq). Enforced via `cargo tree`,
//! which catches TRANSITIVE pulls a direct-`[dependencies]` check would miss.

use std::process::Command;

/// Crates that must NEVER appear in `jurisearch-client`'s normal-dependency tree.
const FORBIDDEN: [&str; 11] = [
    "jurisearch-storage",
    "jurisearch-embed",
    "jurisearch-ingest",
    "jurisearch-cli",
    "jurisearch-official-api",
    "jurisearch-package",
    "jurisearch-package-build",
    "jurisearch-syncd",
    "postgres",
    "tokenizers",
    "ureq",
];

#[test]
fn the_thin_client_has_a_clean_dependency_cone() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(&cargo)
        .args([
            "tree",
            "-e",
            "normal",
            "--prefix",
            "none",
            "-p",
            "jurisearch-client",
        ])
        .output()
        .unwrap_or_else(|error| panic!("failed to run `cargo tree`: {error}"));
    assert!(
        output.status.success(),
        "`cargo tree` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    // Each `--prefix none` line is "<name> vX.Y.Z [(path)]"; the first token is the crate name.
    let deps: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    for forbidden in FORBIDDEN {
        assert!(
            !deps.contains(&forbidden),
            "jurisearch-client unexpectedly depends on `{forbidden}` (the thin client must stay free of \
             the heavy stack):\n{tree}"
        );
    }
}
