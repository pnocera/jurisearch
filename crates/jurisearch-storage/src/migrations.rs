use crate::runtime::{ManagedPostgres, StorageError, sql_identifier, sql_string_literal};

pub const CURRENT_SCHEMA_VERSION: i32 = 11;

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

const MIGRATIONS: &[Migration] = &[
    Migration {
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
    },
    Migration {
        version: 2,
        name: "chunk_bm25_index",
        sql: r#"
CREATE INDEX IF NOT EXISTS chunks_bm25_idx
ON chunks USING bm25 (chunk_id, body)
WITH (key_field = 'chunk_id');

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 2), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 3,
        name: "ingest_operational_accounting",
        sql: r#"
CREATE TABLE IF NOT EXISTS ingest_run (
    run_id text PRIMARY KEY,
    source text NOT NULL,
    status text NOT NULL CHECK (status IN ('running', 'completed', 'failed', 'aborted')),
    parser_version text NOT NULL,
    schema_version text NOT NULL,
    code_version text NOT NULL,
    safe_mode boolean NOT NULL DEFAULT false,
    archive_plan jsonb NOT NULL DEFAULT '{}'::jsonb,
    manifest jsonb NOT NULL DEFAULT '{}'::jsonb,
    error_message text,
    started_at timestamptz NOT NULL DEFAULT now(),
    completed_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS ingest_member (
    member_id bigserial PRIMARY KEY,
    run_id text NOT NULL REFERENCES ingest_run(run_id) ON DELETE CASCADE,
    archive_name text NOT NULL,
    member_path text NOT NULL,
    source text NOT NULL,
    source_entity text,
    date_anchor date,
    status text NOT NULL CHECK (status IN ('discovered', 'parsed', 'inserted', 'skipped', 'failed')),
    parser_version text NOT NULL,
    schema_version text NOT NULL,
    code_version text NOT NULL,
    source_payload_hash text NOT NULL,
    attempt_count integer NOT NULL DEFAULT 1 CHECK (attempt_count > 0),
    error_count integer NOT NULL DEFAULT 0 CHECK (error_count >= 0),
    last_error_class text,
    last_error_code text,
    last_error_message text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (run_id, archive_name, member_path)
);

CREATE TABLE IF NOT EXISTS ingest_error (
    error_id bigserial PRIMARY KEY,
    run_id text NOT NULL REFERENCES ingest_run(run_id) ON DELETE CASCADE,
    member_id bigint REFERENCES ingest_member(member_id) ON DELETE SET NULL,
    error_class text NOT NULL,
    error_code text NOT NULL,
    message text NOT NULL,
    retry_policy text NOT NULL DEFAULT 'none',
    context jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ingest_member_resume_idx
ON ingest_member (archive_name, member_path, updated_at DESC, member_id DESC);

CREATE INDEX IF NOT EXISTS ingest_member_run_status_idx
ON ingest_member (run_id, status);

CREATE INDEX IF NOT EXISTS ingest_member_payload_compat_idx
ON ingest_member (archive_name, member_path, source_payload_hash, parser_version, schema_version, code_version);

CREATE INDEX IF NOT EXISTS ingest_error_run_class_idx
ON ingest_error (run_id, error_class, error_code);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 3), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 4,
        name: "legi_metadata_roots",
        sql: r#"
CREATE TABLE IF NOT EXISTS legi_metadata_roots (
    metadata_key text PRIMARY KEY,
    root_kind text NOT NULL CHECK (root_kind IN ('TEXTE_VERSION', 'SECTION_TA', 'TEXTELR')),
    source_uid text,
    parent_source_uid text,
    title text,
    valid_from date,
    valid_to date,
    valid_to_raw text,
    source_payload_hash text NOT NULL,
    source_archive text,
    source_member_path text,
    canonical_version text NOT NULL,
    canonical_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS legi_metadata_roots_kind_source_idx
ON legi_metadata_roots (root_kind, source_uid);

CREATE INDEX IF NOT EXISTS legi_metadata_roots_parent_idx
ON legi_metadata_roots (parent_source_uid);

CREATE INDEX IF NOT EXISTS legi_metadata_roots_validity_idx
ON legi_metadata_roots (valid_from, valid_to);

CREATE INDEX IF NOT EXISTS legi_metadata_roots_payload_idx
ON legi_metadata_roots (source_payload_hash);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 4), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 5,
        name: "documents_source_uid_index",
        sql: r#"
CREATE INDEX IF NOT EXISTS documents_source_uid_idx
ON documents (source_uid);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 5), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 6,
        name: "chunk_provenance_columns",
        sql: r#"
ALTER TABLE chunks
    ADD COLUMN IF NOT EXISTS contextualized_body text,
    ADD COLUMN IF NOT EXISTS chunking text NOT NULL DEFAULT 'structural',
    ADD COLUMN IF NOT EXISTS boundary text NOT NULL DEFAULT 'unknown',
    ADD COLUMN IF NOT EXISTS hierarchy_path jsonb NOT NULL DEFAULT '[]'::jsonb;

UPDATE chunks c
SET contextualized_body = COALESCE(
        NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'contextualized_body', ''),
        c.contextualized_body
    ),
    chunking = COALESCE(
        NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'chunking', ''),
        c.chunking
    ),
    boundary = COALESCE(
        NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'boundary', ''),
        c.boundary
    ),
    hierarchy_path = COALESCE(
        d.canonical_json->'chunks'->c.chunk_index->'hierarchy_path',
        c.hierarchy_path
    )
