use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use tar::Archive;
use thiserror::Error;

pub const DEFAULT_MEMBER_BYTE_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveMember {
    pub archive_path: PathBuf,
    pub member_path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveVisit {
    Continue,
    Stop,
}

#[derive(Debug, Error)]
pub enum ArchiveReadError {
    #[error("failed to open archive `{path}`: {source}")]
    Open { path: PathBuf, source: io::Error },
    #[error("failed to read archive `{path}`: {source}")]
    Archive { path: PathBuf, source: io::Error },
    #[error("failed to read member `{member}` from `{archive}`: {source}")]
    Member {
        archive: PathBuf,
        member: String,
        source: io::Error,
    },
    #[error("member `{member}` from `{archive}` exceeds byte limit {limit}")]
    MemberTooLarge {
        archive: PathBuf,
        member: String,
        limit: u64,
    },
}

pub fn for_each_xml_member<F>(
    archive_path: &Path,
    max_member_bytes: u64,
    mut visit: F,
) -> Result<usize, ArchiveReadError>
where
    F: FnMut(ArchiveMember) -> Result<(), ArchiveReadError>,
{
    for_each_xml_member_until(archive_path, max_member_bytes, |member| {
        visit(member)?;
        Ok(ArchiveVisit::Continue)
    })
}

pub fn for_each_xml_member_until<F>(
    archive_path: &Path,
    max_member_bytes: u64,
    mut visit: F,
) -> Result<usize, ArchiveReadError>
where
    F: FnMut(ArchiveMember) -> Result<ArchiveVisit, ArchiveReadError>,
{
    let file = File::open(archive_path).map_err(|source| ArchiveReadError::Open {
        path: archive_path.to_owned(),
        source,
    })?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);
    let mut visited = 0;

    let entries = archive
        .entries()
        .map_err(|source| ArchiveReadError::Archive {
            path: archive_path.to_owned(),
            source,
        })?;
    for entry in entries {
        let mut entry = entry.map_err(|source| ArchiveReadError::Archive {
            path: archive_path.to_owned(),
            source,
        })?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let member_path = entry
            .path()
            .map_err(|source| ArchiveReadError::Archive {
                path: archive_path.to_owned(),
                source,
            })?
            .to_string_lossy()
            .into_owned();
        if !member_path.ends_with(".xml") {
            continue;
        }
        let bytes = read_bounded(
            &mut entry,
            max_member_bytes,
            archive_path,
            member_path.as_str(),
        )?;
        let action = visit(ArchiveMember {
            archive_path: archive_path.to_owned(),
            member_path,
            bytes,
        })?;
        visited += 1;
        if action == ArchiveVisit::Stop {
            break;
        }
    }

    Ok(visited)
}

fn read_bounded<R: Read>(
    reader: &mut R,
    max_member_bytes: u64,
    archive_path: &Path,
    member_path: &str,
) -> Result<Vec<u8>, ArchiveReadError> {
    let mut limited = reader.take(max_member_bytes.saturating_add(1));
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|source| ArchiveReadError::Member {
            archive: archive_path.to_owned(),
            member: member_path.to_owned(),
            source,
        })?;
    if bytes.len() as u64 > max_member_bytes {
        return Err(ArchiveReadError::MemberTooLarge {
            archive: archive_path.to_owned(),
            member: member_path.to_owned(),
            limit: max_member_bytes,
        });
    }
    Ok(bytes)
}
