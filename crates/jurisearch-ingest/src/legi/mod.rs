//! LEGI (statute) ingestion: parse official LEGI XML members into canonical documents,
//! chunks, and graph edges. This module root keeps the shared external imports and the
//! public re-exports; the parser/types/canonical/chunks/links/xml/dates detail lives in
//! submodules that pull the shared scope via `use super::*`.

use std::fmt::Write as _;

use quick_xml::{
    Reader,
    events::{BytesRef, BytesStart, Event},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::archive::ArchiveMember;

mod canonical;
mod chunks;
mod dates;
mod links;
mod parser;
mod types;
mod xml;

use self::chunks::*;
use self::dates::*;
use self::links::*;
use self::parser::*;
use self::xml::*;

pub use canonical::{
    CanonicalChunk, CanonicalDocument, CanonicalGraphEdge, CanonicalValidationError,
    GraphEdgeAttribute, source_payload_hash,
};
pub use parser::{parse_legi_member, parse_legi_xml};
pub use types::{
    LegiParseError, ParsedLegiXml, ParsedSectionTa, ParsedTextStruct, ParsedTextStructLink,
    ParsedTextVersion, SourceProvenance,
};

#[cfg(test)]
mod tests;
