//! The two distinct sequence layers (design §5.1).
//!
//! The producer keeps two *non-interchangeable* ordering coordinates and the type system enforces
//! that they never mix — mixing them is exactly the §5.1 cross-corpus `sequence_gap` hazard:
//!
//! * [`ChangeSeq`] — a **global** build/audit ordering across all corpora (the outbox
//!   `package_change_log.change_seq bigserial`). It interleaves corpora.
//! * [`PackageSequence`] — a **per-corpus**, gap-free monotonic package-chain counter, assigned at
//!   build time, one chain per corpus. Every `from_sequence`/`to_sequence`, the remote manifest's
//!   `head_sequence`/`min_available_sequence`/`catchup_ranges`, and `corpus_state.sequence` live in
//!   this space — **never** in `change_seq` space.
//!
//! Neither type offers a conversion to or from the other, nor any cross-type arithmetic. The only
//! way to obtain one from the other is the producer **package catalog** (a build-time table), which
//! is deliberately outside this contract crate.
//!
//! Both wrap a `u64`, so a negative sequence is **unrepresentable** and `serde` rejects a negative
//! wire value automatically (`bigserial`/package sequences are non-negative). [`ChangeSeq::from_db`]
//! / [`PackageSequence::from_db`] convert a Postgres `bigserial` (`i64`) at the DB boundary, failing
//! loudly on the impossible negative case.

use serde::{Deserialize, Serialize};

/// Failure converting a Postgres `bigserial` (`i64`) into a sequence newtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("sequence value {value} is negative, which is impossible for a bigserial sequence")]
pub struct NegativeSequence {
    pub value: i64,
}

/// Global build/audit ordering across all corpora — the outbox `change_seq` (design §5.1).
///
/// This is **not** a package-chain coordinate. It interleaves corpora and is used only to order and
/// audit the build; package boundaries are never read off it (that would trip a false
/// `sequence_gap`). Kept structurally distinct from [`PackageSequence`] so the two cannot be mixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangeSeq(u64);

impl ChangeSeq {
    /// The lowest meaningful value (`bigserial` starts at 1; 0 is a usable "before anything" floor).
    pub const ZERO: ChangeSeq = ChangeSeq(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Convert a Postgres `bigserial` (`i64`) read from the outbox.
    ///
    /// # Errors
    /// [`NegativeSequence`] if `value < 0` (impossible for a real `bigserial`).
    pub fn from_db(value: i64) -> Result<Self, NegativeSequence> {
        u64::try_from(value)
            .map(Self)
            .map_err(|_| NegativeSequence { value })
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Per-corpus, gap-free package-chain counter assigned at build time (design §5.1).
///
/// Every wire/cursor sequence field (`from_sequence`, `to_sequence`, `head_sequence`,
/// `min_available_sequence`, `corpus_state.sequence`) is a `PackageSequence`. Kept structurally
/// distinct from [`ChangeSeq`] so a global build-order value can never masquerade as a package
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageSequence(u64);

impl PackageSequence {
    /// The sequence a corpus has before any package is applied (an empty client).
    pub const NONE: PackageSequence = PackageSequence(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Convert a Postgres `bigint` (`i64`) read from `corpus_state`/the catalog.
    ///
    /// # Errors
    /// [`NegativeSequence`] if `value < 0` (impossible for a real package sequence).
    pub fn from_db(value: i64) -> Result<Self, NegativeSequence> {
        u64::try_from(value)
            .map(Self)
            .map_err(|_| NegativeSequence { value })
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next sequence in this corpus's chain (gap-free monotonic, design §7.3).
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    /// The sequence a client must currently be at for a package opening at `self` to apply
    /// (`expected_client_from_sequence == from_sequence - 1`, design §7.3). `None` at
    /// [`PackageSequence::NONE`], where there is no predecessor.
    #[must_use]
    pub const fn predecessor(self) -> Option<Self> {
        match self.0.checked_sub(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;

    /// §5.1 compile-time guard, asserted at runtime: the two sequence coordinates are distinct
    /// types. Combined with the *structural* absence of any `From`/`Into`/cross-arithmetic between
    /// them (there is intentionally none in this module), a `ChangeSeq` can never be passed where a
    /// `PackageSequence` is expected, and vice versa — the cross-corpus `sequence_gap` hazard.
    #[test]
    fn change_seq_and_package_sequence_are_non_interchangeable() {
        assert_ne!(
            TypeId::of::<ChangeSeq>(),
            TypeId::of::<PackageSequence>(),
            "ChangeSeq and PackageSequence must remain distinct types (design §5.1)"
        );
        // The values may print/compare identically as integers, but the types do not unify.
        let c = ChangeSeq::new(1041);
        let p = PackageSequence::new(1041);
        assert_eq!(c.get(), p.get());
        // There is no `c == p`, no `From<ChangeSeq> for PackageSequence`, and no `c + p`: those
        // would not compile, which is the actual guarantee this test documents.
    }

    #[test]
    fn package_sequence_chain_helpers() {
        let head = PackageSequence::new(1040);
        assert_eq!(head.next(), PackageSequence::new(1041));
        assert_eq!(head.next().predecessor(), Some(head));
        assert_eq!(PackageSequence::NONE.predecessor(), None);
    }

    #[test]
    fn sequences_round_trip_transparently() {
        let c = ChangeSeq::new(7);
        let p = PackageSequence::new(9);
        assert_eq!(serde_json::to_string(&c).unwrap(), "7");
        assert_eq!(serde_json::to_string(&p).unwrap(), "9");
        assert_eq!(serde_json::from_str::<ChangeSeq>("7").unwrap(), c);
        assert_eq!(serde_json::from_str::<PackageSequence>("9").unwrap(), p);
    }

    #[test]
    fn negative_wire_values_are_rejected() {
        // u64-backed newtypes: serde_json rejects a negative number outright.
        assert!(serde_json::from_str::<ChangeSeq>("-1").is_err());
        assert!(serde_json::from_str::<PackageSequence>("-1").is_err());
    }

    #[test]
    fn from_db_rejects_negative() {
        assert_eq!(ChangeSeq::from_db(5).unwrap(), ChangeSeq::new(5));
        assert_eq!(ChangeSeq::from_db(-1), Err(NegativeSequence { value: -1 }));
        assert!(PackageSequence::from_db(-9).is_err());
    }
}
