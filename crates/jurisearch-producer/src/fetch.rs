//! Drive `jurisearch-fetch` from the producer config: list a DILA source dir, download new archives
//! into the Storebox mirror (integrity-gated, cursor-advanced), or report a dry-run plan.

use jurisearch_fetch::{
    ArchiveDownloader, ArchiveSource, DirectoryLister, FetchConfig, FetchCursor, Fetcher,
    UreqDilaClient,
};

use crate::config::ProducerConfig;
use crate::cursors::FetchCursorCoordinate;
use crate::error::ProducerError;

/// What one source fetch produced (real or dry-run), plus the resulting fetch-cursor coordinate.
#[derive(Debug, Clone)]
pub struct FetchStepReport {
    pub source: ArchiveSource,
    pub dry_run: bool,
    /// File names that WOULD be (dry-run) or WERE downloaded this run.
    pub planned_or_downloaded: Vec<String>,
    /// File names quarantined this run (integrity failed; cursor not advanced). Empty on dry-run.
    pub quarantined: Vec<String>,
    /// File names already present per the cursor (skipped — the no-op-on-rerun proof).
    pub already_present: Vec<String>,
    /// Total archives in the listing that parsed for this source.
    pub listing_total: usize,
    /// The fetch cursor coordinate AFTER this step (archive-timestamp space).
    pub cursor: FetchCursorCoordinate,
}

/// Read a source's persisted fetch cursor into the typed coordinate (no network).
pub fn read_fetch_cursor(
    config: &ProducerConfig,
    source: ArchiveSource,
) -> Result<FetchCursorCoordinate, ProducerError> {
    let cursor = FetchCursor::load(&config.producer.state_dir, source)?;
    Ok(cursor_coordinate(source, &cursor))
}

/// Compute the typed fetch coordinate from a `jurisearch-fetch` cursor.
fn cursor_coordinate(source: ArchiveSource, cursor: &FetchCursor) -> FetchCursorCoordinate {
    // The "latest" entry is the one with the highest archive timestamp.
    let latest = cursor
        .fetched
        .iter()
        .max_by(|a, b| a.1.timestamp.cmp(&b.1.timestamp));
    FetchCursorCoordinate {
        source: source.as_str().to_owned(),
        latest_file_name: latest.map(|(name, _)| name.clone()),
        latest_compact_timestamp: latest.map(|(_, entry)| entry.timestamp.compact().to_owned()),
    }
}

/// Build the `jurisearch-fetch` engine config from the producer config. Quarantine lives under the
/// state dir so the served mirror only ever contains accepted archives.
fn fetch_config<'a>(
    config: &'a ProducerConfig,
    source: ArchiveSource,
    quarantine_dir: &'a std::path::Path,
) -> FetchConfig<'a> {
    FetchConfig {
        source,
        archives_dir: &config.producer.archives_dir,
        quarantine_dir,
        state_dir: &config.producer.state_dir,
    }
}

/// Fetch one DILA source. When `dry_run` is set, lists what WOULD be downloaded without touching the
/// network beyond the directory listing; otherwise downloads + integrity-checks each new archive.
pub fn fetch_source(
    config: &ProducerConfig,
    source: ArchiveSource,
    dry_run: bool,
) -> Result<FetchStepReport, ProducerError> {
    let client = UreqDilaClient::new(
        config.fetch.base_url.clone(),
        config.fetch.user_agent.clone(),
        config.fetch.timeout_secs,
    );
    fetch_source_with(config, source, dry_run, &client, &client)
}

/// The fetch core, generic over the listing/download traits so tests inject fixture listings + archive
/// bytes with NO network. [`fetch_source`] is the production caller (the real DILA HTTPS client).
pub fn fetch_source_with<L: DirectoryLister, D: ArchiveDownloader>(
    config: &ProducerConfig,
    source: ArchiveSource,
    dry_run: bool,
    lister: &L,
    downloader: &D,
) -> Result<FetchStepReport, ProducerError> {
    let quarantine_dir = config.producer.state_dir.join("quarantine");
    let cfg = fetch_config(config, source, &quarantine_dir);
    let fetcher = Fetcher::new(lister, downloader);

    if dry_run {
        let plan = fetcher.plan(&cfg)?;
        let cursor = FetchCursor::load(&config.producer.state_dir, source)?;
        return Ok(FetchStepReport {
            source,
            dry_run: true,
            planned_or_downloaded: plan
                .to_fetch
                .iter()
                .map(|remote| remote.parsed.file_name.clone())
                .collect(),
            quarantined: Vec::new(),
            already_present: plan.already_fetched,
            listing_total: plan.listing_total,
            cursor: cursor_coordinate(source, &cursor),
        });
    }

    let outcome = fetcher.run(&cfg)?;
    let listing_total = outcome.downloaded.len() + outcome.already_present.len();
    Ok(FetchStepReport {
        source,
        dry_run: false,
        planned_or_downloaded: outcome
            .downloaded
            .iter()
            .map(|item| item.file_name.clone())
            .collect(),
        quarantined: outcome
            .quarantined
            .iter()
            .map(|item| item.file_name.clone())
            .collect(),
        already_present: outcome.already_present,
        listing_total,
        cursor: cursor_coordinate(source, &outcome.cursor),
    })
}
