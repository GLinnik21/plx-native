//! **What a PMS notification asks of the screens** (layer-2 view state, L2-4): the mapping from
//! [`NotifyOut`] (what other clients are playing, already stripped of our own sessions) to reads of
//! OUR state. It ships dark with the feed (`notify::NOTIFY_ENABLED`).
//!
//! **A notification is a hint, never a playback event** (S5). Nothing here calls
//! `enqueue_stop`, adds an end-of-stream candidate or publishes an Offset: a stranger's `stopped`
//! session must not scrobble our other servers (`fan_out_origin_watched`). The only things a
//! notification can owe are
//!
//! * a read-back of an item a page HOLDS ([`Stores::holds`]: the keys the stores' enumerators name
//!   for the visible page and the covered ones, plus the Detail pages on the stack), for a session
//!   that is new or changed state;
//! * on `stopped`, one quiet refetch of that server's Home source (coalesced per pump and per
//!   server), plus, when the item is not held anywhere, `BrowseCmd::HubsInvalidateAll`: the section
//!   shelves are re-read whole, because the stop may have moved items between them.
//!
//! **Which read.** An item a Detail page on the stack shows is read the owing way
//! (`ViewReconcileStore::enqueue`): the landed row owes the page (`take_landed` ->
//! `app::content::reconcile_details` -> `DetailReconcile`), which re-reads in place with no focus
//! or scroll move. Any other held item is read QUIET (`enqueue_reread`): its tiles are patched, and
//! a Detail the viewer is looking at is not re-fetched for the sake of a card beneath it. The Detail
//! itself is never touched here; only its landed read-back reaches it.
//!
//! **Owed work is spent only when [`Stores::reread_allowed`]** (not under the player, not while a
//! recording or replay is armed). Until then it is queued, not dropped, and coalesced.
//!
//! `last_state` remembers each session's `(ratingKey, state)` so a steady `playing` ping is no news.
//! It is bounded ([`MAX_SESSIONS`], oldest dropped) and a server's entries are cleared when its feed
//! comes back after a gap: nothing said before the gap is known to still hold, so the first
//! report after it is news again.

use super::notify::NotifyOut;
use super::{browse, ContentArg, HeldKeys, Stores};
use plx_plex::plex::notify::PlayState;
use plx_plex::plex::ServerId;

/// Sessions remembered across all servers. A busy household's concurrent sessions are far fewer;
/// the bound is only a ceiling against a feed that mints keys forever, and an evicted session is
/// at worst treated as new (one more bounded read-back).
pub const MAX_SESSIONS: usize = 64;

type Key = (ServerId, String);

/// What one [`Stores::notify_map`] call actually spent.
#[derive(Default, Debug, PartialEq)]
pub struct Spent {
    /// Servers whose Home source was refetched quietly.
    pub refetched: Vec<ServerId>,
    /// `BrowseCmd::HubsInvalidateAll` was sent.
    pub invalidated: bool,
}

/// Read-backs and refetches a notification owes, not yet spent.
#[derive(Default)]
struct Owed {
    /// `(item, owes its Detail page)`; an owing ask is never downgraded to a quiet one.
    reads: Vec<(Key, bool)>,
    refetch: Vec<ServerId>,
    invalidate: bool,
}

impl Owed {
    fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.refetch.is_empty() && !self.invalidate
    }
    fn read(&mut self, key: Key, owing: bool) {
        match self.reads.iter_mut().find(|(k, _)| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
            Some((_, o)) => *o |= owing,
            None => self.reads.push((key, owing)),
        }
    }
    fn refetch(&mut self, sid: ServerId) {
        if !self.refetch.contains(&sid) {
            self.refetch.push(sid);
        }
    }
}

#[derive(Default)]
pub struct NotifyMap {
    /// `((server, sessionKey), (ratingKey, state))`, oldest first.
    last_state: Vec<((ServerId, String), (String, PlayState))>,
    owed: Owed,
}

impl NotifyMap {
    pub fn tracked(&self) -> usize {
        self.last_state.len()
    }

    pub fn owed_is_empty(&self) -> bool {
        self.owed.is_empty()
    }

    /// Forget everything: the identity changed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record the session's latest state; `true` when that is news (a new session, or a changed
    /// item or state).
    fn observe(&mut self, sid: ServerId, session: &str, rk: &str, state: PlayState) -> bool {
        let id = (sid, session.to_owned());
        if let Some(at) = self.last_state.iter().position(|(k, _)| *k == id) {
            let entry = &mut self.last_state[at].1;
            if entry.0 == rk && entry.1 == state {
                return false;
            }
            *entry = (rk.to_owned(), state);
            return true;
        }
        if self.last_state.len() >= MAX_SESSIONS {
            self.last_state.remove(0);
        }
        self.last_state.push((id, (rk.to_owned(), state)));
        true
    }

