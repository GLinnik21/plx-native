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
//! **Library changes wait for the feed to go quiet** (#1458). A server's finished library entries
//! ([`NotifyLibrary`]) owe four things: one quiet Home refetch of that server, one section-shelf
//! invalidate for it, one `BrowseCmd::ListingsChanged` for its library listings and one Watchlist
//! re-match. None is made when the entry arrives. A scan or a bulk add delivers about one finished
//! entry a second for minutes, and over a remote link one row read takes seconds, so re-reading
//! on every entry kept every read the viewer scrolled to stuck in a queue behind them. The owe is
//! made ONCE, when either no entry for that server has arrived for [`LIBRARY_QUIET_FRAMES`] (10 s), or
//! [`LIBRARY_LONGEST_WAIT_FRAMES`] (60 s) have passed since the oldest entry not yet spent, so a
//! feed that never goes quiet still refreshes about once a minute. After that the next entry starts a
//! new wait. The wait is one `Settling` value per server; it is where the three server-list owes wait,
//! and nothing else waits there, so a `stopped` session's refetch and invalidate (above) are never
//! held by it. A feed gap's sweep and the `playing`/`stopped` path are not waited on, and the
//! spacings downstream (the quiet refetch's and the section shelves', `pms::QUIET_SPACING_S`) still
//! apply. Two waits of one server end at least [`LIBRARY_QUIET_FRAMES`] apart, so no spacing of its
//! own is kept between its re-lists.
//!
//! **Library changes follow the Watchlist** (plan §1.11). A finished movie or show created or
//! deleted on a server owes the Watchlist store one re-match of that server: a DELETE of the item a
//! tile is matched to asks again about that guid, a create asks again about the titles of that kind
//! the server was not seen to hold, in chunks of `GUIDS_PER_REQUEST`. The owe is one per
//! `(server, kind)` however many entries arrive, is made when the server's wait above is over and at
//! most once per [`LIBRARY_SPACING_FRAMES`] (the quiet-refetch spacing, which a feed gap's sweep right
//! after a re-match can still meet), waits for the Watchlist to have loaded
//! (the first read matches the library as it is then: a change that arrives while the list is not
//! ready and no read is out is dropped, since the read that follows asks after it; one that arrives
//! while the first read is out is kept, since that read's matching may already have asked) and is made through the store's own
//! worker. A delete kept that way cannot be followed to a tile, so it asks again about every title of
//! the kind, but it waits for its server's quiet like any other entry: only a feed gap's sweep skips
//! the wait. The tile flips when the answer lands, through the same §1.5 derivation as any
//! re-match.
//!
//! **Library changes also move the server's lists.** When the wait is over the server's Home source
//! is refetched quietly once and its section shelves are invalidated once (coalesced, as a `stopped`'s
//! are), and its library listings are told once (`BrowseCmd::ListingsChanged`). The open grid keeps
//! its rows while a replacement wanted window is read, then swaps in place. A change during that
//! read owes one more re-list. The server's other sections, and a section left while a re-list is
//! outstanding, forget their pages.
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

/// How long a server's feed must have been free of library entries before the refreshes those
/// entries owe are made, in pumped frames: 10 s.
pub const LIBRARY_QUIET_FRAMES: u64 = 10 * FRAMES_PER_S;

/// The longest the refreshes library entries owe are held back, counted from the oldest entry not
/// yet spent, in pumped frames: 60 s. A feed that never goes quiet (a scan, or a bulk add)
/// still refreshes the server's lists about once a minute.
pub const LIBRARY_LONGEST_WAIT_FRAMES: u64 = 60 * FRAMES_PER_S;

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
    /// Servers whose library listings were told of a change (`BrowseCmd::ListingsChanged`).
    pub relisted: Vec<ServerId>,
    /// The endpoint refreshes the quiet refetches' Hubs steps owe (a launch the runner refused is
    /// settled by the next Hubs step, this one included): the caller carries them.
    pub endpoints: super::EndpointRefreshSet,
}

