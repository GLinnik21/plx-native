//! **What a PMS notification asks of the screens** (layer-2 view state, L2-4): the mapping from
//! [`NotifyOut`] (what other clients are playing, already stripped of our own sessions) to reads of
//! OUR state. It runs with the feed (`notify::NOTIFY_ENABLED`, owned servers only).
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
//!   server), plus, when the item is not held anywhere, `BrowseCmd::HubsInvalidateServer` for THAT
//!   server: its section shelves are re-read whole, because the stop may have moved items between
//!   them. It is a hint, not a press: it leaves another server's sections alone, does not reset a
//!   failing section's backoff and is spaced per section like the quiet Home refetch.
//!
//! **Which read.** An item a Detail page on the stack shows is read the owing way
//! (`ViewReconcileStore::enqueue`): the landed row owes the page (`take_landed` ->
//! `app::content::reconcile_details` -> `DetailReconcile`), which re-reads in place with no focus
//! or scroll move. Any other held item is read QUIET (`enqueue_reread`): its tiles are patched, and
//! a Detail the viewer is looking at is not re-fetched for the sake of a card beneath it. The Detail
//! itself is never touched here; only its landed read-back reaches it.
//!
//! **Library changes follow the Watchlist** (plan §1.11). A finished movie or show created or
//! deleted on a server ([`NotifyLibrary`]) owes the Watchlist store one re-match of that server: a
//! DELETE of an item a tile is matched to asks again about that guid, a create asks again about the
//! titles of that kind the server was not seen to hold, in chunks of `GUIDS_PER_REQUEST`. The owe is
//! one per `(server, kind)` however many entries arrive, is made at most once per
//! [`LIBRARY_SPACING_FRAMES`] (the quiet-refetch spacing), waits for the Watchlist to have loaded
//! (the first read matches the library as it is then: a change that arrives while the list is not
//! ready and no read is out is dropped, since the read that follows asks after it; one that arrives
//! while the first read is out is kept, since that read's matching may already have asked) and is made through the store's own
//! worker; the tile flips when the answer lands, through the same §1.5 derivation as any
//! re-match.
//!
//! **Library changes also move the server's lists.** The same entries owe that server one quiet
//! Home refetch and one section-shelf invalidate (coalesced and spaced like a `stopped`'s), and its
//! library grids one re-list (`BrowseCmd::ListingsChanged`): the open grid is listed again where
//! the reader stands, the server's other sections fetch afresh on their next visit.
//!
//! **Owed work is spent only when [`Stores::reread_allowed`]** (not under the player, not while a
//! recording or replay is armed). Until then it is queued, not dropped, and coalesced.
//!
//! `last_state` remembers each session's `(ratingKey, state)` so a steady `playing` ping is no news.
//! It is bounded ([`MAX_SESSIONS`], the longest-quiet session dropped, a stopped one first) and a server's entries are cleared when its feed
//! comes back after a gap: nothing said before the gap is known to still hold, so the first
//! report after it is news again.

use super::notify::{NotifyLibrary, NotifyOut, FRAMES_PER_S};
use super::{browse, ContentArg, HeldKeys, Stores};
use crate::watchlist::model::Kind as WatchKind;
use plx_plex::plex::notify::{LibraryKind, LibraryState, PlayState};
use plx_plex::plex::ServerId;
use std::collections::HashSet;

/// Sessions remembered across all servers. A busy household's concurrent sessions are far fewer;
/// the bound is only a ceiling against a feed that mints keys forever, and an evicted session is
/// at worst treated as new (one more bounded read-back).
pub const MAX_SESSIONS: usize = 64;

/// The least spacing between two library re-matches of one `(server, kind)`, in pumped frames: the
/// quiet-refetch spacing of a Home source (`pms::QUIET_SPACING_S`).
pub const LIBRARY_SPACING_FRAMES: u64 = crate::pms::QUIET_SPACING_S as u64 * FRAMES_PER_S;

type Key = (ServerId, String);

