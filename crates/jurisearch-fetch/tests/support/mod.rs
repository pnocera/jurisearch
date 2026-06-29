//! Shared test support: in-memory fixture client + `.tar.gz` builders. No network.
#![allow(dead_code)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::GzEncoder};

use jurisearch_fetch::{ArchiveDownloader, ArchiveSource, DirectoryLister, FetchError};

/// A fully in-memory DILA client: it serves canned listing HTML per source and
/// canned (or deliberately corrupt) archive bytes per file name. Never touches
/// the network.
#[derive(Default)]
pub struct FixtureClient {
    listings: HashMap<&'static str, String>,
    archives: HashMap<String, Vec<u8>>,
    /// File names the downloader should fail to even download (transport error).
    transport_failures: HashMap<String, ()>,
}

impl FixtureClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_listing(mut self, source: ArchiveSource, html: impl Into<String>) -> Self {
        self.listings.insert(source.as_str(), html.into());
        self
    }

    pub fn with_archive(mut self, file_name: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.archives.insert(file_name.into(), bytes);
        self
    }

    pub fn with_transport_failure(mut self, file_name: impl Into<String>) -> Self {
        self.transport_failures.insert(file_name.into(), ());
        self
    }
}

impl DirectoryLister for FixtureClient {
    fn fetch_index(&self, source: ArchiveSource) -> Result<String, FetchError> {
        Ok(self
            .listings
            .get(source.as_str())
            .cloned()
            .unwrap_or_default())
    }
}

impl ArchiveDownloader for FixtureClient {
    fn download_to(
        &self,
        source: ArchiveSource,
        file_name: &str,
        dest: &Path,
    ) -> Result<(), FetchError> {
        if self.transport_failures.contains_key(file_name) {
            return Err(FetchError::Download {
                archive_source: source.to_string(),
                file_name: file_name.to_owned(),
                message: "simulated transport failure".to_owned(),
            });
        }
        let bytes = self
            .archives
            .get(file_name)
            .cloned()
            .unwrap_or_else(|| panic!("fixture missing archive bytes for `{file_name}`"));
        std::fs::write(dest, bytes).map_err(|err| FetchError::Io {
            path: dest.to_owned(),
            source: err,
        })
    }
}

/// Build a valid `.tar.gz` containing the given (member-name, bytes) entries.
pub fn make_targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *data)
            .expect("append tar member");
    }
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip")
}

/// A small valid archive with one XML member.
pub fn valid_archive() -> Vec<u8> {
    make_targz(&[("legi/article.xml", b"<ARTICLE>ok</ARTICLE>")])
}

/// A truncated archive: a valid `.tar.gz`'s bytes cut short mid-stream so it
/// cannot be fully decompressed/read.
pub fn truncated_archive() -> Vec<u8> {
    let mut bytes = make_targz(&[(
        "legi/article.xml",
        &vec![b'x'; 200_000], // large enough that truncation lands mid-stream
    )]);
    bytes.truncate(bytes.len() / 2);
    bytes
}

/// Bytes that are not a gzip stream at all (corrupt header).
pub fn corrupt_archive() -> Vec<u8> {
    b"this is definitely not a gzip stream".to_vec()
}

/// An archive whose tar members are all fully readable but whose gzip footer
/// (the trailing 8-byte CRC-32 + ISIZE trailer) is corrupted. The tar entry
/// iterator stops at the end-of-archive marker without reading the trailer, so
/// only a full drain of the gzip decoder to EOF catches this.
pub fn footer_corrupt_archive() -> Vec<u8> {
    let mut bytes = valid_archive();
    let len = bytes.len();
    // Flip the trailing 8 bytes (gzip CRC-32 + ISIZE) so the trailer no longer
    // matches the decompressed payload, without disturbing the deflate body or
    // the tar members it encodes.
    for byte in &mut bytes[len - 8..] {
        *byte ^= 0xff;
    }
    bytes
}

/// Helper to read the file names present in a mirror sub-directory.
pub fn mirror_files(archives_dir: &Path, source: ArchiveSource) -> Vec<String> {
    list_dir(&archives_dir.join(source.as_str()))
}

/// Helper to read the file names present in a quarantine sub-directory.
pub fn quarantine_files(quarantine_dir: &Path, source: ArchiveSource) -> Vec<String> {
    list_dir(&quarantine_dir.join(source.as_str()))
}

fn list_dir(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Ignore any leftover `.part` sidecars in assertions.
            if !name.ends_with(".part") {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

/// Convenience to build a `FetchConfig` against a tempdir layout.
pub struct Layout {
    pub archives_dir: PathBuf,
    pub quarantine_dir: PathBuf,
    pub state_dir: PathBuf,
}

impl Layout {
    pub fn under(root: &Path) -> Self {
        Layout {
            archives_dir: root.join("archives"),
            quarantine_dir: root.join("quarantine"),
            state_dir: root.join("state"),
        }
    }
}
