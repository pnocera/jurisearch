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

pub(crate) fn require_configured_index_dir(
    index_dir: Option<&Path>,
) -> Result<PathBuf, ErrorObject> {
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

/// The shared query-command preamble: resolve+validate the index dir, start the managed Postgres,
/// and enforce the query-readiness `gate` — for the read payloads (fetch/cite/context/related/
/// compare/inspect/versions/diff). Command-specific argument validation and no-results handling
/// stay in each payload (this only owns the open+readiness sequence, not the command's semantics).
pub(crate) fn open_query_index(
    index_dir: Option<&Path>,
    gate: QueryReadinessGate,
) -> Result<ManagedPostgres, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, gate)?;
    Ok(postgres)
}

/// Parse a storage-layer JSON string into a `serde_json::Value`, mapping a malformed payload to the
/// standard `dependency_unavailable` error. The storage helpers return serialized JSON strings; this
/// is the shared bridge into the CLI's `Value` world (replaces the repeated
/// `serde_json::from_str(...).map_err(|e| dependency_unavailable(e.to_string()))`).
pub(crate) use jurisearch_query::parse_storage_json;

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
    // work/09 P3A: an installed (client) topology is a read-only LOOKUP of the writer-owned readiness
    // stamp (a missing/stale stamp errors — the writer must have stamped at apply time); the `public`
    // producer/local working set keeps the legacy compute-on-read cache. Either way no write on the
    // read path for an installed topology.
    let readiness = resolve_query_readiness(postgres).map_err(storage_error_object)?;
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