/// Sessions whose state CHANGED this pump (what `observe` accepted), by state. A steady `playing`
/// frame repeats every few seconds for the length of a film and is no news, so it is not counted
/// and not logged. Counts only: a session names another household member's playback, so no key,
/// title or client id is ever logged.
#[derive(Default)]
struct Transitions([usize; 5]);

impl Transitions {
    fn note(&mut self, state: PlayState) {
        self.0[match state {
            PlayState::Playing => 0,
            PlayState::Paused => 1,
            PlayState::Buffering => 2,
            PlayState::Stopped => 3,
            PlayState::Other => 4,
        }] += 1;
    }

    /// One line per pump that saw a transition; nothing otherwise.
    fn emit(&self) {
        if self.0.iter().all(|n| *n == 0) {
            return;
        }
        let by = self.0;
        plx_base::eventlog::log(&format!(
            "notify: sessions playing={} paused={} buffering={} stopped={} other={}",
            by[0], by[1], by[2], by[3], by[4]
        ));
    }
}

/// What one [`Stores::notify_map`] call actually spent.
#[derive(Default, Debug, PartialEq)]
pub struct Spent {
    /// Servers whose Home source was refetched quietly.
    pub refetched: Vec<ServerId>,
    /// Servers whose section shelves were invalidated (`BrowseCmd::HubsInvalidateServer`).
    pub invalidated: Vec<ServerId>,
    /// Servers whose library grids were re-listed (`BrowseCmd::ListingsChanged`).
    pub relisted: Vec<ServerId>,
}

/// Read-backs and refetches a notification owes, not yet spent.
#[derive(Default)]
struct Owed {
    /// `(item, owes its Detail page)`; an owing ask is never downgraded to a quiet one.
    reads: Vec<(Key, bool)>,
    refetch: Vec<ServerId>,
    invalidate: Vec<ServerId>,
    /// Servers whose library grids are out of date (`BrowseCmd::ListingsChanged`).
    listings: Vec<ServerId>,
}

fn push_unique(servers: &mut Vec<ServerId>, sid: ServerId) {
    if !servers.contains(&sid) {
        servers.push(sid);
    }
}

impl Owed {
    fn is_empty(&self) -> bool {
        self.reads.is_empty() && self.refetch.is_empty() && self.invalidate.is_empty() && self.listings.is_empty()
    }
    fn read(&mut self, key: Key, owing: bool) {
        match self.reads.iter_mut().find(|(k, _)| plx_plex::plex::same_item((k.0, &k.1), (key.0, &key.1))) {
            Some((_, o)) => *o |= owing,
            None => self.reads.push((key, owing)),
        }
    }
    fn refetch(&mut self, sid: ServerId) {
        push_unique(&mut self.refetch, sid);
    }
    fn invalidate(&mut self, sid: ServerId) {
        push_unique(&mut self.invalidate, sid);
    }
}

/// What a server's library changes owe the Watchlist for one kind, not yet spent.
#[derive(Default)]
struct LibraryOwed {
    /// Guids a tile was matched to by an item that has since been deleted on the server.
    gone: Vec<String>,
    /// An item was created: ask again about every title the server was not seen to hold.
    added: bool,
    /// The exact change was not followed (the Watchlist had not loaded, or the feed had a gap):
    /// ask again about every title of the kind.
    sweep: bool,
}

#[derive(Default)]
pub struct NotifyMap {
    /// `((server, sessionKey), (ratingKey, state))`, least recently seen first.
    last_state: Vec<((ServerId, String), (String, PlayState))>,
    owed: Owed,
    /// Library re-matches owed to the Watchlist, one per `(server, kind)`.
    library: Vec<((ServerId, WatchKind), LibraryOwed)>,
    /// The frame from which a `(server, kind)` may be re-matched again.
    library_next: Vec<((ServerId, WatchKind), u64)>,
    /// Calls of [`Stores::notify_map`] so far: the clock the spacing is counted in.
    frame: u64,
}

impl NotifyMap {
    pub fn tracked(&self) -> usize {
        self.last_state.len()
    }

    pub fn owed_is_empty(&self) -> bool {
        self.owed.is_empty() && self.library.is_empty()
    }

