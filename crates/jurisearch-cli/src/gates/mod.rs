//! Release gates: the Phase 1 / Phase 2 truthfulness gates that re-derive pass/fail from
//! benchmark artifacts (never trusting a self-reported state). `support` holds the shared
//! dotted-pointer artifact mechanics; `phase1`/`phase2` hold the floors, claims, and status
//! derivation.

pub(crate) mod phase1;
pub(crate) mod phase2;
pub(crate) mod support;

pub(crate) use phase1::*;
pub(crate) use phase2::*;
pub(crate) use support::*;
