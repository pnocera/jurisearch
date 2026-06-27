//! Dependency-cone guard for the three **dependency-light base crates** (`jurisearch-core`,
//! `jurisearch-transport`, `jurisearch-render`). The thin client (work/09 P6) depends only on these,
//! so they must NEVER pull the heavy stack (storage / embed / ingest / cli / official-api / package /
//! syncd / postgres / model runtimes). Caught here in P1, not deferred to the thin-client phase.

use std::process::Command;

/// The base crates whose cone must stay clean.
const BASE_CRATES: [&str; 3] = [
    "jurisearch-core",
    "jurisearch-transport",
    "jurisearch-render",
];

/// Crates that must NOT appear in any base crate's normal-dependency tree.
const FORBIDDEN: [&str; 10] = [
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
];

#[test]
fn base_crates_have_a_clean_dependency_cone() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    for crate_name in BASE_CRATES {
        let output = Command::new(&cargo)
            .args(["tree", "-e", "normal", "--prefix", "none", "-p", crate_name])
            .output()
            .unwrap_or_else(|error| panic!("failed to run `cargo tree` for {crate_name}: {error}"));
        assert!(
            output.status.success(),
            "`cargo tree` failed for {crate_name}: {}",
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
                "{crate_name} unexpectedly depends on `{forbidden}` (heavy stack must stay out of the base cone):\n{tree}"
            );
        }
    }
}