    /// Forget everything: the identity changed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record the session's latest state; `true` when that is news (a new session, or a changed
    /// item or state). `Buffering` is `Playing` (a stalled stream flips between them on its own and
    /// the item is the same one), and `Other` is no state at all: neither is news, and an `Other`
    /// report neither creates nor changes an entry. The entry just seen moves to the back, so the
    /// bound evicts the session that has been quiet longest, and prefers a `Stopped` one to any
    /// live one (it is over; the dedupe of its repeated reports is all it still buys).
    fn observe(&mut self, sid: ServerId, session: &str, rk: &str, state: PlayState) -> bool {
        let state = match state {
            PlayState::Other => return false,
            PlayState::Buffering => PlayState::Playing,
            other => other,
        };
        let id = (sid, session.to_owned());
        if let Some(at) = self.last_state.iter().position(|(k, _)| *k == id) {
            let (key, (was_rk, was)) = self.last_state.remove(at);
            let news = was_rk != rk || was != state;
            self.last_state.push((key, (rk.to_owned(), state)));
            return news;
        }
        if self.last_state.len() >= MAX_SESSIONS {
            let victim = self.last_state.iter().position(|(_, (_, st))| *st == PlayState::Stopped).unwrap_or(0);
            self.last_state.remove(victim);
        }
        self.last_state.push((id, (rk.to_owned(), state)));
        true
    }

    fn forget_server(&mut self, sid: ServerId) {
        self.last_state.retain(|((s, _), _)| *s != sid);
    }

    fn library_owe(&mut self, sid: ServerId, kind: WatchKind) -> &mut LibraryOwed {
        let at = match self.library.iter().position(|(k, _)| *k == (sid, kind)) {
            Some(at) => at,
            None => {
                self.library.push(((sid, kind), LibraryOwed::default()));
                self.library.len() - 1
            }
        };
        &mut self.library[at].1
    }

    fn library_due(&self, sid: ServerId, kind: WatchKind) -> bool {
        self.library_next.iter().find(|(k, _)| *k == (sid, kind)).is_none_or(|(_, at)| self.frame >= *at)
    }

    fn library_spent(&mut self, sid: ServerId, kind: WatchKind) {
        let next = self.frame + LIBRARY_SPACING_FRAMES;
        match self.library_next.iter_mut().find(|(k, _)| *k == (sid, kind)) {
            Some((_, at)) => *at = next,
            None => self.library_next.push(((sid, kind), next)),
        }
    }
}

fn watch_kind(kind: LibraryKind) -> WatchKind {
    match kind {
        LibraryKind::Movie => WatchKind::Movie,
        LibraryKind::Show => WatchKind::Show,
    }
}

/// Which of the app's list pages are on the navigation stack, visible or covered. The Home, Library,
/// Search and Watchlist stores each keep the last page's rows after the page is popped, so what they
/// enumerate is held only while their page is still there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shown {
    pub home: bool,
    pub library: bool,
    pub search: bool,
    pub watchlist: bool,
}

impl Shown {
    pub const ALL: Self = Self { home: true, library: true, search: true, watchlist: true };
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
    /// and every covered one), and a Detail page holds its own item, and, while it is the loaded one, the episodes of its
    /// current season window (owing) and its related and collection tiles (quiet).
    fn held(&self, pages: &[ContentArg], shown: Shown) -> Held {
        let mut cards = if shown.home { self.hubs.held_keys() } else { HeldKeys::new() };
        let mut add = |keys: HeldKeys| for (sid, rk) in keys {
            super::push_held(&mut cards, sid, &rk);
        };
        if shown.library {
            add(self.library_held_keys());
        }
        if shown.search {
            add(self.search.held_keys());
        }
        if shown.watchlist {
            add(self.watchlist.held_keys());
        }
        let mut details = Vec::new();
        for page in pages {
            match page {
                ContentArg::Detail { sid, rk } => {
                    details.push((*sid, rk.clone()));
                    // The loaded Detail also shows episodes (read the owing way: the page cannot be
                    // patched from their facts) and related tiles (patched, so read quiet).
                    let view = self.metadata.view();
                    details.extend(view.held_episodes(*sid, rk));
                    add(view.held_tiles(*sid, rk));
                }
                ContentArg::Person { .. } | ContentArg::Filmography { .. } => add(self.person.held_keys(page)),
                ContentArg::Collection(_) => add(self.collection.held_keys(page)),
                // A title no server holds: no card of its own to owe a read-back.
                ContentArg::Catalog { .. } => {}
            }
        }
        Held { cards, details }
    }

