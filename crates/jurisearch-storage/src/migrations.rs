use crate::runtime::{ManagedPostgres, StorageError, sql_identifier, sql_string_literal};
use postgres::GenericClient;

/// A reproducible digest over the applied migration set (`version:name` pairs, ordered) — the P3
/// stand-in for the schema-migration bundle (plan P3 WARN-1). The producer stamps it into the manifest
/// (`schema_migration_bundle_digest`) and the consumer recomputes it with THIS SAME function and
/// rejects a mismatch, so a client whose migration set differs from the producer's cannot apply even if
/// its `max(version)` happens to match.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn schema_bundle_digest<C: GenericClient>(client: &mut C) -> Result<String, StorageError> {
    let row = client
        .query_one(
            "SELECT coalesce(string_agg(version || ':' || name, '|' ORDER BY version), '') AS agg \
             FROM schema_migrations;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let agg: String = row.get("agg");
    Ok(jurisearch_package::canonical::digest_bytes(agg.as_bytes()))
}

pub const CURRENT_SCHEMA_VERSION: i32 = 21;

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
    Migration {
        version: 12,
        name: "decision_zones_cache",
        // Lazy Judilibre official-zone cache for `fetch --part --online`. A SEPARATE table (not
        // canonical_json) keeps the immutable bulk ingest/projection and the corpus-level
        // `zone_accurate=false` honesty intact: enrichment is a per-decision overlay that can refresh,
        // cache misses/errors, and be re-fetched without contaminating canonical records. Judicial
        // (Cour de cassation: cass + inca) only for now — Judilibre does not cover Cour d'appel (capp,
        // RG numbers) or administrative (jade/Conseil d'Etat).
        sql: r#"
CREATE TABLE IF NOT EXISTS decision_zones (
    document_id text PRIMARY KEY REFERENCES documents(document_id) ON DELETE CASCADE,
    provider text NOT NULL,
    provider_decision_id text,
    source_uid text NOT NULL,
    ecli text,
    status text NOT NULL CHECK (status IN ('ok','not_found','unsupported','invalid_offsets','upstream_error')),
    fetched_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    upstream_update_date text,
    upstream_decision_date text,
    text_hash text,
    offset_unit text,
    zones_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    raw_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    error text,
    zone_schema_version text NOT NULL DEFAULT 'judilibre:v1'
);

CREATE INDEX IF NOT EXISTS decision_zones_provider_idx
ON decision_zones (provider, provider_decision_id);

CREATE INDEX IF NOT EXISTS decision_zones_ecli_idx
ON decision_zones (upper(ecli))
WHERE ecli IS NOT NULL;

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 12), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 13,
        name: "zone_units",
        // Option B parallel zone-retrieval subsystem (work/03-implementation/04-zones). Official
        // Judilibre zone fragments materialized as first-class retrieval units, SEPARATE from the
        // bulk `chunks` corpus so the proven whole-decision retrieval path and the Phase 2
        // `zone_accurate=false` honesty invariant are untouched. Derived from `decision_zones`
        // (per-decision overlay); Cour de cassation only (cass + inca). `search_body` is the
        // BM25-analyzed field (mirrors the v8/v9 `chunks.contextualized_body` contract); `text_hash`
        // is the `decision_zones.text_hash` snapshot; `zone_unit_builder_version` forces a rebuild on
        // a derivation-logic change.
        sql: r#"
CREATE TABLE IF NOT EXISTS zone_units (
    zone_unit_id text PRIMARY KEY,
    document_id text NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    zone text NOT NULL CHECK (zone IN
        ('motivations','moyens','dispositif','expose','introduction','annexes')),
    fragment_index integer NOT NULL CHECK (fragment_index >= 0),
    body text NOT NULL,
    search_body text NOT NULL CHECK (btrim(search_body) <> ''),
    provider text NOT NULL DEFAULT 'judilibre',
    zone_accurate boolean NOT NULL DEFAULT true,
    source text NOT NULL,
    text_hash text NOT NULL,
    zone_unit_builder_version text NOT NULL,
    zone_schema_version text NOT NULL DEFAULT 'judilibre:v1',
    embedding_fingerprint text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (document_id, zone, fragment_index)
);

