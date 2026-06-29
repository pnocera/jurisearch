//! Integrity gate for downloaded DILA `.tar.gz` archives.
//!
//! DILA does not publish per-file checksums alongside the OPENDATA archives, so
//! a clean gunzip + tar open (reading every member fully to end) is treated as
//! the integrity proof: a truncated or corrupt download fails to decompress or
//! to read its full member stream and is rejected. A SHA-256 over the on-disk
//! bytes is computed for the accepted file so the cursor can record a stable
//! content identity for audit/debug.

use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

/// Result of a successful integrity check over a downloaded archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrityReport {
    /// On-disk size of the verified file, in bytes.
    pub size_bytes: u64,
    /// `sha256:<hex>` digest of the on-disk bytes.
    pub sha256: String,
    /// Number of tar members successfully read to completion.
    pub members: usize,
}

/// Reasons an archive fails the integrity gate. Any of these quarantines the
/// file; none of them advances the cursor.
#[derive(Debug, Error)]
pub enum IntegrityError {
    /// The file could not be opened.
    #[error("cannot open `{path}`: {source}")]
    Open { path: PathBuf, source: io::Error },

    /// The advertised size from the listing did not match the downloaded size,
    /// indicating an incomplete download.
    #[error("size mismatch for `{path}`: expected {expected} bytes, got {actual}")]
    SizeMismatch {
        path: PathBuf,
        expected: u64,
        actual: u64,
    },

    /// The downloaded file is empty.
    #[error("downloaded archive `{path}` is empty")]
    Empty { path: PathBuf },

    /// The gzip stream is corrupt or the file is not gzip at all.
    #[error("corrupt gzip/tar stream in `{path}`: {message}")]
    Corrupt { path: PathBuf, message: String },
}

/// Verify a downloaded `.tar.gz` is complete and readable.
///
/// `expected_size` is an optional exact byte count (e.g. from a `HEAD`
/// `Content-Length`); when present and not matching the on-disk size the file
/// is rejected as truncated *before* the more expensive decompression pass.
/// Human-readable directory-listing sizes (`42M`, `484K`) are approximate and
/// MUST NOT be passed here — only exact byte counts.
pub fn verify_targz(
    path: &Path,
    expected_size: Option<u64>,
) -> Result<IntegrityReport, IntegrityError> {
    let size_bytes = std::fs::metadata(path)
        .map_err(|source| IntegrityError::Open {
            path: path.to_owned(),
            source,
        })?
        .len();

    if size_bytes == 0 {
        return Err(IntegrityError::Empty {
            path: path.to_owned(),
        });
    }

    if let Some(expected) = expected_size
        && expected != size_bytes
    {
        return Err(IntegrityError::SizeMismatch {
            path: path.to_owned(),
            expected,
            actual: size_bytes,
        });
    }

    let sha256 = sha256_of(path).map_err(|source| IntegrityError::Open {
        path: path.to_owned(),
        source,
    })?;

    let members = read_full_targz(path)?;

    Ok(IntegrityReport {
        size_bytes,
        sha256,
        members,
    })
}

/// Decompress and walk the whole tar, reading every member to its end, then
/// drain the gzip decoder to EOF so the gzip footer (CRC-32 + ISIZE trailer) is
/// validated. A truncated gzip stream, a truncated tar member, or a
/// corrupt/missing gzip trailer surfaces as an `io::Error` here, which we map to
/// [`IntegrityError::Corrupt`].
fn read_full_targz(path: &Path) -> Result<usize, IntegrityError> {
    let file = File::open(path).map_err(|source| IntegrityError::Open {
        path: path.to_owned(),
        source,
    })?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    let mut members = 0usize;
    {
        let entries = archive.entries().map_err(|err| IntegrityError::Corrupt {
            path: path.to_owned(),
            message: err.to_string(),
        })?;

        let mut sink = [0u8; 64 * 1024];
        for entry in entries {
            let mut entry = entry.map_err(|err| IntegrityError::Corrupt {
                path: path.to_owned(),
                message: err.to_string(),
            })?;
            // Drain the member fully; a truncated stream errors mid-read.
            loop {
                let read = entry
                    .read(&mut sink)
                    .map_err(|err| IntegrityError::Corrupt {
                        path: path.to_owned(),
                        message: err.to_string(),
                    })?;
                if read == 0 {
                    break;
                }
            }
            members += 1;
        }
        // `entries` borrows `archive`; drop it here so we can reclaim the reader.
    }

    // The tar entry iterator stops at the end-of-archive zero blocks WITHOUT
    // necessarily reading the underlying gzip stream to EOF, so the gzip footer
    // (the CRC-32 + ISIZE trailer) is never validated by the loop above. An
    // archive with a readable tar prefix but a missing/corrupt gzip trailer
    // (e.g. end-truncation) would otherwise pass. Reclaim the decoder and drain
    // it to EOF: `GzDecoder` validates the trailer only once it is read past the
    // compressed body, so this forces a CRC/length check and surfaces a corrupt
    // or truncated footer as an `io::Error` -> `Corrupt`.
    let mut decoder = archive.into_inner();
    io::copy(&mut decoder, &mut io::sink()).map_err(|err| IntegrityError::Corrupt {
        path: path.to_owned(),
        message: err.to_string(),
    })?;

    Ok(members)
}

fn sha256_of(path: &Path) -> Result<String, io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(7 + digest.len() * 2);
    hex.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}
