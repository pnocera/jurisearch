mod common;

use common::discover_pg_config;
use jurisearch_storage::package_catalog::{PackageCatalogRow, insert_package_catalog_row};
use jurisearch_storage::runtime::{ManagedPostgres, StorageError};

fn row<'a>(
    package_id: &'a str,
    package_digest: &'a str,
    builder_versions: &'a serde_json::Value,
) -> PackageCatalogRow<'a> {
    PackageCatalogRow {
        corpus: "core",
        package_sequence: 1,
        package_id,
        package_kind: "baseline",
        baseline_id: "core-2026-06-27-g0001",
        generation: "core_g0001",
        included_change_seq_low: 0,
        included_change_seq_high: 7,
        previous_package_id: None,
        previous_package_digest: None,
        package_digest: Some(package_digest),
        manifest_digest: Some("sha256:manifest"),
        schema_version: 22,
        embedding_fingerprint: "fp",
        builder_versions,
        status: "built",
    }
}

#[test]
fn catalog_idempotency_is_identity_checked() -> Result<(), StorageError> {
    // r-codex P3 WARN-3: a re-insert of the SAME package_id is a no-op ONLY when every immutable field
    // matches; a changed re-build (different digest) must be REJECTED, not silently kept.
    let Some(pg_config) = discover_pg_config("package catalog identity")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-catalog.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    postgres.run_migrations()?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    let builders = serde_json::json!({"chunker": "c1"});
    // First insert succeeds.
    insert_package_catalog_row(&mut client, &row("core-1-1", "sha256:aaa", &builders))?;
    // Re-insert with the SAME identity is an idempotent no-op.
    insert_package_catalog_row(&mut client, &row("core-1-1", "sha256:aaa", &builders))?;
    let count = postgres
        .execute_sql("SELECT count(*)::text FROM package_catalog WHERE package_id='core-1-1';")?;
    assert_eq!(count.trim(), "1", "identical re-insert is a no-op");

    // A changed re-build (different package digest) under the same package_id is REJECTED.
    let conflict =
        insert_package_catalog_row(&mut client, &row("core-1-1", "sha256:bbb", &builders));
    assert!(
        matches!(conflict, Err(StorageError::PackageCatalog { .. })),
        "a changed re-build is rejected, got {conflict:?}"
    );
    // The original row is untouched.
    let digest = postgres
        .execute_sql("SELECT package_digest FROM package_catalog WHERE package_id='core-1-1';")?;
    assert_eq!(
        digest.trim(),
        "sha256:aaa",
        "the original catalog row is preserved"
    );
    Ok(())
}