CREATE INDEX IF NOT EXISTS zone_units_document_idx ON zone_units(document_id);
CREATE INDEX IF NOT EXISTS zone_units_zone_idx ON zone_units(zone);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 13), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 14,
        name: "zone_unit_embeddings",
        // Dense space for zone units — SEPARATE physical table/index from `chunk_embeddings` (Option B
        // isolation). Same locked bge-m3:1024:normalize:true fingerprint. The ivfflat index is built
        // at finalize time (after backfill, lists sized to corpus), not here, mirroring how the chunk
        // ivfflat index is a finalize step rather than a base migration.
        sql: r#"
CREATE TABLE IF NOT EXISTS zone_unit_embeddings (
    zone_unit_id text PRIMARY KEY REFERENCES zone_units(zone_unit_id) ON DELETE CASCADE,
    embedding_fingerprint text NOT NULL,
    embedding vector(1024) NOT NULL,
    model text NOT NULL,
    dimension integer NOT NULL CHECK (dimension = 1024),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS zone_unit_embeddings_fingerprint_idx
ON zone_unit_embeddings(embedding_fingerprint);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 14), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 15,
        name: "zone_units_bm25_index",
        // Lexical space for zone units — SEPARATE pg_search BM25 index from `chunks_bm25_idx`, over
        // `search_body`, using the SAME French legal analyzer as the current v9 chunk contract
        // (ascii_folding + French stemmer + French stopwords). Mirrors migrations.rs v9 so zone search
        // is analyzer-equivalent for accents/morphology/French legal terms.
        sql: r#"