    /// Does any page, visible or covered, show `(sid, rk)`? `pages` are the content pages on the
    /// navigation stack and `shown` says which list pages are on it too.
    pub fn holds(&self, sid: ServerId, rk: &str, pages: &[ContentArg], shown: Shown) -> bool {
        let held = self.held(pages, shown);
        held.card(sid, rk) || held.detail(sid, rk)
    }

    /// Map one pump's notifications to the reads they owe, then spend what may be spent now.
    /// Called once a frame (with an empty `out` when nothing arrived) so queued work is released
    /// the frame the network frees up. MAIN THREAD.
    pub fn notify_map(
        &mut self,
        out: &NotifyOut,
        pages: &[ContentArg],
        shown: Shown,
        directory: browse::DirectoryView<'_>,
    ) -> Spent {
        self.notify_map.frame += 1;
        for (sid, _epoch) in &out.resynced {
            self.notify_map.forget_server(*sid);
            // Whatever the library did during the gap was missed.
            if self.watchlist.is_ready() {
                for kind in [WatchKind::Movie, WatchKind::Show] {
                    self.notify_map.library_owe(*sid, kind).sweep = true;
                }
            }
        }
        for library in &out.library {
            self.owe_library(library);
            // The server's lists moved: its Home source, its section shelves and its grids.
            let owed = &mut self.notify_map.owed;
            owed.refetch(library.sid);
            owed.invalidate(library.sid);
            push_unique(&mut owed.listings, library.sid);
        }
        if !out.plays.is_empty() {
            let mut changed = Transitions::default();
            let held = self.held(pages, shown);
            for plays in &out.plays {
                for s in &plays.sessions {
                    // Without a rating key there is no item to read. Without a session key the
                    // client and the item stand in: two clients playing one item are two sessions.
                    let Some(rk) = s.rating_key.as_deref().filter(|rk| !rk.is_empty()) else { continue };
                    let session = match s.session_key.as_deref().filter(|k| !k.is_empty()) {
                        Some(key) => key.to_owned(),
                        None => format!("{}\u{1f}{rk}", s.client_identifier.as_deref().unwrap_or("")),
                    };
                    if !self.notify_map.observe(plays.sid, &session, rk, s.state) {
                        continue;
                    }
                    changed.note(s.state);
                    let detail = held.detail(plays.sid, rk);
                    let is_held = detail || held.card(plays.sid, rk);
                    let owed = &mut self.notify_map.owed;
                    if is_held {
                        owed.read((plays.sid, rk.to_owned()), detail);
                    }
                    if s.state == PlayState::Stopped {
                        owed.refetch(plays.sid);
                        if !is_held {
                            owed.invalidate(plays.sid);
                        }
                    }
                }
            }
            changed.emit();
        }
        self.spend_notify_owed(directory)
    }