/// Read-backs and refetches a notification owes, not yet spent.
#[derive(Default)]
struct Owed {
    /// `(item, owes its Detail page)`; an owing ask is never downgraded to a quiet one.
    reads: Vec<(Key, bool)>,
    refetch: Vec<ServerId>,
    invalidate: Vec<ServerId>,
    /// Servers whose library listings are out of date (`BrowseCmd::ListingsChanged`): the servers
    /// whose library wait has ended.
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
    /// Whether every title of the kind is asked again, because the exact change was not followed, and
    /// whether that waits for the server's quiet.
    sweep: Sweep,
    /// The server's wait that this owe was held for has ended ([`NotifyMap::library_wait_over`], from
    /// `release_settled_library`, and nowhere else). The owe then needs no later wait: a re-match the
    /// store could not start that frame (another re-match or a list read is out) stays queued and is
    /// made as soon as it can, not held again behind the wait the feed's next entry starts, which a
    /// feed that never goes quiet would stretch to a minute a time. An entry that merges into an owe
    /// already marked rides the re-match that is already due: what a re-match asks is worked out
    /// when it starts, so it covers that entry too.
    wait_over: bool,
}

impl LibraryOwed {
    /// Ask after every title of the kind, at least as eagerly as `how`.
    fn sweep_to(&mut self, how: Sweep) {
        self.sweep = self.sweep.max(how);
    }
}

/// How a `(server, kind)` owes the Watchlist a re-match of every title of the kind. The variants are in
/// the order of urgency, so two reasons to sweep merge into the more urgent one.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
enum Sweep {
    /// The owe asks only about what the entries named (`gone`, `added`).
    #[default]
    No,
    /// A delete could not be followed to a tile (the Watchlist had not loaded): the entries are still
    /// waited for like any other library change, and the sweep is made at the server's quiet.
    AfterQuiet,
    /// The feed had a gap, so the exact changes were missed: made at once, not held for the wait.
    Now,
}

/// A server's finished library entries that have not been spent, waiting for its feed to go quiet.
/// The server has a value here only while it waits (there is no "idle" flag to forget to clear): it
/// starts with the server's first entry, moves on with each later one and ends when the refreshes
/// the entries owe are spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Settling {
    /// The frame the oldest entry not yet spent arrived on.
    first: u64,
    /// The frame the newest one arrived on.
    last: u64,
    /// How many entries have arrived since `first` (counted for the log).
    entries: usize,
}

impl Settling {
    /// Is the wait over at `frame`: the feed has been free of this server's entries for
    /// [`LIBRARY_QUIET_FRAMES`], or the oldest has waited [`LIBRARY_LONGEST_WAIT_FRAMES`]?
    fn over(&self, frame: u64) -> bool {
        frame.saturating_sub(self.last) >= LIBRARY_QUIET_FRAMES
            || frame.saturating_sub(self.first) >= LIBRARY_LONGEST_WAIT_FRAMES
    }
}

#[derive(Default)]
pub struct NotifyMap {
    /// `((server, sessionKey), (ratingKey, state))`, least recently seen first.
    last_state: Vec<((ServerId, String), (String, PlayState))>,
    owed: Owed,
    /// The library entries each server's feed delivered that are not yet spent. This is the only
    /// place the server-list refreshes they owe wait (never in `owed`, which a `stopped` session
    /// spends at once), and the Watchlist re-matches below wait with it.
    settling: Vec<(ServerId, Settling)>,
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
        self.owed.is_empty() && self.library.is_empty() && self.settling.is_empty()
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

    /// `entries` finished library entries of `sid` arrived this pump: start its wait, or move it on.
    fn library_arrived(&mut self, sid: ServerId, entries: usize) {
        let frame = self.frame;
        match self.settling.iter_mut().find(|(s, _)| *s == sid) {
            Some((_, wait)) => {
                wait.last = frame;
                wait.entries = wait.entries.saturating_add(entries);
            }
            None => self.settling.push((sid, Settling { first: frame, last: frame, entries })),
        }
    }

    /// Is `sid` still waiting for its feed to go quiet?
    fn library_settling(&self, sid: ServerId) -> bool {
        self.settling.iter().any(|(s, _)| *s == sid)
    }

    /// `sid`'s wait ended: no re-match it has queued is held for a later wait any more.
    fn library_wait_over(&mut self, sid: ServerId) {
        for ((s, _), owe) in &mut self.library {
            if *s == sid {
                owe.wait_over = true;
            }
        }
    }