CREATE INDEX IF NOT EXISTS zone_units_bm25_idx
ON zone_units USING bm25 (zone_unit_id, search_body)
WITH (
    key_field = 'zone_unit_id',
    text_fields = '{
        "search_body": {
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
VALUES ('schema', jsonb_build_object('schema_version', 15), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 16,
        name: "official_api_responses_archive",
        // Durable, append-only archive of EVERY official-API exchange (Judilibre /search + /decision,
        // Legifrance search, and 'local' no-request accounting). SEPARATE from the TTL'd `decision_zones`
        // cache: `decision_zones` is latest-cache state that can expire/refresh/be invalidated, whereas
        // this table is permanent provenance/evidence — quota-limited PISTE responses we never want to
        // re-fetch or lose. Deliberately NO FK to `documents` (durability over relational cleanliness:
        // the archive must survive cache invalidation, index repair, or document churn). Stores the raw
        // response body text (byte-faithful) AND the parsed jsonb (for querying) AND a sha256 of the body.
        sql: r#"
CREATE TABLE IF NOT EXISTS official_api_responses (
    response_id bigserial PRIMARY KEY,
    provider text NOT NULL CHECK (provider IN ('judilibre','legifrance','local')),
    api_environment text NOT NULL DEFAULT 'production',
    endpoint text NOT NULL,
    http_method text NOT NULL CHECK (http_method IN ('GET','POST','LOCAL')),
    subject_document_id text,
    subject_source_uid text,
    provider_object_id text,
    citation_key text,
    request_fingerprint text NOT NULL,
    request_url text,
    request_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    request_body text,
    outcome text NOT NULL CHECK (outcome IN ('ok','not_found','unsupported','upstream_error','parse_error')),
    http_status integer,
    response_body text NOT NULL DEFAULT '',
    response_json jsonb,
    response_body_sha256 text NOT NULL,
    error text,
    run_id text,
    code_version text,
    fetched_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS official_api_responses_subject_idx
ON official_api_responses (subject_document_id, fetched_at DESC);

CREATE INDEX IF NOT EXISTS official_api_responses_provider_request_idx
ON official_api_responses (provider, endpoint, request_fingerprint, fetched_at DESC);

CREATE INDEX IF NOT EXISTS official_api_responses_provider_object_idx
ON official_api_responses (provider, provider_object_id, fetched_at DESC)
WHERE provider_object_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS official_api_responses_citation_key_idx
ON official_api_responses (citation_key, fetched_at DESC)
WHERE citation_key IS NOT NULL;

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 16), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 17,
        name: "decision_legislation_citations",
        // Legislation enrichment (slice 2): decisions cite legislation in their Judilibre `visa`
        // (article + code, e.g. "Article 609 du code de procédure civile"), and MANY decisions cite the
        // SAME article — so citations are extracted from the archived /decision responses
        // (official_api_responses) into per-decision OCCURRENCES, then DEDUPED by a normalized
        // `citation_key` into unique RESOLUTIONS that are resolved against the Legifrance API exactly
        // ONCE each (the Legifrance response itself lands in official_api_responses, provider='legifrance').
        // This keeps the quota-limited upstream evidence durable without re-calling per occurrence.
        sql: r#"
CREATE TABLE IF NOT EXISTS decision_legislation_citations (
    citation_occurrence_id text PRIMARY KEY,
    decision_document_id text NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    decision_source_uid text NOT NULL,
    source_response_id bigint NOT NULL REFERENCES official_api_responses(response_id) ON DELETE CASCADE,
    visa_index integer NOT NULL CHECK (visa_index >= 0),
    citation_key text NOT NULL,
    article_number_raw text,
    article_number_norm text NOT NULL,
    code_name_raw text,
    code_name_norm text NOT NULL,
    canonical_query text NOT NULL,
    legifrance_url text,
    raw_title text NOT NULL,
    extraction_method text NOT NULL CHECK (extraction_method IN
        ('legifrance_url_query','visa_title_regex')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (decision_document_id, visa_index, citation_key)
);

CREATE INDEX IF NOT EXISTS decision_legislation_citations_decision_idx
ON decision_legislation_citations (decision_document_id);

CREATE INDEX IF NOT EXISTS decision_legislation_citations_citation_key_idx
ON decision_legislation_citations (citation_key);

CREATE TABLE IF NOT EXISTS legislation_citation_resolutions (
    citation_key text PRIMARY KEY,
    article_number_norm text NOT NULL,
    code_name_norm text NOT NULL,
    canonical_query text NOT NULL,
    occurrence_count integer NOT NULL DEFAULT 0,
    legifrance_status text NOT NULL DEFAULT 'pending' CHECK (legifrance_status IN
        ('pending','ok','not_found','upstream_error','parse_error')),
    legifrance_response_id bigint REFERENCES official_api_responses(response_id) ON DELETE SET NULL,
    legifrance_request_fingerprint text,
    fetched_at timestamptz,
    error text,
    resolution_schema_version text NOT NULL DEFAULT 'legislation-citation:v1',
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS legislation_citation_resolutions_status_idx
ON legislation_citation_resolutions (legifrance_status);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 17), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 18,
        name: "corpus_attribution",
        // Plan P0 "Corpus attribution": every replicated row resolves to exactly one corpus
        // (design §4.1, §5.1). Most rows are attributed via the *owning document's* `source`:
        // `documents.corpus` is a STORED generated column derived from `source`, and the
        // document/decision-owned tables (chunks, embeddings, zone_units, decision_legislation_
        // citations) inherit their corpus through their owning document at outbox-emit time (P1).
        //
        // Two replicated tables have NO guaranteed owning-document link and so need an *explicit*
        // `corpus` column (the "explicit column where not [derivable from source]" half of P0):
        //   - `official_api_responses`  — `subject_document_id` is nullable (Legifrance citation
        //     lookups have none); corpus is derived from the subject document, else from the
        //     citation's occurrences.
        //   - `legislation_citation_resolutions` — keyed by a `citation_key` deduped across
        //     decisions; corpus is a property of the citation's occurrences, not a single decision.
        // For these, the storage *writers* derive corpus in-SQL from the authoritative links and the
        // column is `NOT NULL`, so a genuinely unattributable row FAILS LOUDLY at insert. The
        // backfill below additionally uses a single-corpus-DB fallback — a one-time bootstrap that is
        // valid because, when the whole DB holds one corpus, an unlinked archive row can belong to
        // no other (the runtime writers deliberately do NOT use this fallback).
        //
        // The `documents` `CASE` MUST stay in lock-step with `jurisearch_package::corpus::
        // KNOWN_SOURCES`; `migration_18_case_matches_known_sources` enforces that. Every
        // `*_corpus_attributed` CHECK makes an unmapped/unlinkable row FAIL LOUDLY.
        sql: r#"
ALTER TABLE documents
    ADD COLUMN corpus text
    GENERATED ALWAYS AS (
        CASE source
            WHEN 'legi' THEN 'core'
            WHEN 'cass' THEN 'core'
            WHEN 'capp' THEN 'core'
            WHEN 'inca' THEN 'core'
            WHEN 'jade' THEN 'core'
        END
    ) STORED;

ALTER TABLE documents
    ADD CONSTRAINT documents_corpus_attributed CHECK (corpus IS NOT NULL);

CREATE INDEX IF NOT EXISTS documents_corpus_idx ON documents(corpus);

-- official_api_responses: explicit corpus, backfilled from authoritative links.
ALTER TABLE official_api_responses ADD COLUMN corpus text;

-- (1) direct subject_document_id link
UPDATE official_api_responses r
SET corpus = d.corpus
FROM documents d
WHERE r.corpus IS NULL AND r.subject_document_id = d.document_id;

-- (2) via the citation's occurrences (a /decision or Legifrance response tied to a citation_key),
--     only when those occurrences resolve to exactly one corpus
UPDATE official_api_responses r
SET corpus = sub.corpus
FROM (
    SELECT c.citation_key AS citation_key, max(d.corpus) AS corpus
    FROM decision_legislation_citations c
    JOIN documents d ON d.document_id = c.decision_document_id
    GROUP BY c.citation_key
    HAVING count(DISTINCT d.corpus) = 1
) sub
WHERE r.corpus IS NULL AND r.citation_key = sub.citation_key;

-- (3) single-corpus-DB bootstrap fallback (migration-only): in a one-corpus DB an unlinked archive
--     row can belong to no other corpus
UPDATE official_api_responses
SET corpus = (SELECT min(corpus) FROM documents)
WHERE corpus IS NULL
  AND (SELECT count(DISTINCT corpus) FROM documents) = 1;

ALTER TABLE official_api_responses ALTER COLUMN corpus SET NOT NULL;
ALTER TABLE official_api_responses
    ADD CONSTRAINT official_api_responses_corpus_attributed CHECK (corpus IS NOT NULL);
CREATE INDEX IF NOT EXISTS official_api_responses_corpus_idx ON official_api_responses(corpus);

-- legislation_citation_resolutions: explicit corpus, derived from its occurrences.
ALTER TABLE legislation_citation_resolutions ADD COLUMN corpus text;

UPDATE legislation_citation_resolutions res
SET corpus = sub.corpus
FROM (
    SELECT c.citation_key AS citation_key, max(d.corpus) AS corpus
    FROM decision_legislation_citations c
    JOIN documents d ON d.document_id = c.decision_document_id
    GROUP BY c.citation_key
    HAVING count(DISTINCT d.corpus) = 1
) sub
WHERE res.corpus IS NULL AND res.citation_key = sub.citation_key;

UPDATE legislation_citation_resolutions
SET corpus = (SELECT min(corpus) FROM documents)
WHERE corpus IS NULL
  AND (SELECT count(DISTINCT corpus) FROM documents) = 1;

ALTER TABLE legislation_citation_resolutions ALTER COLUMN corpus SET NOT NULL;
ALTER TABLE legislation_citation_resolutions
    ADD CONSTRAINT legislation_citation_resolutions_corpus_attributed CHECK (corpus IS NOT NULL);

-- Re-key resolutions by (corpus, citation_key): a resolution (and its Legifrance archive) is
-- replicated per-corpus data and per-corpus physical generations (INV-4) cannot share one global
-- row, so the SAME legislation article cited from two corpora gets an INDEPENDENT resolution per
-- corpus. This also removes the `ON CONFLICT (citation_key) DO NOTHING` cross-corpus blind spot:
-- a later corpus's occurrence creates its own (corpus, citation_key) row rather than silently
-- inheriting the first corpus's attribution.
ALTER TABLE legislation_citation_resolutions
    DROP CONSTRAINT legislation_citation_resolutions_pkey;
ALTER TABLE legislation_citation_resolutions
    ADD PRIMARY KEY (corpus, citation_key);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 18), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 19,
        name: "package_change_log_outbox",
        // Plan P1 "the outbox" (design §5.1): a semantic change ledger written transactionally at the
        // projection boundaries, so incremental package diffs are computable without a uniform
        // `updated_at` (C7) and without snapshot diffing or logical decoding as the primary path.
        //
        // `change_seq` is a GLOBAL build/audit ordering across all corpora — NOT the per-corpus package
        // sequence (that is assigned at build time from the catalog). The ledger records the SCOPE
        // touched (not necessarily full row bodies); the builder rematerialises payloads from the
        // authoritative tables at build time. `op` carries the three event-kind semantics.
        sql: r#"
CREATE TABLE IF NOT EXISTS package_change_log (
    change_seq            bigserial PRIMARY KEY,
    corpus                text NOT NULL,
    ingest_run_id         text NOT NULL,
    table_name            text NOT NULL,
    op                    text NOT NULL CHECK (op IN ('upsert','delete','replace_set')),
    scope_kind            text NOT NULL,
    scope_key             text NOT NULL,
    row_pk                jsonb NOT NULL DEFAULT '{}'::jsonb,
    row_hash              text,
    before_hash           text,
    after_hash            text,
    payload               jsonb,
    builder_versions      jsonb NOT NULL DEFAULT '{}'::jsonb,
    embedding_fingerprint text,
    schema_version        integer NOT NULL,
    created_at            timestamptz NOT NULL DEFAULT now()
);

-- The read API filters by corpus and orders/bounds by change_seq.
CREATE INDEX IF NOT EXISTS package_change_log_corpus_seq_idx
ON package_change_log (corpus, change_seq);

-- Audit by ingest run.
CREATE INDEX IF NOT EXISTS package_change_log_run_idx
ON package_change_log (ingest_run_id);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 19), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 20,
        name: "client_storage_topology",
        // Plan P2 "client storage topology" (design §4, §7.2): the client-side namespaces + control
        // schema. The CLIENT serves replicated data from per-corpus PHYSICAL generation schemas
        // (`jurisearch_server_<corpus>_gNNNN`, created dynamically by the `generations` module, NOT
        // here) fronted by stable `jurisearch_server` views; this migration creates the global,
        // never-swapped namespaces and the control cursor/registry.
        //
        // DDL classification (the §4.2 / codex P2 boundary): the replicated *data* tables are emitted
        // per-generation (qualified) by the `generations` module; `jurisearch_control`,
        // `jurisearch_app`, `jurisearch_server` (views), and the global `package_change_log` /
        // `index_manifest` / `schema_migrations` / `ingest_*` live in `public`/control and are NEVER
        // per-generation. The PRODUCER keeps writing its authoritative working set in `public`
        // (design: producer mutable, client applies generations).
        sql: r#"
CREATE SCHEMA IF NOT EXISTS jurisearch_server;
CREATE SCHEMA IF NOT EXISTS jurisearch_control;
CREATE SCHEMA IF NOT EXISTS jurisearch_app;

-- The single authority on client position, one row per installed corpus (design §7.2). The ONLY
-- writer is the cursor-authority module (conception §4.2).
CREATE TABLE IF NOT EXISTS jurisearch_control.corpus_state (
    corpus                text PRIMARY KEY,
    active_generation     text NOT NULL,
    sequence              bigint NOT NULL,
    baseline_id           text NOT NULL,
    schema_version        integer NOT NULL,
    embedding_fingerprint text NOT NULL,
    builder_versions      jsonb NOT NULL DEFAULT '{}'::jsonb,
    last_package_id       text,
    last_package_digest   text,
    applied_at            timestamptz NOT NULL DEFAULT now()
);

-- Tracks every per-corpus physical generation for rollback + async cleanup (design §7.2).
CREATE TABLE IF NOT EXISTS jurisearch_control.generation_registry (
    corpus              text NOT NULL,
    generation          text NOT NULL,
    physical_schema     text NOT NULL,
    state               text NOT NULL CHECK (state IN ('building','active','retired','failed')),
    source_baseline_id  text,
    source_package_id   text,
    validation_digest   text,
    created_at          timestamptz NOT NULL DEFAULT now(),
    activated_at        timestamptz,
    retired_at          timestamptz,
    PRIMARY KEY (corpus, generation)
);

-- At most one ACTIVE generation per corpus (the stable views point at exactly one).
CREATE UNIQUE INDEX IF NOT EXISTS generation_registry_one_active_per_corpus
ON jurisearch_control.generation_registry (corpus) WHERE state = 'active';

CREATE INDEX IF NOT EXISTS generation_registry_state_idx
ON jurisearch_control.generation_registry (state);

-- Empty stable views for every replicated relation, so a freshly-migrated client (no active
-- generation yet) has a complete `jurisearch_server` namespace returning zero rows of the correct
-- shape — never a "relation does not exist" and never stale public data. `generations::
-- rebuild_server_views` repoints these to the active generation(s) on activation.
CREATE OR REPLACE VIEW jurisearch_server.documents AS SELECT * FROM public.documents WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.chunks AS SELECT * FROM public.chunks WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.chunk_embeddings AS SELECT * FROM public.chunk_embeddings WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.graph_edges AS SELECT * FROM public.graph_edges WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.legi_metadata_roots AS SELECT * FROM public.legi_metadata_roots WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.zone_units AS SELECT * FROM public.zone_units WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.zone_unit_embeddings AS SELECT * FROM public.zone_unit_embeddings WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.decision_zones AS SELECT * FROM public.decision_zones WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.decision_legislation_citations AS SELECT * FROM public.decision_legislation_citations WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.legislation_citation_resolutions AS SELECT * FROM public.legislation_citation_resolutions WHERE false;
CREATE OR REPLACE VIEW jurisearch_server.official_api_responses AS SELECT * FROM public.official_api_responses WHERE false;

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 20), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
"#,
    },
    Migration {
        version: 21,
        name: "producer_package_catalog",
        // Plan P3 (D5, design §5.1 "two sequence layers"): the PRODUCER-side catalog mapping each
        // per-corpus `package_sequence` to the frozen global `change_seq` window it was built from,
        // plus the chain link + compatibility stamps + build/publish status. This is the bridge that
        // keeps the per-corpus package sequence distinct from the global change_seq (so a cross-corpus
        // gap is never a false `sequence_gap`). Producer-only; the client never reads it. Lives in
        // `public` (control/operational, never per-generation).
        sql: r#"
CREATE TABLE IF NOT EXISTS package_catalog (
    corpus                   text    NOT NULL,
    package_sequence         bigint  NOT NULL,
    package_id               text    NOT NULL,
    package_kind             text    NOT NULL CHECK (package_kind IN ('baseline','rebaseline','incremental')),
    baseline_id              text    NOT NULL,
    generation               text    NOT NULL,
    -- The frozen global change_seq high-water mark this package was built from, so the next
    -- incremental has a well-defined `lo` to diff from (design §5.1). NOT NULL even for a baseline.
    included_change_seq_high bigint  NOT NULL,
    previous_package_id      text,
    previous_package_digest  text,
    -- Integrity + compatibility stamps frozen at build (reserved now to avoid P4 churn).
    package_digest           text,
    manifest_digest          text,
    schema_version           integer NOT NULL,
    embedding_fingerprint    text    NOT NULL,
    builder_versions         jsonb   NOT NULL DEFAULT '{}'::jsonb,
    status                   text    NOT NULL DEFAULT 'built'
                                     CHECK (status IN ('built','published','failed')),
    created_at               timestamptz NOT NULL DEFAULT now(),
    published_at             timestamptz,
    updated_at               timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (corpus, package_sequence)
);

-- A package_id is globally unique (it is the artifact identity + chain link target).
CREATE UNIQUE INDEX IF NOT EXISTS package_catalog_package_id_idx
ON package_catalog (package_id);

-- The newest catalog row per corpus drives the next build's `lo` and chain link.
CREATE INDEX IF NOT EXISTS package_catalog_corpus_seq_idx
ON package_catalog (corpus, package_sequence DESC);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 21), now())
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

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_package::corpus::{KNOWN_SOURCES, corpus_for_source};

    fn migration_sql(version: i32) -> &'static str {
        MIGRATIONS
            .iter()
            .find(|m| m.version == version)
            .map(|m| m.sql)
            .unwrap_or_else(|| panic!("migration {version} not found"))
    }

    #[test]
    fn migration_list_is_valid() {
        validate_migration_list().expect("migration list must validate");
    }

    /// The v18 `documents.corpus` backfill `CASE` is the SQL projection of the single source of
    /// truth `jurisearch_package::corpus::KNOWN_SOURCES`. This guards drift: a new source added to
    /// the contract crate without extending the migration `CASE` (or vice versa) fails here, before
    /// it can ship an unattributable row.
    #[test]
    fn migration_18_case_matches_known_sources() {
        let sql = migration_sql(18);
        // Every contract-known source maps to its corpus in the SQL CASE.
        for (source, corpus) in KNOWN_SOURCES {
            let clause = format!("WHEN '{source}' THEN '{corpus}'");
            assert!(
                sql.contains(&clause),
                "migration 18 CASE is missing `{clause}` (out of sync with KNOWN_SOURCES)"
            );
            // …and the contract rule agrees on the same corpus.
            assert_eq!(
                corpus_for_source(source).unwrap().as_str(),
                *corpus,
                "KNOWN_SOURCES and corpus_for_source disagree for `{source}`"
            );
        }
        // No *extra* WHEN clauses beyond the known sources (the migration cannot attribute a source
        // the contract does not know about).
        let when_clauses = sql.matches("WHEN '").count();
        assert_eq!(
            when_clauses,
            KNOWN_SOURCES.len(),
            "migration 18 CASE has {when_clauses} WHEN clauses but KNOWN_SOURCES has {} \
             — they must match exactly",
            KNOWN_SOURCES.len()
        );
    }

    #[test]
    fn migration_18_fails_loudly_on_unattributed_rows() {
        let sql = migration_sql(18);
        // Each replicated table that carries a `corpus` has a CHECK that makes an unmapped/unlinkable
        // row abort the backfill / reject an insert (plan P0 "ambiguous rows fail loudly").
        assert!(sql.contains("documents_corpus_attributed CHECK (corpus IS NOT NULL)"));
        assert!(
            sql.contains("official_api_responses_corpus_attributed CHECK (corpus IS NOT NULL)")
        );
        assert!(sql.contains(
            "legislation_citation_resolutions_corpus_attributed CHECK (corpus IS NOT NULL)"
        ));
    }

    #[test]
    fn migration_18_attributes_non_document_owned_tables() {
        let sql = migration_sql(18);
        // The two tables without a guaranteed owning document get an explicit, NOT NULL corpus column
        // (the "explicit column where not derivable from source" half of P0).
        assert!(sql.contains("ALTER TABLE official_api_responses ADD COLUMN corpus text"));
        assert!(
            sql.contains("ALTER TABLE official_api_responses ALTER COLUMN corpus SET NOT NULL")
        );
        assert!(
            sql.contains("ALTER TABLE legislation_citation_resolutions ADD COLUMN corpus text")
        );
        assert!(sql.contains(
            "ALTER TABLE legislation_citation_resolutions ALTER COLUMN corpus SET NOT NULL"
        ));
    }
}
