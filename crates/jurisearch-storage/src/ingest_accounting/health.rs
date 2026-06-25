//! Aggregate ingest health report (+ coverage metric, percentage).

use super::*;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IngestHealthReport {
    pub latest_run_id: Option<String>,
    pub latest_run_status: Option<String>,
    pub latest_completed_run_id: Option<String>,
    pub latest_manifest: serde_json::Value,
    pub embedding_manifest: serde_json::Value,
    pub total_members: i64,
    pub inserted_members: i64,
    pub skipped_members: i64,
    pub failed_members: i64,
    pub failed_member_percentage: Option<f64>,
    pub error_classes: Vec<IngestErrorClassCount>,
    pub projection_coverage: CoverageMetric,
    pub embedding_coverage: CoverageMetric,
    pub replay_snapshot_status: String,
    pub replay_snapshot_source: String,
    pub replay_snapshot: ReplaySnapshotReport,
    pub recovery_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageMetric {
    pub covered: i64,
    pub total: i64,
    pub percentage: Option<f64>,
}

pub fn load_ingest_health(postgres: &ManagedPostgres) -> Result<IngestHealthReport, StorageError> {
    load_ingest_health_with_replay_snapshot_mode(postgres, ReplaySnapshotMode::Cached)
}

pub fn load_ingest_health_with_replay_snapshot_mode(
    postgres: &ManagedPostgres,
    replay_snapshot_mode: ReplaySnapshotMode,
) -> Result<IngestHealthReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let latest = client
        .query_opt(
            "SELECT run_id, status, manifest::text \
             FROM ingest_run \
             ORDER BY started_at DESC, run_id DESC \
             LIMIT 1;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let latest_run_id = latest.as_ref().map(|row| row.get::<_, String>(0));
    let latest_run_status = latest.as_ref().map(|row| row.get::<_, String>(1));
    let latest_manifest = latest
        .as_ref()
        .map(|row| row.get::<_, String>(2))
        .map(|manifest| serde_json::from_str(&manifest))
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));
    let latest_completed_run_id = client
        .query_opt(
            "SELECT run_id \
             FROM ingest_run \
             WHERE status = 'completed' \
             ORDER BY completed_at DESC NULLS LAST, started_at DESC, run_id DESC \
             LIMIT 1;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .map(|row| row.get::<_, String>(0));
    let embedding_manifest = client
        .query_opt(
            "SELECT value::text \
             FROM index_manifest \
             WHERE key = 'embedding';",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .map(|row| row.get::<_, String>(0))
        .map(|manifest| serde_json::from_str(&manifest))
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({}));

    let counts = client
        .query_one(
            "SELECT count(*)::bigint, \
                    count(*) FILTER (WHERE status = 'inserted')::bigint, \
                    count(*) FILTER (WHERE status = 'skipped')::bigint, \
                    count(*) FILTER (WHERE status = 'failed')::bigint \
             FROM ingest_member \
             WHERE ($1::text IS NULL OR run_id = $1);",
            &[&latest_run_id],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_members: i64 = counts.get(0);
    let inserted_members: i64 = counts.get(1);
    let skipped_members: i64 = counts.get(2);
    let failed_members: i64 = counts.get(3);

    let error_classes = client
        .query(
            "SELECT error_class, error_code, count(*)::bigint \
             FROM ingest_error \
             WHERE ($1::text IS NULL OR run_id = $1) \
             GROUP BY error_class, error_code \
             ORDER BY count(*) DESC, error_class, error_code;",
            &[&latest_run_id],
        )
        .map_err(StorageError::PostgresClient)?
        .into_iter()
        .map(|row| IngestErrorClassCount {
            error_class: row.get(0),
            error_code: row.get(1),
            count: row.get(2),
        })
        .collect();

    let readiness = load_readiness_metrics(&mut client)?;
    let (replay_snapshot, replay_snapshot_source) = match replay_snapshot_mode {
        ReplaySnapshotMode::Cached => load_cached_replay_snapshot(&mut client)?
            .map(|snapshot| (snapshot, "cached".to_owned()))
            .unwrap_or_else(|| (ReplaySnapshotReport::empty(), "missing".to_owned())),
        ReplaySnapshotMode::Refresh => {
            let snapshot = refresh_replay_snapshot_with_client(&mut client)?;
            (snapshot, "refreshed".to_owned())
        }
    };
    let replay_snapshot_status = if replay_snapshot_source == "missing" {
        "missing".to_owned()
    } else {
        replay_snapshot.status().to_owned()
    };

    let mut recovery_warnings = Vec::new();
    if let Some(status) = &latest_run_status
        && status != IngestRunStatus::Completed.as_str()
    {
        recovery_warnings.push(format!("latest ingest run is `{status}`"));
    }
    if failed_members > 0 {
        recovery_warnings.push(format!(
            "{failed_members} member(s) failed in latest ingest run"
        ));
    }

    Ok(IngestHealthReport {
        latest_run_id,
        latest_run_status,
        latest_completed_run_id,
        latest_manifest,
        embedding_manifest,
        total_members,
        inserted_members,
        skipped_members,
        failed_members,
        failed_member_percentage: percentage(failed_members, total_members),
        error_classes,
        projection_coverage: readiness.projection_coverage,
        embedding_coverage: readiness.embedding_coverage,
        replay_snapshot_status: replay_snapshot_status.to_owned(),
        replay_snapshot_source,
        replay_snapshot,
        recovery_warnings,
    })
}

pub(crate) fn percentage(covered: i64, total: i64) -> Option<f64> {
    if total == 0 {
        None
    } else {
        Some((covered as f64 / total as f64) * 100.0)
    }
}
