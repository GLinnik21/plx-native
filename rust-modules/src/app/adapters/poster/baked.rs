//! What the two app-baked card kinds ([`super::cover`], [`super::card`]) share: the shape of one
//! bake's answer and of a disk entry read back. The fan has its own ([`super::fan::Got`] is the one
//! three-way answer they all reuse).

use super::fan::{Got, Rgba};

/// A persisted entry read back from the disk tier: its bytes and whether it is older than the
/// tier's `REFRESH_AFTER` (a stale entry is still served, then revalidated on the refresh lane).
pub(super) struct Cached {
    pub bytes: Vec<u8>,
    pub stale: bool,
}

/// One bake's answer: the delivery, and whether what it was served from is stale, so the worker
/// queues a re-bake on the refresh lane after publishing it. A bake that fetched fresh is never
/// stale.
///
/// `lasting` says how long a `Got::Final` holds: `true` for a definitive answer (a "no cover"
/// marker, a transcoder 4xx, bytes that cannot be used), which the session's miss set keeps for the
/// whole identity; `false` for a miss that came from a read that did not answer (Discover
/// unreachable or refusing, a 5xx, no response), which the set forgets with the Discover memo's
/// window ([`super::miss::note_for`]) so the card asks again once the network may be back. It means
/// nothing for `Ok` and `Transient`.
pub(super) struct Bake {
    pub got: Got<Rgba>,
    pub stale: bool,
    pub lasting: bool,
}

impl Bake {
    pub(super) fn fresh(got: Got<Rgba>) -> Bake {
        Bake { got, stale: false, lasting: true }
    }

    /// A `Final` that a read which did not answer produced: not a verdict about the title.
    pub(super) fn unanswered() -> Bake {
        Bake { got: Got::Final, stale: false, lasting: false }
    }
}
