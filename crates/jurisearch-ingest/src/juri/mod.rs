//! DILA bulk jurisprudence ingestion: `TEXTE_JURI_JUDI` (judicial: CASS/CAPP/INCA) and
//! `TEXTE_JURI_ADMIN` (administrative: JADE) official XML → canonical decision records.
//!
//! Per the Phase 2 scope decision (`work/03-implementation/02-evidence/
//! 2026-06-23-phase2-jurisprudence-ingestion-scope-decision.md`), DILA bulk is the primary
//! offline full-corpus jurisprudence path. Bulk XML carries no official Judilibre zone offsets, so
//! chunking is honestly flagged `heuristic` and never satisfies the official-zone gate by assertion.
//! Decisions are *dated, not versioned*: `decision_date` is canonical and `valid_to` is always null.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use quick_xml::{
    Reader,
    events::{BytesRef, BytesStart, Event},
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::archive::{ArchiveMember, ArchiveSource};
use crate::legi::{
    CanonicalChunk, CanonicalGraphEdge, GraphEdgeAttribute, SourceProvenance, source_payload_hash,
};

mod chunks;
mod dates;
mod inferred_citations;
mod parser;
mod publisher_links;
mod types;
mod xml;

use self::chunks::*;
use self::dates::*;
use self::inferred_citations::*;
use self::parser::*;
use self::publisher_links::*;
use self::types::*;
use self::xml::*;

pub use parser::{parse_juri_member, parse_juri_xml};
pub use types::{
    CanonicalDecision, DecisionSummary, DecisionValidationError, JuriFamily, JuriParseError,
    ParsedJuriXml,
};

#[cfg(test)]
mod tests;
