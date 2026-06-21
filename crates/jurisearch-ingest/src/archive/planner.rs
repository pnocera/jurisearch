use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use walkdir::WalkDir;

use super::parser::{
    ArchiveKind, ArchiveParseError, ArchiveSource, ArchiveTimestamp, ParsedArchive,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedArchive {
    pub source: ArchiveSource,
    pub kind: ArchiveKind,
    pub timestamp: ArchiveTimestamp,
    pub file_name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedArchive {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePlan {
    pub source: ArchiveSource,
    pub baseline: PlannedArchive,
    pub deltas: Vec<PlannedArchive>,
    pub skipped: Vec<SkippedArchive>,
}

#[derive(Debug, Error)]
pub enum ArchivePlanError {
    #[error("failed to walk archive directory `{path}`: {source}")]
    Walk {
        path: PathBuf,
        source: walkdir::Error,
    },
    #[error("no baseline archive found for {archive_source:?}")]
    MissingBaseline { archive_source: ArchiveSource },
}

pub fn plan_from_dir(source: ArchiveSource, dir: &Path) -> Result<ArchivePlan, ArchivePlanError> {
    let mut paths = Vec::new();
    for entry in WalkDir::new(dir).max_depth(1).into_iter() {
        let entry = entry.map_err(|err| ArchivePlanError::Walk {
            path: dir.to_owned(),
            source: err,
        })?;
        if entry.file_type().is_file() {
            paths.push(entry.into_path());
        }
    }
    plan_from_paths(source, paths)
}

pub fn plan_from_paths<I, P>(
    source: ArchiveSource,
    paths: I,
) -> Result<ArchivePlan, ArchivePlanError>
where
    I: IntoIterator<Item = P>,
    P: Into<PathBuf>,
{
    let mut recognized = Vec::<PlannedArchive>::new();
    let mut skipped = Vec::<SkippedArchive>::new();

    for path in paths.into_iter().map(Into::into) {
        match ParsedArchive::parse_path(source, &path) {
            Ok(parsed) => recognized.push(PlannedArchive {
                source: parsed.source,
                kind: parsed.kind,
                timestamp: parsed.timestamp,
                file_name: parsed.file_name,
                path,
            }),
            Err(ArchiveParseError::Unrecognized { .. }) => skipped.push(SkippedArchive {
                path,
                reason: "unrecognized_filename".into(),
            }),
            Err(error) => skipped.push(SkippedArchive {
                path,
                reason: error.to_string(),
            }),
        }
    }

    recognized.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.file_name.cmp(&right.file_name))
    });

    let baseline = recognized
        .iter()
        .filter(|archive| archive.kind == ArchiveKind::Baseline)
        .max_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.file_name.cmp(&right.file_name))
        })
        .cloned()
        .ok_or(ArchivePlanError::MissingBaseline {
            archive_source: source,
        })?;

    let mut deltas = Vec::new();
    for archive in recognized {
        match archive.kind {
            ArchiveKind::Baseline if archive.file_name != baseline.file_name => {
                skipped.push(SkippedArchive {
                    path: archive.path,
                    reason: "older_baseline".into(),
                });
            }
            ArchiveKind::Baseline => {}
            ArchiveKind::Delta if archive.timestamp > baseline.timestamp => deltas.push(archive),
            ArchiveKind::Delta => skipped.push(SkippedArchive {
                path: archive.path,
                reason: "delta_not_after_selected_baseline".into(),
            }),
        }
    }

    deltas.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.file_name.cmp(&right.file_name))
    });
    skipped.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(ArchivePlan {
        source,
        baseline,
        deltas,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn selects_latest_baseline_and_chronological_deltas_after_it() {
        let plan = plan_from_paths(
            ArchiveSource::Legi,
            [
                p("LEGI_20250712-000000.tar.gz"),
                p("Freemium_legi_global_20250713-140000.tar.gz"),
                p("LEGI_20250714-000000.tar.gz"),
                p("Freemium_legi_global_20250710-140000.tar.gz"),
                p("LEGI_20250715-060000.tar.gz"),
                p("notes.txt"),
            ],
        )
        .unwrap();

        assert_eq!(
            plan.baseline.file_name,
            "Freemium_legi_global_20250713-140000.tar.gz"
        );
        assert_eq!(
            plan.deltas
                .iter()
                .map(|archive| archive.file_name.as_str())
                .collect::<Vec<_>>(),
            vec!["LEGI_20250714-000000.tar.gz", "LEGI_20250715-060000.tar.gz"]
        );
        let mut reasons = plan
            .skipped
            .iter()
            .map(|skipped| skipped.reason.as_str())
            .collect::<Vec<_>>();
        reasons.sort();
        assert_eq!(
            reasons,
            vec![
                "delta_not_after_selected_baseline",
                "older_baseline",
                "unrecognized_filename"
            ]
        );
    }

    #[test]
    fn errors_without_baseline() {
        let error =
            plan_from_paths(ArchiveSource::Legi, [p("LEGI_20250714-000000.tar.gz")]).unwrap_err();

        assert!(matches!(error, ArchivePlanError::MissingBaseline { .. }));
    }
}
