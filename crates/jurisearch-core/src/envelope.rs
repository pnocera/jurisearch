//! The framed **site** protocol envelope: a [`SessionRequest`] plus an EXPLICIT protocol version, so
//! the codec ([`jurisearch-transport`]) has something concrete to reject a skewed peer on.
//!
//! The local JSONL surfaces (`session` / `batch` / `serve`) send a **bare** `SessionRequest` and are
//! version-free **by design** — that legacy compatibility is load-bearing (existing agent workflows
//! send `{id, command, args}`), so the version rides only on the *new* site protocol envelope.
//! `SessionRequest`'s `{id, command, args}` shape is unchanged; the envelope wraps it.

use serde::{Deserialize, Serialize};

use crate::session::SessionRequest;

/// The wire protocol version. Serialized transparently (a bare integer), so an envelope is
/// `{"proto": 1, "request": {…}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolVersion(pub u32);

/// The single site protocol version this build speaks. A peer that frames a different version is
/// rejected loudly by the codec (the architecture's "fail on skew, never silently degrade" rule).
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(1);

/// A framed site request: the version-carrying wrapper around a [`SessionRequest`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolEnvelope {
    pub proto: ProtocolVersion,
    pub request: SessionRequest,
}

impl ProtocolEnvelope {
    /// Wrap a request at this build's protocol version.
    pub fn new(request: SessionRequest) -> Self {
        Self {
            proto: PROTOCOL_VERSION,
            request,
        }
    }
}
