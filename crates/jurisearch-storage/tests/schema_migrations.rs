use jurisearch_storage::{
    migrations::CURRENT_SCHEMA_VERSION,
    runtime::{ManagedPostgres, PgConfig, StorageError},
};

fn discover_pg_config() -> Result<Option<PgConfig>, StorageError> {
    let pg_config = match PgConfig::discover() {
        Ok(pg_config) => pg_config,
        Err(error @ StorageError::MissingPgConfig { .. }) => {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping schema migration smoke: {error}");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    for extension in ["pg_search", "vector"] {
        if let Err(error) = pg_config.require_extension_assets(extension) {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping schema migration smoke: {error}");
            return Ok(None);
        }
    }

    Ok(Some(pg_config))
}

fn vector_literal(active_index: usize) -> String {
    let values = (0..1024)
        .map(|index| if index == active_index { "1" } else { "0" })
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

#[test]
fn migrations_install_minimal_schema_and_are_idempotent() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config()? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-schema-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let query_vector = vector_literal(0);
    let unrelated_vector = vector_literal(1);

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        let report = postgres.run_migrations()?;
        assert_eq!(report.current_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(report.applied, vec![CURRENT_SCHEMA_VERSION]);

        let migrations = postgres.execute_sql(
            "SELECT version::text || ':' || name \
             FROM schema_migrations \
             ORDER BY version;",
        )?;
        assert!(migrations.contains(&format!(
            "{CURRENT_SCHEMA_VERSION}:canonical_documents_chunks_vectors"
        )));

        postgres.execute_sql(&format!(
            "INSERT INTO documents \
               (document_id, source, kind, source_uid, version_group, citation, title, body, \
                valid_from, source_payload_hash, canonical_json) \
             VALUES \
               ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                'LEGIARTI000006419320', 'LEGIARTI000006419320', 'Code civil article 1240', \
                'Article 1240', 'Tout fait quelconque de l''homme...', '1804-02-21', \
                'sha256:article-1240', '{{\"official\":true}}'), \
               ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
                'LEGIARTI000000000001', 'LEGIARTI000000000001', 'Code de cuisine article 1', \
                'Article cuisine', 'Recette sans rapport juridique', '2024-01-01', \
                'sha256:recipe', '{{\"official\":true}}'); \
             INSERT INTO chunks \
               (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                chunk_builder_version, embedding_fingerprint) \
             VALUES \
               ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                'responsabilite civile article 1240', 'sha256:article-1240', \
                'chunker:v0', 'bge-m3:1024:normalize:true'), \
               ('chunk:recipe:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
                'recette de tarte aux pommes', 'sha256:recipe', \
                'chunker:v0', 'bge-m3:1024:normalize:true'); \
             INSERT INTO chunk_embeddings \
               (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
               ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024), \
               ('chunk:recipe:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
            query_vector, unrelated_vector
        ))?;
    }

    {
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        let migration_count =
            postgres.execute_sql("SELECT count(*)::text FROM schema_migrations;")?;
        assert_eq!(migration_count, "1");

        let nearest = postgres.execute_sql(&format!(
            "SELECT c.body \
             FROM chunks c \
             JOIN chunk_embeddings e ON e.chunk_id = c.chunk_id \
             ORDER BY e.embedding <-> '{}' \
             LIMIT 1;",
            query_vector
        ))?;
        assert_eq!(nearest, "responsabilite civile article 1240");

        let manifest_version = postgres.execute_sql(
            "SELECT value->>'schema_version' FROM index_manifest WHERE key = 'schema';",
        )?;
        assert_eq!(manifest_version, CURRENT_SCHEMA_VERSION.to_string());
    }

    Ok(())
}
