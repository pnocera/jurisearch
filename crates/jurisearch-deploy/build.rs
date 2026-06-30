//! Stamp the workspace version's companion build metadata (git commit + build target) into THIS crate's
//! compile env, so clap's `version = jurisearch_buildinfo::version!()` resolves at compile time. The
//! resolution/override contract lives in `jurisearch_buildinfo::stamp`.
fn main() {
    jurisearch_buildinfo::stamp();
}
