//! Index lifecycle + query-readiness gating: resolve/validate the index dir, open the
//! managed Postgres (query vs bulk-ingest), and enforce the projection/embedding coverage
//! gate before retrieval. Error construction lives in `crate::errors`.

use crate::*;

pub(crate) fn require_existing_index_dir(index_dir: Option<&Path>) -> Result<PathBuf, ErrorObject> {
    let configured = configured_index_dir(index_dir);
    let Some(index_dir) = configured else {
        return Err(index_unavailable(
            "index directory is required; pass `--index-dir` or set JURISEARCH_INDEX_DIR",
        ));
    };
    if !index_dir.join("pg/data/PG_VERSION").is_file() {
        return Err(index_unavailable(format!(
            "`{}` is not an initialized jurisearch index",
            index_dir.display()
        )));
    }
    Ok(index_dir)
}

pub(crate) fn require_configured_index_dir(index_dir: Option<&Path>) -> Result<PathBuf, ErrorObject> {
    configured_index_dir(index_dir).ok_or_else(|| {
        index_unavailable(
            "index directory is required; pass `--index-dir` or set JURISEARCH_INDEX_DIR",
        )
    })
}

pub(crate) fn configured_index_dir(index_dir: Option<&Path>) -> Option<PathBuf> {
    index_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("JURISEARCH_INDEX_DIR").map(PathBuf::from))
}

pub(crate) fn open_index(index_dir: &Path) -> Result<ManagedPostgres, ErrorObject> {
    let pg_config = PgConfig::discover().map_err(storage_error_object)?;
    ManagedPostgres::start_durable(pg_config, index_dir).map_err(storage_error_object)
}

pub(crate) fn open_index_for_bulk_ingest(index_dir: &Path) -> Result<ManagedPostgres, ErrorObject> {
    let pg_config = PgConfig::discover().map_err(storage_error_object)?;
    ManagedPostgres::start_durable_with_profile(
        pg_config,
        index_dir,
        PostgresRuntimeProfile::BulkIngest,
    )
    .map_err(storage_error_object)
}

pub(crate) fn coverage_complete(covered: i64, total: i64) -> bool {
    total > 0 && covered == total
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum QueryReadinessGate {
    Fetch,
    SearchLexical,
    Search,
}

impl QueryReadinessGate {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::SearchLexical => "search --mode bm25",
            Self::Search => "search",
        }
    }
}

pub(crate) fn ensure_query_readiness(
    postgres: &ManagedPostgres,
    gate: QueryReadinessGate,
) -> Result<(), ErrorObject> {
    // One round-trip on the hot path: a manifest cache hit skips the full-corpus coverage
    // aggregations (a count(DISTINCT) over ~1.74M documents plus a count over ~1.85M chunks). The
    // cache is only populated when the index is fully ready and is invalidated by ingest/embed runs.
    let (readiness, _from_cache) =
        load_or_compute_query_readiness(postgres).map_err(storage_error_object)?;
    let projection_coverage = readiness.projection_coverage;
    let embedding_coverage = readiness.embedding_coverage;

    if !coverage_complete(projection_coverage.covered, projection_coverage.total) {
        return Err(index_not_query_ready(
            gate,
            "projection coverage gate is incomplete",
            &projection_coverage,
            None,
        ));
    }

    if matches!(
        gate,
        QueryReadinessGate::Fetch | QueryReadinessGate::SearchLexical
    ) {
        return Ok(());
    }

    if matches!(gate, QueryReadinessGate::Search)
        && !coverage_complete(embedding_coverage.covered, embedding_coverage.total)
    {
        return Err(index_not_query_ready(
            gate,
            "embedding coverage gate is incomplete",
            &projection_coverage,
            Some(&embedding_coverage),
        ));
    }

    Ok(())
}
