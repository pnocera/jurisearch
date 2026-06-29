//! Network abstraction for DILA listing + download.
//!
//! The fetch engine speaks to two small traits — [`DirectoryLister`] and
//! [`ArchiveDownloader`] — so that tests inject fixture listings and fixture
//! archive bytes with NO network. The real implementation, [`UreqDilaClient`],
//! talks HTTPS to `echanges.dila.gouv.fr` via `ureq`; it is never used by the
//! test suite.

use std::{io, path::Path, time::Duration};

use url::Url;

use jurisearch_ingest::archive::ArchiveSource;

use crate::error::FetchError;

/// Retrieves the raw HTML of a DILA dataset directory listing.
pub trait DirectoryLister {
    /// Fetch the Apache directory-listing HTML for `source`'s dataset dir.
    fn fetch_index(&self, source: ArchiveSource) -> Result<String, FetchError>;
}

/// Downloads a single archive's bytes to a destination path.
pub trait ArchiveDownloader {
    /// Download the archive `file_name` belonging to `source`, writing the raw
    /// bytes to `dest`. Implementations should write completely or fail; partial
    /// writes left at `dest` are caught by the engine's integrity gate.
    fn download_to(
        &self,
        source: ArchiveSource,
        file_name: &str,
        dest: &Path,
    ) -> Result<(), FetchError>;
}

/// The DILA dataset sub-directory name for a source, as it appears under
/// `/OPENDATA/` (e.g. `LEGI`, `CASS`). This matches the uppercase delta prefix.
#[must_use]
pub fn dataset_dir(source: ArchiveSource) -> &'static str {
    source.delta_prefix()
}

/// Real HTTPS client for the DILA "serveur d'échanges".
///
/// Construct with [`UreqDilaClient::new`]. Tests do not use this type; the live
/// DILA leg is deferred to an authorized run.
#[derive(Debug, Clone)]
pub struct UreqDilaClient {
    base_url: String,
    user_agent: String,
    timeout: Duration,
}

impl UreqDilaClient {
    /// Build a client.
    ///
    /// * `base_url` — e.g. `https://echanges.dila.gouv.fr/OPENDATA` (no trailing
    ///   slash required).
    /// * `user_agent` — a polite identifying UA, e.g.
    ///   `jurisearch-producer/<version> (+contact)`.
    /// * `timeout_secs` — per read/connect timeout.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        user_agent: impl Into<String>,
        timeout_secs: u64,
    ) -> Self {
        UreqDilaClient {
            base_url: base_url.into(),
            user_agent: user_agent.into(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(self.timeout)
            .timeout_read(self.timeout)
            .user_agent(&self.user_agent)
            .build()
    }

    fn dir_url(&self, source: ArchiveSource) -> Result<Url, FetchError> {
        // Ensure a single trailing slash so `Url::join` resolves file names
        // relative to the directory, not its parent.
        let base = self.base_url.trim_end_matches('/');
        let raw = format!("{base}/{}/", dataset_dir(source));
        Url::parse(&raw).map_err(|err| FetchError::Listing {
            archive_source: source.to_string(),
            message: format!("invalid base url `{raw}`: {err}"),
        })
    }
}

impl DirectoryLister for UreqDilaClient {
    fn fetch_index(&self, source: ArchiveSource) -> Result<String, FetchError> {
        let url = self.dir_url(source)?;
        let response =
            self.agent()
                .get(url.as_str())
                .call()
                .map_err(|err| FetchError::Listing {
                    archive_source: source.to_string(),
                    message: err.to_string(),
                })?;
        response.into_string().map_err(|err| FetchError::Listing {
            archive_source: source.to_string(),
            message: format!("failed to read listing body: {err}"),
        })
    }
}

impl ArchiveDownloader for UreqDilaClient {
    fn download_to(
        &self,
        source: ArchiveSource,
        file_name: &str,
        dest: &Path,
    ) -> Result<(), FetchError> {
        let url = self
            .dir_url(source)?
            .join(file_name)
            .map_err(|err| FetchError::Download {
                archive_source: source.to_string(),
                file_name: file_name.to_owned(),
                message: format!("invalid archive url: {err}"),
            })?;

        let response =
            self.agent()
                .get(url.as_str())
                .call()
                .map_err(|err| FetchError::Download {
                    archive_source: source.to_string(),
                    file_name: file_name.to_owned(),
                    message: err.to_string(),
                })?;

        let mut reader = response.into_reader();
        let mut file = std::fs::File::create(dest).map_err(|err| FetchError::io(dest, err))?;
        io::copy(&mut reader, &mut file).map_err(|err| FetchError::Download {
            archive_source: source.to_string(),
            file_name: file_name.to_owned(),
            message: format!("download stream error: {err}"),
        })?;
        Ok(())
    }
}
