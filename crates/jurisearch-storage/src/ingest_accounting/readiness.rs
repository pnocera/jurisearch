//! Query-readiness + projection/embedding coverage metrics and the readiness cache.

use super::*;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestReadinessReport {
    pub projection_coverage: CoverageMetric,
    pub embedding_coverage: CoverageMetric,
}

pub fn load_ingest_readiness(
    postgres: &ManagedPostgres,
) -> Result<IngestReadinessReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_readiness_metrics(&mut client)
}

pub fn load_ingest_projection_coverage(
    postgres: &ManagedPostgres,
) -> Result<CoverageMetric, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_projection_coverage(&mut client)
}

pub fn load_ingest_embedding_coverage(
    postgres: &ManagedPostgres,
) -> Result<CoverageMetric, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_embedding_coverage(&mut client)
}

pub(crate) fn load_readiness_metrics(
    client: &mut postgres::Client,
) -> Result<IngestReadinessReport, StorageError> {
    Ok(IngestReadinessReport {
        projection_coverage: load_projection_coverage(client)?,
        embedding_coverage: load_embedding_coverage(client)?,
    })
}

/// Manifest key holding a cached, fully-ready query-readiness report. Its mere PRESENCE means the
/// index was fully query-ready (projection AND embedding coverage complete) at cache time; ingest
/// and embed runs delete it (see `invalidate_query_readiness`), so a present entry is still valid.
pub(crate) const QUERY_READINESS_MANIFEST_KEY: &str = "query_readiness";

/// Load the cached fully-ready query-readiness report, if present and parseable. A returned `Some`
/// means the index was fully query-ready and nothing has ingested/embedded since.
pub fn load_cached_query_readiness(
    postgres: &ManagedPostgres,
) -> Result<Option<IngestReadinessReport>, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let Some(row) = client
        .query_opt(
            "SELECT value::text FROM index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?
    else {
        return Ok(None);
    };
    Ok(serde_json::from_str::<IngestReadinessReport>(&row.get::<_, String>(0)).ok())
}

/// Cache a fully-ready readiness report so subsequent query-readiness checks skip the full-corpus
/// coverage aggregations. Callers MUST only store a report whose projection AND embedding coverage
/// are complete, since the cache fast-path treats presence as "ready for every gate".
pub fn store_query_readiness(
    postgres: &ManagedPostgres,
    report: &IngestReadinessReport,
) -> Result<(), StorageError> {
    let value = serde_json::to_string(report).map_err(StorageError::Json)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    client
        .execute(
            "INSERT INTO index_manifest(key, value, updated_at) \
             VALUES ($1, $2::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE \
             SET value = EXCLUDED.value, \
                 updated_at = EXCLUDED.updated_at;",
            &[&QUERY_READINESS_MANIFEST_KEY, &value],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Drop the cached readiness report so the next query-readiness check recomputes coverage live.
/// Called at the start of ingest and embed runs (which can change coverage).
pub fn invalidate_query_readiness<C: GenericClient>(client: &mut C) -> Result<(), StorageError> {
    client
        .execute(
            "DELETE FROM index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Convenience wrapper over [`invalidate_query_readiness`] for callers that hold a `ManagedPostgres`
/// rather than a client (e.g. the embed-chunks command, which mutates `chunk_embeddings`).
pub fn invalidate_cached_query_readiness(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    invalidate_query_readiness(&mut client)
}

/// Resolve the index's query-readiness report, preferring the manifest cache. On a cache hit the
/// returned `bool` is `true` and no coverage aggregation runs; on a miss the full projection and
/// embedding coverage are computed, and a fully-ready result is cached for next time. All of this
/// happens on ONE connection (a cache hit is a single indexed manifest lookup), so the common hot
/// path costs one round-trip instead of the full-corpus `count(DISTINCT)`/`count(*)` scans.
pub fn load_or_compute_query_readiness(
    postgres: &ManagedPostgres,
) -> Result<(IngestReadinessReport, bool), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    if let Some(row) = client
        .query_opt(
            "SELECT value::text FROM index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?
        && let Ok(cached) = serde_json::from_str::<IngestReadinessReport>(&row.get::<_, String>(0))
    {
        return Ok((cached, true));
    }

    let report = IngestReadinessReport {
        projection_coverage: load_projection_coverage(&mut client)?,
        embedding_coverage: load_embedding_coverage(&mut client)?,
    };
    let fully_ready = coverage_is_complete(&report.projection_coverage)
        && coverage_is_complete(&report.embedding_coverage);
    if fully_ready {
        let value = serde_json::to_string(&report).map_err(StorageError::Json)?;
        client
            .execute(
                "INSERT INTO index_manifest(key, value, updated_at) \
                 VALUES ($1, $2::text::jsonb, now()) \
                 ON CONFLICT (key) DO UPDATE \
                 SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at;",
                &[&QUERY_READINESS_MANIFEST_KEY, &value],
            )
            .map_err(StorageError::PostgresClient)?;
    }
    Ok((report, false))
}

/// A coverage metric is complete when every counted item is covered and at least one exists.
pub(crate) fn coverage_is_complete(metric: &CoverageMetric) -> bool {
    metric.total > 0 && metric.covered == metric.total
}

pub(crate) fn load_projection_coverage<C: GenericClient>(
    client: &mut C,
) -> Result<CoverageMetric, StorageError> {
    let projection = client
        .query_one(
            "SELECT count(DISTINCT d.document_id)::bigint, \
                    count(DISTINCT d.document_id) FILTER (WHERE c.chunk_id IS NOT NULL)::bigint \
             FROM documents d \
             LEFT JOIN chunks c ON c.document_id = d.document_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_documents: i64 = projection.get(0);
    let projected_documents: i64 = projection.get(1);

    Ok(CoverageMetric {
        covered: projected_documents,
        total: total_documents,
        percentage: percentage(projected_documents, total_documents),
    })
}

pub(crate) fn load_embedding_coverage<C: GenericClient>(
    client: &mut C,
) -> Result<CoverageMetric, StorageError> {
    // The non-NULL guards are redundant with SQL equality semantics, but make
    // the freshness requirement explicit in the coverage query.
    let embedding = client
        .query_one(
            "SELECT count(*)::bigint, \
                    count(*) FILTER ( \
                        WHERE c.embedding_fingerprint IS NOT NULL \
                          AND ce.chunk_id IS NOT NULL \
                          AND ce.embedding_fingerprint = c.embedding_fingerprint \
                    )::bigint \
             FROM chunks c \
             LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_chunks: i64 = embedding.get(0);
    let embedded_chunks: i64 = embedding.get(1);

    Ok(CoverageMetric {
        covered: embedded_chunks,
        total: total_chunks,
        percentage: percentage(embedded_chunks, total_chunks),
    })
}
