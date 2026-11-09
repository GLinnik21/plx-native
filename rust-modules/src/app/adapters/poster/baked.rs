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
pub(super) struct Bake {
    pub got: Got<Rgba>,
    pub stale: bool,
}

impl Bake {
    pub(super) fn fresh(got: Got<Rgba>) -> Bake {
        Bake { got, stale: false }
    }
}
