use std::{fmt, path::Path, sync::LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

static LEGI_BASELINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Freemium_legi_global_(?<date>\d{8})-(?<time>\d{6})\.tar\.gz$")
        .expect("valid LEGI baseline regex")
});
static LEGI_DELTA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^LEGI_(?<date>\d{8})-(?<time>\d{6})\.tar\.gz$").expect("valid LEGI delta regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveSource {
    Legi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    Baseline,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ArchiveTimestamp(String);

impl ArchiveTimestamp {
    pub fn parse(date: &str, time: &str) -> Result<Self, ArchiveParseError> {
        if date.len() != 8
            || time.len() != 6
            || !date.chars().all(|c| c.is_ascii_digit())
            || !time.chars().all(|c| c.is_ascii_digit())
        {
            return Err(ArchiveParseError::InvalidTimestamp {
                timestamp: format!("{date}-{time}"),
            });
        }
        Ok(Self(format!("{date}{time}")))
    }

    pub fn compact(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ArchiveTimestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = self.compact();
        write!(f, "{}-{}", &value[0..8], &value[8..14])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedArchive {
    pub source: ArchiveSource,
    pub kind: ArchiveKind,
    pub timestamp: ArchiveTimestamp,
    pub file_name: String,
}

impl ParsedArchive {
    pub fn parse_file_name(
        source: ArchiveSource,
        file_name: impl AsRef<str>,
    ) -> Result<Self, ArchiveParseError> {
        let file_name = file_name.as_ref();
        match source {
            ArchiveSource::Legi => parse_legi_file_name(file_name),
        }
    }

    pub fn parse_path(source: ArchiveSource, path: &Path) -> Result<Self, ArchiveParseError> {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ArchiveParseError::MissingFileName)?;
        Self::parse_file_name(source, file_name)
    }
}

fn parse_legi_file_name(file_name: &str) -> Result<ParsedArchive, ArchiveParseError> {
    if let Some(captures) = LEGI_BASELINE_RE.captures(file_name) {
        return Ok(ParsedArchive {
            source: ArchiveSource::Legi,
            kind: ArchiveKind::Baseline,
            timestamp: ArchiveTimestamp::parse(&captures["date"], &captures["time"])?,
            file_name: file_name.to_owned(),
        });
    }
    if let Some(captures) = LEGI_DELTA_RE.captures(file_name) {
        return Ok(ParsedArchive {
            source: ArchiveSource::Legi,
            kind: ArchiveKind::Delta,
            timestamp: ArchiveTimestamp::parse(&captures["date"], &captures["time"])?,
            file_name: file_name.to_owned(),
        });
    }
    Err(ArchiveParseError::Unrecognized {
        file_name: file_name.to_owned(),
        archive_source: ArchiveSource::Legi,
    })
}

#[derive(Debug, Error)]
pub enum ArchiveParseError {
    #[error("archive path has no valid UTF-8 file name")]
    MissingFileName,
    #[error("unrecognized {archive_source:?} archive filename `{file_name}`")]
    Unrecognized {
        file_name: String,
        archive_source: ArchiveSource,
    },
    #[error("invalid archive timestamp `{timestamp}`")]
    InvalidTimestamp { timestamp: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_legi_baseline_name() {
        let parsed = ParsedArchive::parse_file_name(
            ArchiveSource::Legi,
            "Freemium_legi_global_20250713-140000.tar.gz",
        )
        .unwrap();

        assert_eq!(parsed.kind, ArchiveKind::Baseline);
        assert_eq!(parsed.timestamp.compact(), "20250713140000");
    }

    #[test]
    fn parses_legi_delta_name() {
        let parsed =
            ParsedArchive::parse_file_name(ArchiveSource::Legi, "LEGI_20250714-000000.tar.gz")
                .unwrap();

        assert_eq!(parsed.kind, ArchiveKind::Delta);
        assert_eq!(parsed.timestamp.to_string(), "20250714-000000");
    }

    #[test]
    fn rejects_unrecognized_name() {
        let error =
            ParsedArchive::parse_file_name(ArchiveSource::Legi, "Freemium_kali_global.tar.gz")
                .unwrap_err();

        assert!(matches!(error, ArchiveParseError::Unrecognized { .. }));
    }
}
