use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};

#[test]
fn creates_pg_search_and_vector_extensions_when_assets_are_available() -> Result<(), StorageError> {
    let pg_config = match PgConfig::discover() {
        Ok(pg_config) => pg_config,
        Err(error @ StorageError::MissingPgConfig { .. }) => {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping PostgreSQL extension smoke: {error}");
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    for extension in ["pg_search", "vector"] {
        if let Err(error) = pg_config.require_extension_assets(extension) {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping PostgreSQL extension smoke: {error}");
            return Ok(());
        }
    }

    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let extensions = postgres.execute_sql(
        "CREATE EXTENSION vector; \
         CREATE EXTENSION pg_search; \
         SELECT extname || ':' || extversion \
         FROM pg_extension \
         WHERE extname IN ('vector', 'pg_search') \
         ORDER BY extname;",
    )?;
    assert!(extensions.contains("pg_search:"));
    assert!(extensions.contains("vector:"));

    let nearest = postgres.execute_sql(
        "CREATE TABLE docs (id serial PRIMARY KEY, body text, embedding vector(3)); \
         INSERT INTO docs (body, embedding) VALUES \
           ('responsabilite civile article 1240', '[1,0,0]'), \
           ('recette de tarte aux pommes', '[0,1,0]'); \
         SELECT body FROM docs ORDER BY embedding <-> '[1,0,0]' LIMIT 1;",
    )?;
    assert_eq!(nearest, "responsabilite civile article 1240");
    Ok(())
}