    /// End and return the waits that are over now; the rest go on.
    fn take_settled(&mut self) -> Vec<(ServerId, Settling)> {
        let frame = self.frame;
        let (over, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.settling).into_iter().partition(|(_, w)| w.over(frame));
        self.settling = waiting;
        over
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
        clients: &plx_plex::plex::ClientKeys,
    ) -> Spent {
        let _step = plx_base::step::StepScope::enter();
        self.notify_map.frame += 1;
        for (sid, _epoch) in &out.resynced {
            self.notify_map.forget_server(*sid);
            // Whatever the library did during the gap was missed.
            if self.watchlist.is_ready() {
                for kind in [WatchKind::Movie, WatchKind::Show] {
                    self.notify_map.library_owe(*sid, kind).sweep_to(Sweep::Now);
                }
            }
        }
        for library in &out.library {
            self.owe_library(library);
            // The server's lists moved, but they are read once its feed has gone quiet.
            self.notify_map.library_arrived(library.sid, library.entries.len());
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
        self.spend_notify_owed(shown, directory, clients)
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
                        self.notify_map.library_owe(sid, kind).sweep_to(Sweep::AfterQuiet);
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

    /// Spend the library re-matches that are due: the server's feed has gone quiet, the Watchlist has
    /// loaded, the `(server, kind)` is outside its spacing and the store could start its worker.
    /// Anything else stays queued. A feed gap's sweep is not held for the wait.
    fn spend_library_owed(&mut self) {
        let queued = std::mem::take(&mut self.notify_map.library);
        let mut kept = Vec::new();
        for ((sid, kind), owe) in queued {
            // A feed gap's sweep asks after every title of the kind, so it covers every entry that has
            // arrived; an entry still to come makes a new owe, which waits for its own quiet. It is not
            // held. A delete that could not be followed sweeps too, but it is one of those entries.
            // An owe whose own wait has ended (and could not be spent that frame) is not held for the
            // wait the feed's next entry started since.
            let waits = owe.sweep != Sweep::Now && !owe.wait_over && self.notify_map.library_settling(sid);
            if !self.watchlist.is_ready() || waits || !self.notify_map.library_due(sid, kind) {
                kept.push(((sid, kind), owe));
                continue;
            }
            let mut asks: Vec<(String, WatchKind)> = owe.gone.iter().map(|g| (g.clone(), kind)).collect();
            if owe.sweep != Sweep::No {
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

    /// End the waits whose feed has gone quiet (or whose oldest entry has waited long enough): each owes
    /// its server one quiet Home refetch, one section-shelf invalidate and one re-list, spent by the
    /// pass that follows. Logs one line per wait ended, never one per frame it waited.
    fn release_settled_library(&mut self) {
        for (sid, wait) in self.notify_map.take_settled() {
            plx_base::eventlog::log(&format!(
                "notify: library settled sid={} entries={} waited={}s",
                sid.raw(),
                wait.entries,
                self.notify_map.frame.saturating_sub(wait.first) / FRAMES_PER_S
            ));
            self.notify_map.library_wait_over(sid);
            let owed = &mut self.notify_map.owed;
            owed.refetch(sid);
            owed.invalidate(sid);
            push_unique(&mut owed.listings, sid);
        }
    }

    fn spend_notify_owed(&mut self, shown: Shown, directory: browse::DirectoryView<'_>,
        clients: &plx_plex::plex::ClientKeys) -> Spent {
        let mut spent = Spent::default();
        if self.notify_map.owed_is_empty() || !self.reread_allowed() {
            return spent;
        }
        self.release_settled_library();
        self.spend_library_owed();
        // What waits (for a feed to go quiet, for a spacing) is not spent, and is not logged every
        // frame it waits.
        let owed = std::mem::take(&mut self.notify_map.owed);
        if owed.is_empty() {
            return spent;
        }
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
            // a Hubs step settles the launches the runner refused since the last one: the refreshes
            // it owes leave with the pass
            spent.endpoints.merge(self.hubs.run_with_directory(super::hubs::HubsCmd::RefetchSourceQuiet { sid }, directory, clients).endpoints);
            spent.refetched.push(sid);
        }
        for sid in owed.listings {
            let cmd = browse::BrowseCmd::ListingsChanged { sid, library_on_stack: shown.library };
            let _ = self.browse.borrow_mut().run(cmd, clients);
            spent.relisted.push(sid);
        }
        for sid in owed.invalidate {
            let _ = self.browse.borrow_mut().run(browse::BrowseCmd::HubsInvalidateServer { sid }, clients);
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
        all.extend(self.notify_map.settling.iter().map(|(s, _)| (*s, "settling".into())));
        all
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn notify_tracked_for_test(&self) -> usize {
        self.notify_map.tracked()
    }
}
