use std::{fmt, path::Path, sync::LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Baseline/delta naming is shared across every DILA Freemium dataset:
//   baseline: `Freemium_<src>_global_<YYYYMMDD>-<HHMMSS>.tar.gz` (lowercase source token)
//   delta:    `<SRC>_<YYYYMMDD>-<HHMMSS>.tar.gz`                  (uppercase source token)
// We capture the source token generically and verify it against the requested source, so a
// `Freemium_kali_global_...` parsed as `Cass` is rejected as unrecognized rather than mis-claimed.
static BASELINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Freemium_(?<src>[a-z]+)_global_(?<date>\d{8})-(?<time>\d{6})\.tar\.gz$")
        .expect("valid DILA baseline regex")
});
static DELTA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?<src>[A-Z]+)_(?<date>\d{8})-(?<time>\d{6})\.tar\.gz$")
        .expect("valid DILA delta regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveSource {
    Legi,
    Cass,
    Capp,
    Inca,
    Jade,
}

impl ArchiveSource {
    pub const ALL: [ArchiveSource; 5] = [
        ArchiveSource::Legi,
        ArchiveSource::Cass,
        ArchiveSource::Capp,
        ArchiveSource::Inca,
        ArchiveSource::Jade,
    ];

    /// Lowercase source token as it appears in `Freemium_<src>_global_...` baseline names and the
    /// canonical `source` discriminator.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ArchiveSource::Legi => "legi",
            ArchiveSource::Cass => "cass",
            ArchiveSource::Capp => "capp",
            ArchiveSource::Inca => "inca",
            ArchiveSource::Jade => "jade",
        }
    }

    /// Uppercase source token as it appears in `<SRC>_<ts>.tar.gz` delta names.
    #[must_use]
    pub fn delta_prefix(self) -> &'static str {
        match self {
            ArchiveSource::Legi => "LEGI",
            ArchiveSource::Cass => "CASS",
            ArchiveSource::Capp => "CAPP",
            ArchiveSource::Inca => "INCA",
            ArchiveSource::Jade => "JADE",
        }
    }

    /// Whether this source is a bulk jurisprudence dataset (decision records) rather than statutes.
    #[must_use]
    pub fn is_jurisprudence(self) -> bool {
        !matches!(self, ArchiveSource::Legi)
    }

    /// Parse a lowercase source token (e.g. from a CLI `--source` flag) into a known source.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        ArchiveSource::ALL
            .into_iter()
            .find(|source| source.as_str() == token)
    }
}

impl fmt::Display for ArchiveSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
        if let Some(captures) = BASELINE_RE.captures(file_name)
            && &captures["src"] == source.as_str()
        {
            return Ok(ParsedArchive {
                source,
                kind: ArchiveKind::Baseline,
                timestamp: ArchiveTimestamp::parse(&captures["date"], &captures["time"])?,
                file_name: file_name.to_owned(),
            });
        }
        if let Some(captures) = DELTA_RE.captures(file_name)
            && &captures["src"] == source.delta_prefix()
        {
            return Ok(ParsedArchive {
                source,
                kind: ArchiveKind::Delta,
                timestamp: ArchiveTimestamp::parse(&captures["date"], &captures["time"])?,
                file_name: file_name.to_owned(),
            });
        }
        Err(ArchiveParseError::Unrecognized {
            file_name: file_name.to_owned(),
            archive_source: source,
        })
    }

    pub fn parse_path(source: ArchiveSource, path: &Path) -> Result<Self, ArchiveParseError> {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(ArchiveParseError::MissingFileName)?;
        Self::parse_file_name(source, file_name)
    }
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
    fn parses_jurisprudence_baseline_and_delta_names() {
        for source in [
            ArchiveSource::Cass,
            ArchiveSource::Capp,
            ArchiveSource::Inca,
            ArchiveSource::Jade,
        ] {
            let baseline = ParsedArchive::parse_file_name(
                source,
                format!("Freemium_{}_global_20250713-140000.tar.gz", source.as_str()),
            )
            .unwrap();
            assert_eq!(baseline.kind, ArchiveKind::Baseline);
            assert_eq!(baseline.source, source);
            assert_eq!(baseline.timestamp.compact(), "20250713140000");

            let delta = ParsedArchive::parse_file_name(
                source,
                format!("{}_20250721-212334.tar.gz", source.delta_prefix()),
            )
            .unwrap();
            assert_eq!(delta.kind, ArchiveKind::Delta);
            assert_eq!(delta.source, source);
            assert_eq!(delta.timestamp.to_string(), "20250721-212334");
        }
    }

    #[test]
    fn rejects_cross_source_filename() {
        // A CASS baseline must not parse as a JADE archive (or vice versa).
        let error = ParsedArchive::parse_file_name(
            ArchiveSource::Jade,
            "Freemium_cass_global_20250713-140000.tar.gz",
        )
        .unwrap_err();
        assert!(matches!(error, ArchiveParseError::Unrecognized { .. }));

        let error =
            ParsedArchive::parse_file_name(ArchiveSource::Legi, "CASS_20250721-212334.tar.gz")
                .unwrap_err();
        assert!(matches!(error, ArchiveParseError::Unrecognized { .. }));
    }

    #[test]
    fn rejects_unrecognized_name() {
        let error =
            ParsedArchive::parse_file_name(ArchiveSource::Legi, "Freemium_kali_global.tar.gz")
                .unwrap_err();

        assert!(matches!(error, ArchiveParseError::Unrecognized { .. }));
    }

    #[test]
    fn from_token_roundtrips_all_sources() {
        for source in ArchiveSource::ALL {
            assert_eq!(ArchiveSource::from_token(source.as_str()), Some(source));
        }
        assert_eq!(ArchiveSource::from_token("kali"), None);
    }
}
