//! The framed **site** protocol envelope: a [`SessionRequest`] plus an EXPLICIT protocol version, so
//! the codec ([`jurisearch-transport`]) has something concrete to reject a skewed peer on.
//!
//! The local JSONL surfaces (`session` / `batch` / `serve`) send a **bare** `SessionRequest` and are
//! version-free **by design** — that legacy compatibility is load-bearing (existing agent workflows
//! send `{id, command, args}`), so the version rides only on the *new* site protocol envelope.
//! `SessionRequest`'s `{id, command, args}` shape is unchanged; the envelope wraps it.

use serde::{Deserialize, Serialize};

use crate::session::{SessionRequest, SessionResponse};

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

/// A framed site RESPONSE: the version-carrying wrapper around a [`SessionResponse`] (work/09 P6). The
/// site service replies with this (NOT a bare response), so the thin client validates the SERVER's
/// protocol version on every reply and fails loudly on skew — symmetric with the request envelope. A
/// bare/unversioned reply is rejected as an old/incompatible server. The local `session`/`batch`/`serve`
/// surfaces still reply BARE (version-free by design); this envelope rides only on the site path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolResponseEnvelope {
    pub proto: ProtocolVersion,
    pub response: SessionResponse,
}

impl ProtocolResponseEnvelope {
    /// Wrap a response at this build's protocol version.
    pub fn new(response: SessionResponse) -> Self {
        Self {
            proto: PROTOCOL_VERSION,
            response,
        }
    }
}
