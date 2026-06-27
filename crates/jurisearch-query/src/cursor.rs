//! Opaque search pagination cursors (work/09 P4-4B). Parsing is boundary validation — it runs in the
//! adapter BEFORE a snapshot is opened — but the parsed value crosses into `build_search`
//! ([`ParsedSearchCursor::as_retrieval_cursor`]), so it lives in `jurisearch-query` (the CLI re-exports
//! it). Parameterized on the storage [`GroupBy`] vocabulary, never a CLI enum.

use jurisearch_core::error::ErrorObject;
use jurisearch_storage::retrieval::{GroupBy, RetrievalCursor};

#[derive(Debug, Clone)]
pub enum ParsedSearchCursor {
    Chunk {
        score: String,
        chunk_id: String,
    },
    Document {
        score: String,
        document_id: String,
    },
    /// A multi-corpus fan-out cursor `mc:<group>:<cross_score>:<corpus>:<id>` (work/09 P3C).
    MultiCorpus {
        score: String,
        corpus: String,
        id: String,
    },
}

impl ParsedSearchCursor {
    pub fn as_retrieval_cursor(&self) -> RetrievalCursor<'_> {
        match self {
            Self::Chunk { score, chunk_id } => RetrievalCursor::Chunk { score, chunk_id },
            Self::Document { score, document_id } => {
                RetrievalCursor::Document { score, document_id }
            }
            Self::MultiCorpus { score, corpus, id } => {
                RetrievalCursor::MultiCorpus { score, corpus, id }
            }
        }
    }
}

pub fn validate_cursor_score(score: &str, tail: &str) -> Result<(), ErrorObject> {
    let parsed = score.parse::<f64>().map_err(|_| {
        ErrorObject::bad_input(
            "search --cursor must start with a numeric score followed by ':' and an id",
        )
    })?;
    if !parsed.is_finite() || parsed < 0.0 || tail.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "search --cursor must be a finite non-negative score followed by ':' and an id",
        ));
    }
    Ok(())
}

/// Parse the opaque cursor, tagged by grouping. An `mc:`-prefixed cursor is a multi-corpus fan-out
/// cursor; a `doc:`-prefixed cursor is a document cursor; an unprefixed `<score>:<chunk_id>` is a chunk
/// cursor. A cursor from the other grouping is rejected rather than silently mis-paging.
pub fn parse_search_cursor(
    cursor: &str,
    group_by: GroupBy,
) -> Result<ParsedSearchCursor, ErrorObject> {
    if let Some(rest) = cursor.strip_prefix("mc:") {
        // `mc:<group>:<cross_score>:<corpus>:<id>` — the id may itself contain ':' (e.g. `cass:D1#0`),
        // so split into exactly four fields and keep the remainder as the id.
        let parts: Vec<&str> = rest.splitn(4, ':').collect();
        let [group, score, corpus, id] = parts.as_slice() else {
            return Err(ErrorObject::bad_input(
                "malformed multi-corpus cursor (expected mc:<group>:<score>:<corpus>:<id>)",
            ));
        };
        let cursor_group = match *group {
            "chunk" => GroupBy::Chunk,
            "document" => GroupBy::Document,
            other => {
                return Err(ErrorObject::bad_input(format!(
                    "multi-corpus cursor has an unknown grouping `{other}`"
                )));
            }
        };
        if cursor_group != group_by {
            return Err(ErrorObject::bad_input(format!(
                "this is a `{group}`-grouped multi-corpus cursor; rerun with --group-by {group}"
            )));
        }
        validate_cursor_score(score, id)?;
        if corpus.trim().is_empty() {
            return Err(ErrorObject::bad_input(
                "multi-corpus cursor is missing its corpus",
            ));
        }
        return Ok(ParsedSearchCursor::MultiCorpus {
            score: (*score).to_owned(),
            corpus: (*corpus).to_owned(),
            id: (*id).to_owned(),
        });
    }
    if let Some(rest) = cursor.strip_prefix("doc:") {
        if group_by != GroupBy::Document {
            return Err(ErrorObject::bad_input(
                "this is a document cursor; rerun with --group-by document",
            ));
        }
        let (score, document_id) = rest.split_once(':').ok_or_else(|| {
            ErrorObject::bad_input("malformed document cursor (expected doc:<score>:<document_id>)")
        })?;
        validate_cursor_score(score, document_id)?;
        Ok(ParsedSearchCursor::Document {
            score: score.to_owned(),
            document_id: document_id.to_owned(),
        })
    } else {
        if group_by != GroupBy::Chunk {
            return Err(ErrorObject::bad_input(
                "this is a chunk cursor; rerun with --group-by chunk (the default)",
            ));
        }
        let (score, chunk_id) = cursor.split_once(':').ok_or_else(|| {
            ErrorObject::bad_input(
                "search --cursor must use the cursor value returned by a previous search candidate",
            )
        })?;
        validate_cursor_score(score, chunk_id)?;
        Ok(ParsedSearchCursor::Chunk {
            score: score.to_owned(),
            chunk_id: chunk_id.to_owned(),
        })
    }
}