FROM documents d
WHERE d.document_id = c.document_id
  AND jsonb_typeof(d.canonical_json->'chunks') = 'array';

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 6), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 7,
        name: "document_hierarchy_path_index",
        sql: r#"
ALTER TABLE documents
    ADD COLUMN IF NOT EXISTS hierarchy_path jsonb NOT NULL DEFAULT '[]'::jsonb;

UPDATE documents d
SET hierarchy_path = COALESCE(
        CASE
            WHEN jsonb_typeof(d.canonical_json->'hierarchy_path') = 'array'
                THEN d.canonical_json->'hierarchy_path'
            ELSE NULL
        END,
        (
            SELECT c.hierarchy_path
            FROM chunks c
            WHERE c.document_id = d.document_id
              AND jsonb_typeof(c.hierarchy_path) = 'array'
              AND jsonb_array_length(c.hierarchy_path) > 0
            ORDER BY c.chunk_index
            LIMIT 1
        ),
        d.hierarchy_path
    );

CREATE INDEX IF NOT EXISTS documents_context_hierarchy_idx
ON documents (source, kind, (md5(hierarchy_path::text)));

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 7), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 8,
        name: "chunk_contextualized_bm25_index",
        sql: r#"
UPDATE chunks
SET contextualized_body = body
WHERE contextualized_body IS NULL
   OR btrim(contextualized_body) = '';

ALTER TABLE chunks
    ALTER COLUMN contextualized_body SET NOT NULL;

ALTER TABLE chunks
    DROP CONSTRAINT IF EXISTS chunks_contextualized_body_not_empty;

ALTER TABLE chunks
    ADD CONSTRAINT chunks_contextualized_body_not_empty
    CHECK (btrim(contextualized_body) <> '');

DROP INDEX IF EXISTS chunks_bm25_idx;

CREATE INDEX chunks_bm25_idx
ON chunks USING bm25 (chunk_id, contextualized_body)
WITH (key_field = 'chunk_id');

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 8), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 9,
        name: "chunk_french_legal_bm25_analyzer",
        sql: r#"
DROP INDEX IF EXISTS chunks_bm25_idx;

CREATE INDEX chunks_bm25_idx
ON chunks USING bm25 (chunk_id, contextualized_body)
WITH (
    key_field = 'chunk_id',
    text_fields = '{
        "contextualized_body": {
            "tokenizer": {
                "type": "default",
                "ascii_folding": true,
                "stemmer": "French",
                "stopwords_language": "French"
            }
        }
    }'
);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 9), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 10,
        name: "graph_edges_reverse_citation_index",
        // Partial expression index supporting `related --rel cited_by`: reverse citation lookups key
        // on payload->>'to_source_uid' (the seed's source_uid), which `graph_edges_from_idx` cannot
        // serve. Without it, cited_by is a full scan of the ~12.9M-edge table. Partial + expression
        // keeps it small (only resolved publisher CITATION/cible edges). Migrations run inside a
        // transaction, so this is a plain CREATE INDEX (not CONCURRENTLY).
        sql: r#"
CREATE INDEX IF NOT EXISTS graph_edges_publisher_citation_to_source_uid_idx
ON graph_edges ((payload->>'to_source_uid'))
WHERE edge_source = 'publisher'
  AND payload->'attributes' @> '[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb;

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 10), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 11,
        name: "decision_citation_lookup_indexes",
        // Decision citation lookups by ECLI and pourvoi/NUMERO_AFFAIRE were full scans of the
        // ~1.1M-decision `documents` set (measured ECLI ~22s, pourvoi ~108s). Two partial
        // decision-only indexes remove the scans:
        //  - ECLI: expression index on upper(canonical_json->>'ecli'). citation.rs already filters
        //    `kind='decision' AND upper(canonical_json->>'ecli') = …`, which matches this index.
        //  - pourvoi: an IMMUTABLE function returns the dot/space-normalized case_numbers array, and
        //    a GIN index over it serves the rewritten `… @> ARRAY[…]` containment predicate instead
        //    of expanding the jsonb array row by row.
        // Migrations run inside a transaction, so these are plain CREATE INDEX (not CONCURRENTLY).
        sql: r#"
CREATE INDEX IF NOT EXISTS documents_decision_ecli_idx
ON documents (upper(canonical_json->>'ecli'))
WHERE kind = 'decision';

CREATE OR REPLACE FUNCTION jurisearch_normalized_case_numbers(doc jsonb)
RETURNS text[]
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
  SELECT coalesce(
           array_agg(replace(replace(cn, '.', ''), ' ', '')),
           ARRAY[]::text[]
         )
  FROM jsonb_array_elements_text(coalesce(doc->'case_numbers', '[]'::jsonb)) AS cn
$$;

CREATE INDEX IF NOT EXISTS documents_decision_case_numbers_idx
ON documents USING gin (jurisearch_normalized_case_numbers(canonical_json))
WHERE kind = 'decision';

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 11), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
];

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
