//! `jurisearch-fetch` — DILA source fetching and integrity gating.
//!
//! This crate owns the *remote* side of the DILA ingest pipeline: turning a
//! `https://echanges.dila.gouv.fr/OPENDATA/<SRC>/` Apache directory listing into
//! a verified, incremental download into a local mirror, without ingesting
//! anything. It is the producer-side counterpart to the on-disk archive planner
//! in [`jurisearch_ingest::archive`].
//!
//! The crate is built around four pieces:
//!
//! * [`listing`] — a robust parser for Apache `mod_autoindex` output that
//!   extracts archive entries and filters them to a requested [`ArchiveSource`]
//!   using the *same* name parser the ingest planner uses
//!   ([`jurisearch_ingest::archive::ParsedArchive::parse_file_name`]). A name
//!   that does not parse for the requested source is ignored, never downloaded
//!   (cross-source safety).
//! * [`cursor`] — a persistent, per-source [`cursor::FetchCursor`] that records
//!   which archives have already been fully downloaded *and* passed integrity.
//!   Selection/ordering keys on the DILA [`ArchiveTimestamp`]/archive name, NOT
//!   on a package `change_seq`. The cursor advances ONLY after an archive is
//!   fully downloaded and passes integrity.
//! * [`integrity`] — the gate for downloaded `.tar.gz` files: a complete-download
//!   + gzip/tar validity check, with a SHA-256 over the accepted bytes.
//! * [`engine`] — ties the above together behind the [`remote::DirectoryLister`]
//!   / [`remote::ArchiveDownloader`] traits so tests inject fixture listings and
//!   fixture archives with no network. Corrupt/truncated downloads are moved to
//!   quarantine and DO NOT advance the cursor.
//!
//! # Cursor invariant (archive cursor != package cursor)
//!
//! The fetch cursor lives in *DILA archive-timestamp* space, keyed per archive
//! file name. It has nothing to do with the package builder's `change_seq`
//! high-water mark. Archive selection is by [`ArchiveTimestamp`]/name plus
//! per-archive journal state, never by package sequence.
//!
//! The [`jurisearch_ingest::archive`] module is consumed strictly read-only:
//! this crate reuses [`ArchiveSource`], [`ArchiveKind`], [`ArchiveTimestamp`],
//! and [`jurisearch_ingest::archive::ParsedArchive`] and does not modify it.

pub mod cursor;
pub mod engine;
pub mod error;
pub mod integrity;
pub mod listing;
pub mod remote;

pub use cursor::{CursorEntry, FetchCursor};
pub use engine::{
    DownloadedArchive, FetchConfig, FetchOutcome, FetchPlan, Fetcher, PlannedDownload,
    QuarantinedArchive,
};
pub use error::FetchError;
pub use integrity::{IntegrityError, IntegrityReport, verify_targz};
pub use listing::{ListedEntry, RemoteArchive, parse_apache_index, parse_source_listing};
pub use remote::{ArchiveDownloader, DirectoryLister, UreqDilaClient};

// Re-export the read-only archive types this crate keys on, so downstream
// callers (the producer) get a single import surface.
pub use jurisearch_ingest::archive::{ArchiveKind, ArchiveSource, ArchiveTimestamp, ParsedArchive};