    /// Note what a server's finished creates and deletes owe the Watchlist. Counts only are logged:
    /// an item names a title in somebody's library.
    fn owe_library(&mut self, library: &NotifyLibrary) {
        let sid = library.sid;
        let ready = self.watchlist.is_ready();
        // Before the first load the read to come matches the library as it is then, which already
        // holds this change: owing a re-match for it would only ask the whole list again right after
        // boot. A read already out may have asked before the change landed on the server, so that
        // one keeps its owe (and is spent once the list is ready).
        let follow = ready || self.watchlist.read_in_flight();
        // A delete is followed to the tile it was matched to; one lookup table per server and frame.
        let held = (ready && library.entries.iter().any(|e| e.state == LibraryState::Deleted))
            .then(|| self.watchlist.held_by_rk(sid));
        let (mut created, mut deleted) = (0, 0);
        for e in &library.entries {
            let kind = watch_kind(e.kind);
            match e.state {
                LibraryState::Created if !follow => created += 1,
                LibraryState::Deleted if !follow => deleted += 1,
                LibraryState::Created => {
                    created += 1;
                    self.notify_map.library_owe(sid, kind).added = true;
                }
                LibraryState::Deleted => {
                    deleted += 1;
                    let Some(held) = &held else {
                        self.notify_map.library_owe(sid, kind).sweep = true;
                        continue;
                    };
                    let tile = e.item_id.trim().parse::<i64>().ok().and_then(|rk| held.get(&rk));
                    if let Some((guid, _)) = tile {
                        let gone = &mut self.notify_map.library_owe(sid, kind).gone;
                        if !gone.contains(guid) {
                            gone.push(guid.clone());
                        }
                    }
                }
            }
        }
        plx_base::eventlog::log(&format!(
            "notify: library sid={} created={created} deleted={deleted}",
            sid.raw()
        ));
    }

    /// Spend the library re-matches that are due: the Watchlist has loaded, the `(server, kind)` is
    /// outside its spacing and the store could start its worker. Anything else stays queued.
    fn spend_library_owed(&mut self) {
        let queued = std::mem::take(&mut self.notify_map.library);
        let mut kept = Vec::new();
        for ((sid, kind), owe) in queued {
            if !self.watchlist.is_ready() || !self.notify_map.library_due(sid, kind) {
                kept.push(((sid, kind), owe));
                continue;
            }
            let mut asks: Vec<(String, WatchKind)> = owe.gone.iter().map(|g| (g.clone(), kind)).collect();
            if owe.sweep {
                asks.extend(self.watchlist.rows_of(kind));
            } else if owe.added {
                asks.extend(self.watchlist.unmatched_on(sid, kind));
            }
            let mut seen = HashSet::new();
            asks.retain(|(g, _)| seen.insert(g.clone()));
            let n = asks.len();
            if !self.watchlist.rematch_on(sid, asks) {
                kept.push(((sid, kind), owe));
                continue;
            }
            if n > 0 {
                self.notify_map.library_spent(sid, kind);
                plx_base::eventlog::log(&format!("notify: library re-match sid={} guids={n}", sid.raw()));
            }
        }
        self.notify_map.library = kept;
    }

    fn spend_notify_owed(&mut self, directory: browse::DirectoryView<'_>) -> Spent {
        let mut spent = Spent::default();
        if self.notify_map.owed_is_empty() || !self.reread_allowed() {
            return spent;
        }
        self.spend_library_owed();
        if self.notify_map.owed.is_empty() {
            return spent;
        }
        let owed = std::mem::take(&mut self.notify_map.owed);
        plx_base::eventlog::log(&format!(
            "notify: owes reads={} (detail={}) refetch={} invalidate={} listings={}",
            owed.reads.len(),
            owed.reads.iter().filter(|(_, d)| *d).count(),
            owed.refetch.len(),
            owed.invalidate.len(),
            owed.listings.len()
        ));
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
        for sid in owed.listings {
            let _ = self.browse.borrow_mut().run(browse::BrowseCmd::ListingsChanged { sid });
            spent.relisted.push(sid);
        }
        for sid in owed.invalidate {
            let _ = self.browse.borrow_mut().run(browse::BrowseCmd::HubsInvalidateServer { sid });
            spent.invalidated.push(sid);
        }
        spent
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn notify_owed_for_test(&self) -> Vec<(ServerId, String)> {
        let owed = &self.notify_map.owed;
        let mut all: Vec<_> = owed.reads.iter().map(|(k, _)| k.clone()).collect();
        all.extend(owed.refetch.iter().map(|s| (*s, String::new())));
        all.extend(owed.invalidate.iter().map(|s| (*s, "invalidate".into())));
        all.extend(owed.listings.iter().map(|s| (*s, "listings".into())));
        all.extend(self.notify_map.library.iter().map(|((s, _), _)| (*s, "library".into())));
        all
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn notify_tracked_for_test(&self) -> usize {
        self.notify_map.tracked()
    }
}
