//! Error types for the deploy layer: strict-parse failures, validation diagnostics, and IO.

use std::fmt;
use std::path::PathBuf;

use thiserror::Error;

/// A single, actionable validation diagnostic. Every diagnostic carries a stable machine `code`,
/// a human `message`, and a concrete `suggestion` for the next operator action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub suggestion: String,
}

impl Diagnostic {
    pub fn new(
        code: &'static str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: suggestion.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "[{}] {} (suggestion: {})",
            self.code, self.message, self.suggestion
        )
    }
}

/// The collected result of validating a parsed [`crate::SiteConfig`]. All diagnostics are reported
/// at once so the operator can fix the whole config in one pass rather than one error per run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationErrors {
    pub diagnostics: Vec<Diagnostic>,
}

impl ValidationErrors {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.diagnostics.is_empty()
    }

    pub fn push(
        &mut self,
        code: &'static str,
        message: impl Into<String>,
        suggestion: impl Into<String>,
    ) {
        self.diagnostics
            .push(Diagnostic::new(code, message, suggestion));
    }

    /// Turn a non-empty collection into an `Err`, or `Ok(())` when clean.
    pub fn into_result(self) -> Result<(), ValidationErrors> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "site config validation failed with {} problem(s):",
            self.diagnostics.len()
        )?;
        for diagnostic in &self.diagnostics {
            writeln!(formatter, "  - {diagnostic}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Top-level error for parse + validate + render flows.
#[derive(Debug, Error)]
pub enum DeployError {
    #[error("failed to read `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse `{path}` as TOML: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error(transparent)]
    Validation(#[from] ValidationErrors),
}
