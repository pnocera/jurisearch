use jurisearch_storage::runtime::{PgConfig, StorageError};

/// Discover a usable PostgreSQL config for a PG-gated integration test, or `None` (skip) when no
/// extension assets are available — unless `JURISEARCH_REQUIRE_PG_EXTENSIONS` forces a hard failure.
/// Mirrors the storage crate's test harness so the pipeline tests gate the same way.
#[allow(dead_code)]
pub fn discover_pg_config(test_name: &str) -> Result<Option<PgConfig>, StorageError> {
    let pg_config = match PgConfig::discover() {
        Ok(pg_config) => pg_config,
        Err(error @ StorageError::MissingPgConfig { .. }) => {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping {test_name}: {error}");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    for extension in ["pg_search", "vector"] {
        if let Err(error) = pg_config.require_extension_assets(extension) {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping {test_name}: {error}");
            return Ok(None);
        }
    }

    Ok(Some(pg_config))
}
