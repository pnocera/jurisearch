//! Output serialization/emission: write JSON responses and artifacts to stdout/files
//! and terminate with the correct process exit code on error.
//!
//! This module owns only *emission*; `ErrorObject` construction lives with the error
//! helpers (currently in `main.rs`, moving to `errors.rs` in a later phase).

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use jurisearch_core::error::{ErrorObject, ProcessExit};
use jurisearch_core::session::SessionResponse;

use crate::dependency_unavailable;

/// Pretty-render a JSON value with a single trailing newline. These are the exact bytes
/// written to an artifact file, and they match what [`write_json`] emits to stdout
/// (`to_writer_pretty` + `\n`), so `eval --out FILE` and stdout stay byte-identical.
pub(crate) fn render_artifact(value: &Value) -> Result<String, serde_json::Error> {
    Ok(format!("{}\n", serde_json::to_string_pretty(value)?))
}

/// Print an artifact to stdout, and additionally write it to `out` when given.
pub(crate) fn emit_artifact(response: Value, out: Option<PathBuf>) -> anyhow::Result<()> {
    if let Some(path) = out {
        let rendered = match render_artifact(&response) {
            Ok(rendered) => rendered,
            Err(error) => {
                return emit_error(dependency_unavailable(format!(
                    "failed to serialize artifact: {error}"
                )));
            }
        };
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty())
            && let Err(error) = fs::create_dir_all(parent)
        {
            return emit_error(dependency_unavailable(format!(
                "failed to create artifact directory {}: {error}",
                parent.display()
            )));
        }
        if let Err(error) = fs::write(&path, &rendered) {
            return emit_error(dependency_unavailable(format!(
                "failed to write artifact to {}: {error}",
                path.display()
            )));
        }
    }
    write_json(&response)
}

pub(crate) fn emit_error(error: ErrorObject) -> anyhow::Result<()> {
    let exit: ProcessExit = error.code.into();
    write_json(&json!({ "ok": false, "error": error }))?;
    std::process::exit(exit.code());
}

pub(crate) fn write_json(value: &Value) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer_pretty(&mut handle, value)?;
    handle.write_all(b"\n")?;
    Ok(())
}

pub(crate) fn write_session_response(
    stdout: &mut io::StdoutLock<'_>,
    response: &SessionResponse,
) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *stdout, response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn render_artifact_is_pretty_with_trailing_newline_and_matches_stdout_bytes() {
        let value = json!({
            "command": "eval",
            "metrics": {"recall_at_10": 0.5, "queries": 15},
            "nested": {"a": [1, 2, 3], "b": true}
        });
        let rendered = render_artifact(&value).expect("render artifact");
        // File bytes are pretty JSON + exactly one trailing newline.
        assert_eq!(
            rendered,
            format!("{}\n", serde_json::to_string_pretty(&value).unwrap())
        );
        // stdout (write_json) uses to_writer_pretty + "\n"; assert the file path is byte-identical.
        let mut stdout_bytes = Vec::new();
        serde_json::to_writer_pretty(&mut stdout_bytes, &value).unwrap();
        stdout_bytes.push(b'\n');
        assert_eq!(rendered.as_bytes(), stdout_bytes.as_slice());
    }
}
