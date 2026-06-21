use crate::runtime::{ManagedPostgres, StorageError, sql_identifier, sql_string_literal};

pub const CURRENT_SCHEMA_VERSION: i32 = 1;

struct Migration {
    version: i32,
    name: &'static str,
    sql: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// All schema versions present after the migration run, including versions
    /// that were already present before this call.
    pub applied: Vec<i32>,
    pub current_version: i32,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "canonical_documents_chunks_vectors",
    sql: r#"
CREATE EXTENSION IF NOT EXISTS vector;
CREATE EXTENSION IF NOT EXISTS pg_search;

CREATE TABLE IF NOT EXISTS documents (
    document_id text PRIMARY KEY,
    source text NOT NULL,
    kind text NOT NULL CHECK (kind IN ('article', 'decision')),
    source_uid text NOT NULL,
    version_group text,
    citation text,
    title text,
    body text NOT NULL,
    valid_from date,
    valid_to date,
    valid_to_raw text,
    source_url text,
    source_payload_hash text NOT NULL,
    canonical_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS chunks (
    chunk_id text PRIMARY KEY,
    document_id text NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    chunk_index integer NOT NULL CHECK (chunk_index >= 0),
    body text NOT NULL,
    chunk_kind text NOT NULL DEFAULT 'body',
    source_fields jsonb NOT NULL DEFAULT '[]'::jsonb,
    source_payload_hash text NOT NULL,
    chunk_builder_version text NOT NULL,
    embedding_fingerprint text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (document_id, chunk_index)
);

CREATE TABLE IF NOT EXISTS chunk_embeddings (
    chunk_id text PRIMARY KEY REFERENCES chunks(chunk_id) ON DELETE CASCADE,
    embedding_fingerprint text NOT NULL,
    embedding vector(1024) NOT NULL,
    model text NOT NULL,
    dimension integer NOT NULL CHECK (dimension = 1024),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS graph_edges (
    edge_id text PRIMARY KEY,
    from_document_id text REFERENCES documents(document_id) ON DELETE CASCADE,
    to_document_id text REFERENCES documents(document_id) ON DELETE CASCADE,
    edge_kind text NOT NULL,
    edge_source text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS index_manifest (
    key text PRIMARY KEY,
    value jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS documents_kind_idx ON documents(kind);
CREATE INDEX IF NOT EXISTS documents_validity_idx ON documents(valid_from, valid_to);
CREATE INDEX IF NOT EXISTS chunks_document_idx ON chunks(document_id, chunk_index);
CREATE INDEX IF NOT EXISTS chunk_embeddings_fingerprint_idx ON chunk_embeddings(embedding_fingerprint);
CREATE INDEX IF NOT EXISTS graph_edges_from_idx ON graph_edges(from_document_id);
CREATE INDEX IF NOT EXISTS graph_edges_to_idx ON graph_edges(to_document_id);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 1), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
}];

impl ManagedPostgres {
    pub fn run_migrations(&self) -> Result<MigrationReport, StorageError> {
        validate_migration_list()?;
        self.execute_sql(
            "CREATE TABLE IF NOT EXISTS schema_migrations (\
             version integer PRIMARY KEY, \
             name text NOT NULL, \
             applied_at timestamptz NOT NULL DEFAULT now()\
             );",
        )?;

        let applied_versions =
            self.execute_sql("SELECT version::text FROM schema_migrations ORDER BY version;")?;
        let mut applied = applied_versions
            .lines()
            .map(|line| {
                line.parse::<i32>()
                    .map_err(|error| StorageError::MigrationPlan {
                        message: format!("invalid schema_migrations version `{line}`: {error}"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(database_version) = applied.iter().copied().max()
            && database_version > CURRENT_SCHEMA_VERSION
        {
            return Err(StorageError::SchemaVersionAhead {
                database_version,
                binary_version: CURRENT_SCHEMA_VERSION,
            });
        }

        for migration in MIGRATIONS {
            if applied.contains(&migration.version) {
                continue;
            }
            self.execute_sql(&format!(
                "BEGIN;\n{}\nINSERT INTO {} (version, name) VALUES ({}, {});\nCOMMIT;",
                migration.sql,
                sql_identifier("schema_migrations"),
                migration.version,
                sql_string_literal(migration.name)
            ))?;
            applied.push(migration.version);
        }

        applied.sort_unstable();
        Ok(MigrationReport {
            applied,
            current_version: CURRENT_SCHEMA_VERSION,
        })
    }
}

fn validate_migration_list() -> Result<(), StorageError> {
    for (expected, migration) in (1..).zip(MIGRATIONS.iter()) {
        if migration.version != expected {
            return Err(StorageError::MigrationPlan {
                message: format!(
                    "migration versions must be contiguous from 1; expected {expected}, found {}",
                    migration.version
                ),
            });
        }
    }

    let latest = MIGRATIONS
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0);
    if latest != CURRENT_SCHEMA_VERSION {
        return Err(StorageError::MigrationPlan {
            message: format!(
                "CURRENT_SCHEMA_VERSION ({CURRENT_SCHEMA_VERSION}) must match latest migration ({latest})"
            ),
        });
    }
    Ok(())
}
