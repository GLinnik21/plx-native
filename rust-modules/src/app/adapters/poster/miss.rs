//! The session's SETTLED MISSES of the two app-baked card kinds (`/plx/cover/…`, `/plx/card/…`),
//! kept beside the slots instead of in them.
//!
//! A slot that settled as `P_FAILED` is recyclable once no draw has touched it for
//! `WANT_STALE_FRAMES` frames (`super::victim`), and a recycled key reads "never requested", which
//! is not failed. A deck card whose cover settled as a miss would then re-ask for it, dial plex.tv
//! again and (the cover is a WAN read) pay the connect budget once per card per recycle. This set
//! is what survives the recycle: [`super::settled_failed`] and `lookup`'s claim both consult it, so
//! the card walks on to its next rung and a prefetch does not re-claim the key.
//!
//! An entry is only as good as the identity it was recorded under, the same three the slot check
//! uses: the disk epoch (`imgcache::generation`, moved by a sign-out), the server's `grant_epoch`
//! (moved by every revocation) and the grant's `token_gen`. `grant_epoch` alone would not do: a
//! profile switch retokens the SAME client in place, which leaves it unchanged but moves
//! `token_gen`, so a miss recorded under one profile is asked again under the next. A newer disk
//! epoch empties the set.
//!
//! Bounded ([`MISS_CAP`], oldest out): its natural size is the number of distinct deck titles seen
//! in a session, and an eviction only means one more ask, never missing art.

use plx_plex::plex::ServerId;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// Misses kept. Far above a session's deck titles; precedent: `refresh.rs`'s `RECENT_CAP`.
pub(super) const MISS_CAP: usize = 1024;

/// The identity an entry was recorded under, and the one a lookup must present live.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Currency {
    pub cache_gen: u64,
    pub grant_epoch: u32,
    pub token_gen: u32,
}

impl Currency {
    /// The identity in force for `srv` now (`0, 0` for a server the registry does not resolve, as
    /// the slot check reads it).
    pub(super) fn live(srv: ServerId) -> Currency {
        let (token_gen, grant_epoch) = plx_plex::plex::client_for(srv)
            .map_or((0, 0), |c| (c.token_gen(), c.grant_epoch()));
        Currency { cache_gen: plx_platform::imgcache::generation(), grant_epoch, token_gen }
    }
}

/// The bounded set. Nested by server so a hit borrows the key (no allocation on the per-frame
/// path, the reason `KeyMemo` nests too).
#[derive(Default)]
pub(super) struct MissSet {
    by_server: HashMap<u16, HashMap<String, Currency>>,
    order: VecDeque<(u16, String)>,
    epoch: u64,
}

impl MissSet {
    pub(super) fn insert(&mut self, srv: u16, key: &str, cur: Currency) {
        if cur.cache_gen < self.epoch {
            return; // a late answer of a signed-out account
        }
        if cur.cache_gen > self.epoch {
            self.by_server.clear();
            self.order.clear();
            self.epoch = cur.cache_gen;
        }
        let known = self.by_server.entry(srv).or_default().insert(key.to_owned(), cur).is_some();
        if known {
            return;
        }
        self.order.push_back((srv, key.to_owned()));
        if self.order.len() > MISS_CAP {
            if let Some((old_srv, old)) = self.order.pop_front() {
                if let Some(m) = self.by_server.get_mut(&old_srv) {
                    m.remove(&old);
                }
            }
        }
    }

    pub(super) fn contains(&self, srv: u16, key: &str, cur: Currency) -> bool {
        cur.cache_gen == self.epoch
            && self.by_server.get(&srv).and_then(|m| m.get(key)) == Some(&cur)
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.order.len()
    }
}

static MISSES: Mutex<Option<MissSet>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut MissSet) -> R) -> R {
    let mut g = MISSES.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(MissSet::default))
}

/// Record a settled miss of `key` on `srv`, under the identity its bake started with.
pub(super) fn note(srv: ServerId, key: &str, cur: Currency) {
    with(|m| m.insert(srv.raw(), key, cur));
}

/// Has `key` on `srv` settled as a miss this session, under the identity in force NOW?
pub(super) fn known(srv: ServerId, key: &str, cur: Currency) -> bool {
    with(|m| m.contains(srv.raw(), key, cur))
}

/// Forget every miss. Callers hold `plx_base::testlock::serial()`: this is a process global.
#[cfg(test)]
pub(super) fn reset_for_test() {
    with(|m| *m = MissSet::default());
}
