mod common;

use common::discover_pg_config;
use jurisearch_storage::runtime::{ManagedPostgres, StorageError};

#[test]
fn durable_lifecycle_restarts_and_rejects_concurrent_owner() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("durable PostgreSQL lifecycle smoke")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-durable-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        let extensions = postgres.execute_sql(
            "SELECT extname || ':' || extversion \
             FROM pg_extension \
             WHERE extname IN ('vector', 'pg_search') \
             ORDER BY extname;",
        )?;
        assert!(extensions.contains("pg_search:"));
        assert!(extensions.contains("vector:"));

        let second = ManagedPostgres::start_durable(pg_config.clone(), root.path());
        assert!(matches!(second, Err(StorageError::StorageLockBusy { .. })));

        postgres.execute_sql(
            "CREATE TABLE docs (id serial PRIMARY KEY, body text, embedding vector(3)); \
             INSERT INTO docs (body, embedding) VALUES \
               ('responsabilite civile article 1240', '[1,0,0]'), \
               ('recette de tarte aux pommes', '[0,1,0]');",
        )?;
    }

    {
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        let nearest = postgres
            .execute_sql("SELECT body FROM docs ORDER BY embedding <-> '[1,0,0]' LIMIT 1;")?;
        assert_eq!(nearest, "responsabilite civile article 1240");
    }

    Ok(())
}