    fn forget_server(&mut self, sid: ServerId) {
        self.last_state.retain(|((s, _), _)| *s != sid);
    }
}

/// What the pages hold: the stores' enumerated cards, and the items of the Detail pages on the stack.
struct Held {
    cards: HeldKeys,
    details: Vec<Key>,
}

impl Held {
    fn card(&self, sid: ServerId, rk: &str) -> bool {
        self.cards.iter().any(|(s, k)| plx_plex::plex::same_item((*s, k), (sid, rk)))
    }
    fn detail(&self, sid: ServerId, rk: &str) -> bool {
        self.details.iter().any(|(s, k)| plx_plex::plex::same_item((*s, k), (sid, rk)))
    }
}

impl Stores {
    /// What the pages hold, from each store's enumerator. Home's hold window, the Library page's
    /// shelves and grid window and Search's results are the single current ones; Person, Filmography
    /// and Collection are asked about the pages on the stack (`pages`, bottom to top: the visible page
    /// and every covered one), and a Detail page holds its own item.
    fn held(&self, pages: &[ContentArg]) -> Held {
        let mut cards = self.hubs.held_keys();
        let mut add = |keys: HeldKeys| for (sid, rk) in keys {
            super::push_held(&mut cards, sid, &rk);
        };
        add(self.library_held_keys());
        add(self.search.held_keys());
        let mut details = Vec::new();
        for page in pages {
            match page {
                ContentArg::Detail { sid, rk } => details.push((*sid, rk.clone())),
                ContentArg::Person { .. } | ContentArg::Filmography { .. } => add(self.person.held_keys(page)),
                ContentArg::Collection(_) => add(self.collection.held_keys(page)),
            }
        }
        Held { cards, details }
    }

    /// Does any page, visible or covered, show `(sid, rk)`? `pages` are the content pages on the
    /// navigation stack.
    pub fn holds(&self, sid: ServerId, rk: &str, pages: &[ContentArg]) -> bool {
        let held = self.held(pages);
        held.card(sid, rk) || held.detail(sid, rk)
    }

    /// Map one pump's notifications to the reads they owe, then spend what may be spent now.
    /// Called once a frame (with an empty `out` when nothing arrived) so queued work is released
    /// the frame the network frees up. MAIN THREAD.
    pub fn notify_map(
        &mut self,
        out: &NotifyOut,
        pages: &[ContentArg],
        directory: browse::DirectoryView<'_>,
    ) -> Spent {
        for (sid, _epoch) in &out.resynced {
            self.notify_map.forget_server(*sid);
        }
        if !out.plays.is_empty() {
            let held = self.held(pages);
            for plays in &out.plays {
                for s in &plays.sessions {
                    // Without a rating key there is no item to read; without a session key the item
                    // stands in (one report per item, which is all the cache needs).
                    let Some(rk) = s.rating_key.as_deref().filter(|rk| !rk.is_empty()) else { continue };
                    let session = s.session_key.as_deref().unwrap_or(rk);
                    if !self.notify_map.observe(plays.sid, session, rk, s.state) {
                        continue;
                    }
                    let detail = held.detail(plays.sid, rk);
                    let is_held = detail || held.card(plays.sid, rk);
                    let owed = &mut self.notify_map.owed;
                    if is_held {
                        owed.read((plays.sid, rk.to_owned()), detail);
                    }
                    if s.state == PlayState::Stopped {
                        owed.refetch(plays.sid);
                        owed.invalidate |= !is_held;
                    }
                }
            }
        }
        self.spend_notify_owed(directory)
    }

    fn spend_notify_owed(&mut self, directory: browse::DirectoryView<'_>) -> Spent {
        let mut spent = Spent::default();
        if self.notify_map.owed.is_empty() || !self.reread_allowed() {
            return spent;
        }
        let owed = std::mem::take(&mut self.notify_map.owed);
        for (key, owing) in owed.reads {
            if owing {
                self.reconcile.enqueue([key]);
            } else {
                self.reconcile.enqueue_reread([key]);
            }
        }
        for sid in owed.refetch {
            let _ = self.hubs.run_with_directory(super::hubs::HubsCmd::RefetchSourceQuiet { sid }, directory);
            spent.refetched.push(sid);
        }
        if owed.invalidate {
            let _ = self.browse.borrow_mut().run(browse::BrowseCmd::HubsInvalidateAll);
            spent.invalidated = true;
        }
        spent
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn notify_owed_for_test(&self) -> Vec<(ServerId, String)> {
        let owed = &self.notify_map.owed;
        let mut all: Vec<_> = owed.reads.iter().map(|(k, _)| k.clone()).collect();
        all.extend(owed.refetch.iter().map(|s| (*s, String::new())));
        if owed.invalidate {
            all.push((ServerId::UNSET, "invalidate".into()));
        }
        all
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn notify_tracked_for_test(&self) -> usize {
        self.notify_map.tracked()
    }
}
