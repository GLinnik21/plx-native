//! **One library's own shelves** — the `Library Recommended` rows its server's owner arranged,
//! quoted rather than composed.
//!
//! `GET /hubs/sections/{id}` ([`plx_plex::plex::Client::library_hubs`], verified live 2026-09-05;
//! `docs/pms-api.md` §3a is the record and five of its findings contradict the OpenAPI spec). The
//! Library screen draws these above its A–Z grid, in one continuous scroll.
//!
//! ## Why this is a FIELD on `SecState` and not a store of its own
//!
//! The obvious shape was a sibling of [`crate::pms`] keyed by `(ServerId, section_key)` with its
//! own LRU. [`super::SecState`] is *already* the per-library-section aggregate — it owns that
//! section's query, verdict, items, focus and scroll — so a second store meant a second section
//! identity, a second generation gate, a second mailbox and a second retry policy for one library,
//! plus a cache that would **survive a Plex Home profile switch** and show the next profile the
//! previous one's resume and watched state. [`super::reset`] already solves that, and `EPOCH`
//! already gates landings. One identity, not two that must agree.
//!
//! ## The publication machine, and why it is not just "commit when terminal"
//!
//! The grid and the hubs land independently, and **the shelf count sets the grid's absolute
//! offset** — a late landing of twelve shelves would move a focused grid row by 6,588px, and a
//! section has no shelf cap to keep that small (issue #412), and still has none: every hub keeps a descriptor. "Commit when the
//! fetch is terminal" does not work, because [`crate::pms::backoff_secs`] is an INFINITE ladder
//! (2/4/8/16/30s and then 30s forever): terminal never arrives on a dead server, and an ordinary
//! fail-then-succeed would still shift the grid under a reader.
//!
//! So there are **two independent axes**, which is the thing to hold on to here. A *publication*
//! state ([`Publication`]) and a *fetch* state (idle / in flight / backing off) running underneath
//! it: zero shelves can be `Committed` while the ladder is still retrying, and a refresh can be in
//! flight over a `Committed` or `Staged` set.
//!
//! | [`Publication`] | Meaning | Effect on layout |
//! |---|---|---|
//! | `Fetching` | nothing published yet; the first attempt is out, inside a bounded first-paint window | not committed; the grid draws where it already is |
//! | `Staged` | a landing exists but publishing it would move a grid the user is looking at | held; nothing moves |
//! | `Committed` | the shelf set the current layout was built from | the document |
//!
//! The **first-paint window is a separate policy from the retry ladder** — when it expires the
//! section commits ZERO shelves and the page is usable, while the ladder keeps retrying underneath
//! and any later success arrives as `Staged`. A newer success replaces an older staged set; a
//! FAILURE preserves both the staged success and the last committed set, because a transient
//! failure must never be able to subtract shelves.
//!
//! That last rule is not an edge case here, it is the ORDINARY refresh path: §3a measured two of
//! these hubs changing identity between requests — the genre and actor/director shelves rotate
//! their subject on every call and sometimes answer empty — so a plain refetch legitimately returns
//! a different shelf count and different ids with nothing changed on the server.
//!
//! ## Who drives it
//!
//! [`kick`] is what arms a section and asks for its first read; every read, whichever step asked
//! (`kick`, a page ask, an invalidation, the tick), is decided in one place, `hubs_pump_spawns`,
//! and **leaves the store as a [`crate::stores::StoreJob::BrowseHubs`] on its outbox**: nothing here
//! starts a thread or reads the registry. The application's runner starts the job
//! ([`finish_browse_hubs_job`] is its IO body) after checking that the client the step decided on
//! is still the registry's, and the answer comes back to the pump as one addressed landing, or as
//! a refusal the store's next step settles. The owned Library emits `PublishShelves`;
//! the application combines its visibility flags with live press state (a press holds only
//! committed shelves; a page's first publication is never held by one) and delivers addressed
//! `LibraryWork::Hubs` to `stores::browse`, which calls `kick` and gates staged publication.
//! It was dormant for one landing — deliberately, since a fetch is
//! production network, task and retry behaviour and "fetched but not drawn" would not have been
//! dormancy at all — and the screen that draws the shelves is what woke it.
#![allow(dead_code)] // the accessors are Landing 3's; see the Dormant note above

use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
use plx_plex::plex::{Client, ClientKey, ClientKeys, ServerId};
use crate::pms::{clamp_hold, PmsMovie, HOLD_ROWS, MAX_SHELF_ITEMS};
use crate::stores::paging::{self, Ask, PageInfo, Row, RowMode, RowState};
use crate::stores::StoreJob;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// Items asked for per hub. The server defaults to 6 and honours at least 24 (§3a); it is
/// items-per-hub and never changes how many hubs come back.
const HUB_FETCH_COUNT: i64 = 12;

/// Frames the page waits for the FIRST answer before committing zero shelves and becoming usable
/// — ~4 s at 60 fps. Deliberately not derived from the retry ladder (module doc), and counted in
/// FRAMES because [`super::pump`] is a per-frame call with no `dt`, exactly as its own `RETRY_CD`
/// is. The one conversion in this module is [`backoff_frames`].
const FIRST_PAINT_FRAMES: u32 = 240;

/// Seconds to this module's unit (frames at 60 fps; the same truncating conversion everywhere).
const fn secs_to_frames(secs: f32) -> u32 {
    (secs * 60.0) as u32
}

/// The least spacing between two notification-driven reads of one section: the quiet Home
/// refetch's ([`crate::pms::QUIET_SPACING_S`]), in frames.
const NOTIFY_SPACING_FRAMES: u32 = secs_to_frames(crate::pms::QUIET_SPACING_S);

/// [`crate::pms::backoff_secs`]' 2/4/8/16/30s ladder in this module's unit. The ladder is shared —
/// hoisted as a PURE function only, deliberately: `pms`'s `landed_ok`/`landed_fail` beside it carry
/// Home-specific logging, stale-build retention and endpoint-refresh policy that mean nothing here.
fn backoff_frames(fails: u32) -> u32 {
    secs_to_frames(crate::pms::backoff_secs(fails))
}

/// One published shelf: the owner's own row, with its items already filtered and capped.
#[derive(Clone, Default)]
pub struct Shelf {
    /// `hubIdentifier`. **Not a stable key across refetches** for the genre and actor/director
    /// rows, which rotate their subject on every call (§3a) — anything remembering a position by
    /// it owes a fallback for an id that is simply not there any more.
    pub id: String,
    /// The hub's provider listing `key`, verbatim. A promoted `custom.collection.*` shelf's key is
    /// `/library/collections/{rk}/children` — the collection its linked heading opens.
    pub key: String,
    /// Where a promoted `custom.collection.*` shelf's linked heading leads, classified once at
    /// parse ([`plx_plex::plex::collections::promoted_collection_link`]); `None` for every other hub.
    pub link: Option<plx_plex::plex::collections::CollectionRef>,
    /// Every item the hub's listing holds (`plex::Hub::total`), which `items` caps — a linked
    /// collection heading's "· N". 0 when the server named no total.
    pub total: usize,
    pub title: String,
    /// A Continue Watching row scoped to this section. **Not `home.continue`**, which is
    /// `pms`'s whole-server id and does not appear here — see [`shelf_is_continue`].
    pub is_continue: bool,
    /// **This row is EPISODES, so it is drawn as landscape stills** rather than portrait posters —
    /// see [`is_episode_shelf`] for why that is decided from the items and not from the hub.
    pub landscape: bool,
    /// The window of the hub's listing the row holds: at most [`MAX_SHELF_ITEMS`] cards. Empty for a
    /// row the ring has given up (`shown > 0`), which the page draws as that many placeholders.
    pub items: Vec<PmsMovie>,
    /// Where each item sits in the hub's listing (see [`paging`]).
    pub positions: Vec<usize>,
    /// The listing positions the window covers, and whether more lies beyond its end.
    pub offset: usize,
    pub end: usize,
    pub more: bool,
    /// Cards the row showed when the ring took them; 0 while it holds cards.
    pub shown: usize,
    /// How the window relates to its listing, and the ledger when the listing would not hold still.
    pub row: RowState,
    /// How many page reads have landed on this row, one that moved nothing included
    /// (`plx_ui::cards::CardSource::page_epoch`). Not recorded.
    pub epoch: u32,
}

impl Shelf {
    fn is(&self, id: &str, key: &str) -> bool { self.id == id && self.key == key }

    /// A descriptor: the row gave its cards up to the ring and keeps what is needed to read them again.
    pub fn released(&self) -> bool { self.items.is_empty() && self.shown > 0 }

    /// Cards the page lays out for the row: the window, or the placeholders of a released row.
    pub fn slots(&self) -> usize { if self.released() { self.shown } else { self.items.len() } }

    fn window(&self) -> (Vec<Row>, PageInfo) {
        let rows = self.positions.iter().copied().zip(self.items.iter().map(|item| Arc::new(item.clone()))).collect();
        (rows, PageInfo { offset: self.offset, end: self.end, total: self.total, more: self.more, unstable: false })
    }
}

/// Where a section's shelves are in the publication axis. The fetch axis is separate and runs
/// underneath this; see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publication {
    Fetching,
    Staged,
    Committed,
}

/// The library section whose publication a retained snapshot belongs to. A section-table index is
/// not an identity: [`super::reset`] reuses it for the next profile, and two servers may both have
/// section `1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HubsId {
    pub epoch: u32,
    pub sid: ServerId,
    pub section: i64,
}

/// One retained immutable publication. Cloning this copies one [`Arc`], never a shelf, item or
/// string; later commits and optimistic edits detach their writable backing as needed.
#[derive(Clone)]
pub struct HubsSnapshot {
    data: Option<Arc<Vec<Shelf>>>,
    id: Option<HubsId>,
    publication: Publication,
    revision: Option<u64>,
}

impl HubsSnapshot {
    pub fn empty() -> Self {
        Self {
            data: None,
            id: None,
            publication: Publication::Fetching,
            revision: None,
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn empty_for_test() -> Self {
        Self::empty()
    }

    pub fn view(&self) -> HubsView<'_> {
        HubsView {
            shelves: self.data.as_deref().map(Vec::as_slice).unwrap_or(&[]),
            id: self.id,
            publication: self.publication,
            revision: self.revision,
        }
    }
}

/// A frame-borrowed read of one retained publication. Absence is explicit in [`HubsView::id`];
/// the empty slice then means no data, not a fabricated section with a static identity.
#[derive(Clone, Copy)]
pub struct HubsView<'a> {
    shelves: &'a [Shelf],
    id: Option<HubsId>,
    publication: Publication,
    revision: Option<u64>,
}

impl<'a> HubsView<'a> {
    pub fn shelves(self) -> &'a [Shelf] {
        self.shelves
    }

    pub fn publication(self) -> Publication {
        self.publication
    }

    pub fn id(self) -> Option<HubsId> {
        self.id
    }

    /// Revision of the committed contents, scoped by `id()`. Unlike an allocation address this
    /// changes for in-place edits too. Staged-only changes do not invalidate a visible layout.
    pub fn revision(self) -> Option<u64> {
        self.revision
    }
}

/// One section's shelves and the two axes' state. A field on [`super::SecState`].
#[derive(Clone, Default)]
pub struct SecHubs {
    /// The set the current layout was built from. Empty and `committed == false` is "nothing
    /// published yet"; empty and `committed == true` is the answer "this library has no shelves",
    /// which is a real state the design draws as "this screen with a shorter top".
    pub(super) committed: Arc<Vec<Shelf>>,
    pub(super) revision: u64,
    /// Has anything ever been published? Distinct from `committed.is_empty()` for that reason.
    published: bool,
    /// A landing held back because publishing it would move a grid the user is looking at.
    staged: Option<Arc<Vec<Shelf>>>,
    /// Frames left in the first-paint window. Only counts down while nothing is published.
    first_paint_left: u32,
    /// Has [`kick`] ever been called for this section? Nothing here ticks or fetches until it has.
    armed: bool,
    /// **A fetch is WANTED and has not been claimed yet.** The per-section half of the
    /// single-flight, and it exists because the claim is shared by all sections in one Browse
    /// adapter (one mailbox and flag per store, not one pair per section).
    ///
    /// Without it a refresh is silently lost, which is what a Codex review found on 2026-09-05:
    /// [`invalidate`] called [`spawn`] once, [`spawn`] returned early because another section held
    /// the flag, and nothing recorded that this section still wanted one — after which [`kick`]
    /// also returns early, since the section IS published and has no failures. The comment
    /// promising "kick retries by itself" was true only of the spawn-refused path and only because
    /// `fails` happened to be non-zero there.
    ///
    /// It is cleared when the step emits the read (`hubs_spawn`). A read the runner refuses is put
    /// back by `hubs_settle` at the head of the next step, so `!owed` means a read was sent, not
    /// that a worker is running.
    owed: bool,
    /// Consecutive failures, for [`crate::pms::backoff_secs`]' ladder.
    fails: u32,
    /// Frames left before another attempt may spawn.
    retry_left: u32,
    /// Frames before a server NOTIFICATION may ask for another read of this section, and whether
    /// one was asked for inside that spacing ([`Self::invalidate_notified`]).
    notified_wait: u32,
    notified_owed: bool,
    /// The rows the page holds cards for (`LibraryWork::HubHold`); `None` until it says, which is
    /// the head of the page.
    hold: Option<(usize, usize)>,
    /// A page of one row the user asked for, waiting for the claim.
    ask: Option<RowAsk>,
    /// Rows whose re-read came back empty while held; asked again only after the hold moves.
    stalled: Vec<(String, String)>,
    /// The row reads' own backoff, apart from the list's: a row that will not read must not delay
    /// the list's refresh, nor the other way round.
    ask_fails: u32,
    ask_retry_left: u32,
    /// The row read a worker is out on, and whether the page withdrew it since: a withdrawn read is
    /// discarded when it lands, so its slide never reaches a window the reader has left.
    inflight: Option<(RowAsk, bool)>,
}

/// One read of a row's listing: a page the user walked to, or the row's window re-read because it
/// came back into the held range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RowAsk {
    id: String,
    key: String,
    before: bool,
    reload: bool,
}

/// The worker-visible half of the section hubs: the ONE claim every section's read shares, the
/// mailbox its answer goes to, and the refusal of a read that was never started
/// ([`crate::stores::Fetch`]). The claim is raised by the step that emits the read
/// ([`super::BrowseState::hubs_pump_spawns`]) and lowered by the take of its answer or by its
/// refusal.
#[derive(Default)]
pub(super) struct HubAdapter {
    pub(super) fetch: crate::stores::Fetch<HubResult>,
}

impl SecHubs {
    pub(super) fn needs_tick(&self) -> bool {
        self.armed && (self.retry_left > 0 || self.notified_wait > 0 || self.notified_owed || (!self.published && self.first_paint_left > 0)
            || (self.owed && self.retry_left == 0) || self.ask_retry_left > 0 || self.row_wanted())
    }

    /// The range of rows the page holds cards for, as the store keeps it.
    fn held(&self) -> (usize, usize) {
        clamp_hold(self.hold.unwrap_or((0, HOLD_ROWS - 1)))
    }

    /// The items of the rows inside the hold window, in row order: what the page holds cards for.
    pub(super) fn held_items(&self) -> impl Iterator<Item = &PmsMovie> {
        let (lo, hi) = self.held();
        self.committed.iter().skip(lo).take(hi.saturating_add(1).saturating_sub(lo)).flat_map(|shelf| shelf.items.iter())
    }

    /// Whether a row read is due: a page the user asked for, or a held row without its cards.
    fn row_wanted(&self) -> bool {
        self.published && (self.ask.is_some() || self.wanted_reload().is_some())
    }

    /// The first held row that is a descriptor and has not stalled: its window is read again.
    fn wanted_reload(&self) -> Option<RowAsk> {
        let (lo, hi) = self.held();
        self.committed.iter().enumerate().skip(lo).take(hi.saturating_add(1).saturating_sub(lo))
            .find(|(_, shelf)| shelf.released() && !self.stalled.iter().any(|(id, key)| shelf.is(id, key)))
            .map(|(_, shelf)| RowAsk { id: shelf.id.clone(), key: shelf.key.clone(), before: false, reload: true })
    }

    /// What the next row read is: the user's page before any re-read.
    fn next_ask(&self) -> Option<RowAsk> {
        if self.ask_retry_left > 0 { return None; }
        self.ask.clone().or_else(|| self.wanted_reload())
    }

    /// The page asks to move row `id`'s window; refused when the row cannot move that way.
    fn want_page(&mut self, seen: u64, id: &str, key: &str, before: bool) -> bool {
        // The asker read a window that a landing has since replaced: where its focus stood in the
        // old one says nothing about the new one. The landing's publication reaches the page, which
        // re-arms every row it holds (the rows the publication changed and the ones it left as they
        // stood), so a row whose edge is still wanted asks again from the window that stands.
        if seen != self.revision { return false; }
        let Some(shelf) = self.committed.iter().find(|shelf| shelf.is(id, key)) else { return false };
        if !plx_plex::plex::is_pageable_hub_key(key) || shelf.released()
            || (before && shelf.offset == 0) || (!before && !shelf.more) { return false; }
        let ask = RowAsk { id: id.into(), key: key.into(), before, reload: false };
        if self.ask.as_ref() == Some(&ask) { return false; }
        self.ask = Some(ask);
        self.ask_fails = 0;
        self.ask_retry_left = 0;
        true
    }

    /// The page withdraws its ask for row `id`: focus left the edge it was sent from. The ask not yet
    /// claimed is dropped; one a worker is out on is marked, and its landing discarded.
    fn cancel_page(&mut self, id: &str, key: &str) {
        if self.ask.as_ref().is_some_and(|ask| !ask.reload && ask.id == id && ask.key == key) { self.ask = None; }
        if let Some((ask, withdrawn)) = self.inflight.as_mut() {
            if !ask.reload && ask.id == id && ask.key == key { *withdrawn = true; }
        }
    }

    /// The page holds rows `lo..=hi`: rows that left the range give their cards up, rows that
    /// entered it are read again. Returns whether anything the page can see changed.
    fn set_hold(&mut self, lo: usize, hi: usize) -> bool {
        if self.hold == Some((lo, hi)) { return false; }
        self.hold = Some((lo, hi));
        self.stalled.clear();
        let range = self.held();
        let outside = |shelves: &[Shelf]| shelves.iter().enumerate().any(|(row, shelf)|
            !(range.0..=range.1).contains(&row) && !shelf.released());
        if !outside(&self.committed) { return false; }
        settle(Arc::make_mut(&mut self.committed).as_mut_slice(), range);
        self.revised();
        true
    }

    /// A row read has landed. `None` is a failed read. A page replaces the row's window (a page that
    /// brought nothing new ends the row there, unless the ledger's walk advanced, which is kept
    /// with the row left open); a re-read replaces the placeholders and restores
    /// the card count. Applied to the committed rows in place: the rows keep their number and their
    /// height, so nothing the page is looking at moves.
    fn land_row(&mut self, ask: &RowAsk, read: Option<Shelf>) -> bool {
        if self.inflight.take().is_some_and(|(out, withdrawn)| withdrawn && out == *ask) { return false; }
        // A failed page stays asked for: the backoff below re-spawns it. While focus stays on the
        // edge the shelf repeats the ask on its ladder (the same ask, which the store takes
        // idempotently), and `cancel_page` drops it when focus leaves, so keeping it cannot read a
        // page nobody waits for.
        let Some(next) = read else {
            self.ask_fails = self.ask_fails.saturating_add(1);
            self.ask_retry_left = backoff_frames(self.ask_fails);
            return false;
        };
        if !ask.reload && self.ask.as_ref() == Some(ask) { self.ask = None; }
        self.ask_fails = 0;
        let Some(at) = self.committed.iter().position(|shelf| shelf.is(&ask.id, &ask.key)) else { return false };
        let current = &self.committed[at];
        if ask.reload {
            if !current.released() { return false; }
            if next.items.is_empty() {
                self.stalled.push((ask.id.clone(), ask.key.clone()));
                return false;
            }
        } else if next.items.is_empty() || (next.more && next.offset == current.offset && next.end <= current.end) {
            let shelf = &mut Arc::make_mut(&mut self.committed)[at];
            shelf.epoch = shelf.epoch.wrapping_add(1);
            // An ask that moved nothing but advanced the ledger's walk of the listing (every key it
            // read was already known) keeps that walk and the row open; the row ends when the ledger
            // says the listing ended. Any other row that moved nothing has nothing more to read.
            if !next.items.is_empty() && next.row != shelf.row { shelf.row = next.row; shelf.total = next.total; }
            else { shelf.more = false; }
            self.revised();
            return true;
        }
        let shelf = &mut Arc::make_mut(&mut self.committed)[at];
        let mut next = next;
        next.items.truncate(MAX_SHELF_ITEMS);
        next.positions.truncate(MAX_SHELF_ITEMS);
        shelf.items = next.items;
        shelf.positions = next.positions;
        shelf.offset = next.offset;
        shelf.end = next.end;
        shelf.more = next.more;
        shelf.total = next.total;
        shelf.row = next.row;
        shelf.shown = 0;
        shelf.epoch = shelf.epoch.wrapping_add(1);
        self.revised();
        true
    }
    fn revised(&mut self) {
        self.revision = self
            .revision
            .checked_add(1)
            .expect("section shelf revision exhausted");
    }

    /// The published shelves — what a layout may be built from, and the only set a screen draws.
    pub fn shelves(&self) -> &[Shelf] {
        self.committed.as_slice()
    }

    pub fn publication(&self) -> Publication {
        // **`Staged` outranks `Fetching`**, which is the ordering the first-paint commit made
        // reachable: before it, an answer held back on the page's FIRST landing had nowhere to be
        // reported but `Fetching`, i.e. "nothing has arrived", of a set that had.
        if self.staged.is_some() {
            Publication::Staged
        } else if !self.published {
            Publication::Fetching
        } else {
            Publication::Committed
        }
    }

    /// Advance both axes by one frame. Returns whether anything a screen can see changed — today
    /// only the first-paint window expiring, which publishes zero shelves.
    fn tick(&mut self) -> bool {
        if !self.armed {
            return false;
        }
        self.retry_left = self.retry_left.saturating_sub(1);
        self.ask_retry_left = self.ask_retry_left.saturating_sub(1);
        self.notified_wait = self.notified_wait.saturating_sub(1);
        if self.notified_owed && self.notified_wait == 0 {
            self.notified_owed = false;
            self.owed = true;
            self.notified_wait = NOTIFY_SPACING_FRAMES;
        }
        if self.published {
            return false;
        }
        self.first_paint_left = self.first_paint_left.saturating_sub(1);
        if self.first_paint_left > 0 {
            return false;
        }
        if self.staged.is_some() {
            // An answer IS in hand and is simply not allowed through yet. The window is not what is
            // holding it, so there is nothing to publish here and nothing to report — the screen's
            // own `commit_staged` is the one that will let it in.
            return false;
        }
        // The window closed with no answer. Publish NOTHING — the page becomes usable at the
        // offset it already has, the ladder keeps retrying underneath, and any later success will
        // arrive as `Staged` rather than moving a grid somebody is reading.
        self.published = true;
        self.revised();
        true
    }

    /// A successful landing. It **always stages**, and a newer success replaces an older staged
    /// set: the freshest answer is the one worth showing when the page finally lets it through.
    ///
    /// **The FIRST landing stages too, and that is a correction (2026-09-05).** It used to commit
    /// itself whenever nothing had been published, on the reasoning that inside the first-paint
    /// window there is no layout yet to protect. That reasoning is about the first FRAMES and the
    /// window is four SECONDS: `/tmp/plxnative-library` boots straight into this screen, two
    /// presses reach the grid, and the shelves then inserted themselves above a grid row somebody
    /// was already looking at — the exact 6,588px jump the rest of this machine exists to prevent,
    /// arriving through the one path that did not ask. Whether the ground may move is the SCREEN's
    /// answer and it is already computed every frame, so the first paint goes through
    /// [`SecHubs::commit_staged`] like every later one. At the head — where a page four seconds old
    /// almost always is — `BrowseStore::pump` lands and the screen commits in the SAME frame, so
    /// nothing waits.
    fn land_ok(&mut self, shelves: Vec<Shelf>) {
        self.fails = 0;
        self.retry_left = 0;
        self.staged = Some(Arc::new(shelves));
    }

    /// **An answer that arrived for a server this slot no longer is.** Not a landing at all: the
    /// response is discarded and the section is still OWED the fetch it asked for.
    ///
    /// It has to say so explicitly, because `hubs_spawn` clears `owed` when it emits the read. A
    /// refused read is put back by `hubs_settle`, but a read that lands for a server the slot no
    /// longer is has to say so itself. `kick` cannot recover it either: it re-arms neither a
    /// published section nor an unpublished one with `fails == 0`. So without this an ordinary
    /// `sync_roster` re-point or a token refresh left a library's shelves empty, or a refresh
    /// stale, for the rest of the session, silently and with no failure anywhere to read.
    ///
    /// The ladder is deliberately NOT armed: it exists for a server that answered badly, and this
    /// one may never have answered at all.
    fn landing_was_not_ours(&mut self) {
        if self.armed {
            self.owed = true;
        }
    }

    /// A server's NOTIFICATION says this section may have moved (a stranger's playback stopped).
    /// The read is owed, but it is a hint and not a user's press: it never forgives a failing
    /// server (`fails` and `retry_left` stay), and reads of one section are spaced
    /// ([`NOTIFY_SPACING_FRAMES`], the quiet Home refetch's spacing), a burst inside it becoming ONE
    /// read when it is over.
    fn invalidate_notified(&mut self) {
        if !self.armed {
            return;
        }
        if self.notified_wait > 0 {
            self.notified_owed = true;
        } else {
            self.owed = true;
            self.notified_wait = NOTIFY_SPACING_FRAMES;
        }
    }

    /// A failed landing. It **preserves both** the staged success and the last committed set: a
    /// transient failure must never be able to subtract shelves, which is `pms::Src::last`'s rule
    /// at the same join. It arms the backoff and nothing else.
    fn land_fail(&mut self) {
        self.fails = self.fails.saturating_add(1);
        self.retry_left = backoff_frames(self.fails);
    }

    /// Publish a staged set. `may_move` is the caller's answer to "can the user see the ground
    /// this would move" — `true` at a route or section transition floor (the page is not on screen,
    /// so nothing can be SEEN to move) and, on a visible page, only once a BACK-driven head
    /// transition has settled. Returns whether anything was published, so the caller knows to
    /// re-resolve focus against the POST-commit layout.
    pub(super) fn commit_staged(&mut self, may_move: bool) -> bool {
        if !may_move {
            return false;
        }
        match self.staged.take() {
            Some(mut s) => {
                // A refresh brings every row's preview. A row the reader had walked, or whose cards
                // the ring had taken, keeps its window and descriptor and takes only the fresh total,
                // so it comes back where it was.
                carry_windows(&self.committed, Arc::make_mut(&mut s).as_mut_slice());
                settle(Arc::make_mut(&mut s).as_mut_slice(), self.held());
                self.committed = s;
                self.published = true;
                self.revised();
                true
            }
            None => false,
        }
    }
}

/// Arm a section's shelves and let the first fetch happen. Idempotent; call it every frame the
/// screen wants shelves. Nothing is started here: a read that is due leaves as a
/// [`StoreJob::BrowseHubs`] on `jobs` (see `hubs_pump_spawns`).
impl super::BrowseState {
    pub(super) fn hubs_kick(&mut self, sec: usize, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys,
        jobs: &mut Vec<StoreJob>) {
        let Some(st) = self.state_mut(sec) else {
            return;
        };
        if !st.hubs.armed {
            st.hubs.armed = true;
            st.hubs.first_paint_left = FIRST_PAINT_FRAMES;
            st.hubs.owed = true;
        }
        // Failing sections keep asking; a published, healthy one is owed nothing until something
        // invalidates it.
        if st.hubs.fails > 0 && !st.hubs.published {
            st.hubs.owed = true;
        }
        self.hubs_pump_spawns(adapter, keys, jobs);
    }

    /// Emit the one read that is due, if the shared claim is free: the list of a section that is
    /// owed one, else a row read of a published section. Nothing is started here: the read leaves
    /// as a [`StoreJob::BrowseHubs`] on `jobs` and the application's runner starts it, so the
    /// registry is read by the host (`keys`), never by this step.
    fn hubs_pump_spawns(&mut self, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys, jobs: &mut Vec<StoreJob>) {
        if adapter.hubs.fetch.busy() {
            return;
        }
        // A read the runner refused freed the claim in the call that refused it; its section is
        // settled FIRST, so the state below is the one a read that never started leaves.
        self.hubs_settle(adapter);
        let want = self
            .states()
            .iter()
            .position(|st| st.hubs.armed && st.hubs.owed && st.hubs.retry_left == 0);
        if let Some(sec) = want {
            self.hubs_spawn(sec, adapter, keys, jobs);
            return;
        }
        // The one claim every section's read shares is the gate: a row read waits for the list
        // and for any other read, one at a time, whatever the number of rows.
        let row = self.states().iter().position(|st| st.hubs.armed && st.hubs.published && st.hubs.next_ask().is_some());
        if let Some(sec) = row {
            self.hubs_spawn_row(sec, adapter, keys, jobs);
        }
    }

    /// Raise the shared claim for a read of `sec` and record it as the one in flight. The claim is
    /// raised HERE, at the step, because this step is claim-gated and several run before one drain
    /// (a burst of invalidations); the runner's refusal gives it back.
    fn hubs_emit(&mut self, adapter: &Arc<super::BrowseAdapter>, jobs: &mut Vec<StoreJob>, sec: usize,
        sid: ServerId, key: ClientKey, flight: Flight, read: Read) {
        let epoch = self.table_epoch();
        self.hubs_stamp = self.ledger.stamp();
        self.hubs_gen = self.hubs_gen.wrapping_add(1);
        let gen = self.hubs_gen;
        adapter.hubs.fetch.claim(gen);
        self.hubs_flight = Some(HubsFlight { gen, epoch, sec, flight });
        jobs.push(StoreJob::BrowseHubs(
            Job { addr: browse_hubs_addr(gen), req: BrowseHubsReq { sid, key, epoch, sec, read } },
            BrowseHubsPort::of(adapter),
        ));
    }

    /// Settle the read the runner refused since the last step, if one was: the state a read that
    /// never started leaves. A list read is owed again (`hubs_spawn` cleared that when it emitted),
    /// a row read is no longer in flight. A flight with no claim and no refusal belongs to an
    /// adapter that has since been retired, and is dropped. Never recorded anywhere.
    fn hubs_settle(&mut self, adapter: &Arc<super::BrowseAdapter>) {
        let Some(out) = self.hubs_flight.take() else { return };
        if !adapter.hubs.fetch.take_refused(out.gen) || out.epoch != self.table_epoch() {
            return;
        }
        if let Some(st) = self.state_mut(out.sec) {
            match out.flight {
                Flight::List => st.hubs.owed = true,
                Flight::Row => st.hubs.inflight = None,
            }
        }
        plx_machine::idle::invalidate();
    }

    /// Reads one row's window: a page the reader walked to, or the cards of a row that came back
    /// into the held range, at the offset it left.
    fn hubs_spawn_row(&mut self, sec: usize, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys,
        jobs: &mut Vec<StoreJob>) {
        let Some(sid) = self.section_sid(sec) else { return };
        let Some(key) = keys.key(sid) else { return };
        let Some(ask) = self.state_mut(sec).and_then(|st| st.hubs.next_ask()) else { return };
        let window = self.states().get(sec).and_then(|st| st.hubs.committed.iter()
            .find(|shelf| shelf.is(&ask.id, &ask.key)).cloned());
        if let Some(st) = self.state_mut(sec) { st.hubs.inflight = Some((ask.clone(), false)); }
        self.hubs_emit(adapter, jobs, sec, sid, key, Flight::Row, Read::Row { ask, window });
    }

    fn hubs_spawn(&mut self, sec: usize, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys,
        jobs: &mut Vec<StoreJob>) {
        let Some(section) = self.sections().get(sec).map(|s| s.key) else {
            return;
        };
        let Some(sid) = self.section_sid(sec) else {
            return;
        };
        let Some(key) = keys.key(sid) else {
            return;
        };
        let hold = self.states().get(sec).map_or((0, HOLD_ROWS - 1), |st| st.hubs.held());
        if let Some(st) = self.state_mut(sec) {
            st.hubs.owed = false;
        }
        self.hubs_emit(adapter, jobs, sec, sid, key, Flight::List, Read::List { section, hold });
    }

    pub fn hubs_land(&mut self, adapter: &Arc<super::BrowseAdapter>,
        gate: &plx_machine::landgate::Gate, keys: &ClientKeys) -> bool {
        // DUMP MODE: owed while the section-hubs claim is held.
        let taken = crate::stores::take_landing_owed(gate, crate::stores::StoreId::Browse,
            || adapter.hubs.fetch.busy(), || adapter.hubs.fetch.take_current(|mail| mail.gen));
        let Some(result) = taken else {
            return false;
        };
        if self.hubs_flight.as_ref().is_some_and(|out| out.gen == result.gen) {
            self.hubs_flight = None;
        }
        if result.epoch != self.table_epoch() {
            return false;
        }
        // The identity the read was made with, against the registry's answer as of this call: a
        // slot re-pointed or retokened while the read was out is not the server it asked.
        let still_current = self.section_sid(result.sec).and_then(|sid| keys.key(sid)) == Some(result.key);
        if !still_current {
            if let Some(state) = self.state_mut(result.sec) {
                state.hubs.landing_was_not_ours();
            }
            return false;
        }
        let (ledger, stamp) = (self.ledger.clone(), self.hubs_stamp);
        let Some(state) = self.state_mut(result.sec) else {
            return false;
        };
        match result.outcome {
            Outcome::Row { ask, mut shelf } => {
                if let Some(shelf) = shelf.as_mut() { replay_since_request(&ledger, stamp, std::slice::from_mut(shelf)); }
                let failed = shelf.is_none();
                if failed {
                    plx_base::eventlog::log(&format!("libhubs: section {} row {} read failed", result.sec, ask.id));
                }
                let moved = state.hubs.land_row(&ask, shelf);
                plx_machine::idle::invalidate();
                return moved || failed;
            }
            Outcome::List(Some(parsed)) => {
                let mut shelves = parsed.shelves;
                replay_since_request(&ledger, stamp, &mut shelves);
                // The harness (`tests/mock_fps.py`, `LIBHUBS_RE`) reads the section and the shelf
                // count; new fields go AFTER them.
                plx_base::eventlog::log(&format!(
                    "libhubs: section {} landed {} shelves, {} items",
                    result.sec,
                    shelves.len(),
                    shelves.iter().map(|s| s.items.len()).sum::<usize>()
                ));
                state.hubs.land_ok(shelves);
            }
            Outcome::List(None) => {
                plx_base::eventlog::log(&format!(
                    "libhubs: section {} failed ({} in a row)",
                    result.sec,
                    state.hubs.fails + 1
                ));
                state.hubs.land_fail();
                state.hubs.owed = true;
            }
        }
        plx_machine::idle::invalidate();
        true
    }

    fn hubs_invalidate(&mut self, sec: usize, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys,
        jobs: &mut Vec<StoreJob>) {
        if let Some(st) = self.state_mut(sec) {
            if st.hubs.armed {
                st.hubs.fails = 0;
                st.hubs.retry_left = 0;
                st.hubs.owed = true;
            }
        }
        self.hubs_pump_spawns(adapter, keys, jobs);
    }

    pub fn hubs_tick_all(&mut self, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys,
        jobs: &mut Vec<StoreJob>) -> bool {
        let mut moved = false;
        for st in self.states_mut() {
            moved |= st.hubs.tick();
        }
        self.hubs_pump_spawns(adapter, keys, jobs);
        moved
    }

    fn hubs_shelves(&self, sec: usize) -> &[Shelf] {
        self.states()
            .get(sec)
            .map(|s| s.hubs.shelves())
            .unwrap_or(&[])
    }

    fn hubs_publication(&self, sec: usize) -> Publication {
        self.states()
            .get(sec)
            .map(|s| s.hubs.publication())
            .unwrap_or(Publication::Fetching)
    }

    pub fn hubs_snapshot(&self, sec: usize) -> HubsSnapshot {
        let Some(section) = self.sections().get(sec) else {
            return HubsSnapshot::empty();
        };
        let Some(state) = self.states().get(sec) else {
            return HubsSnapshot::empty();
        };
        let Some(source) = self.sources().get(section.src) else {
            return HubsSnapshot::empty();
        };
        HubsSnapshot {
            data: Some(Arc::clone(&state.hubs.committed)),
            id: Some(HubsId {
                epoch: self.table_epoch(),
                sid: source.sid,
                section: section.key,
            }),
            publication: state.hubs.publication(),
            revision: Some(state.hubs.revision),
        }
    }

    /// The page asks to move row `id`'s window. Returns whether a read was queued.
    pub(super) fn hubs_page(&mut self, sec: usize, seen: u64, id: &str, key: &str, before: bool,
        adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys, jobs: &mut Vec<StoreJob>) -> bool {
        let asked = self.state_mut(sec).is_some_and(|st| st.hubs.want_page(seen, id, key, before));
        self.hubs_pump_spawns(adapter, keys, jobs);
        asked
    }

    /// The page withdraws its ask for row `id`. Nothing the page shows changes.
    pub(super) fn hubs_cancel_page(&mut self, sec: usize, id: &str, key: &str,
        adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys, jobs: &mut Vec<StoreJob>) -> bool {
        if let Some(st) = self.state_mut(sec) { st.hubs.cancel_page(id, key); }
        self.hubs_pump_spawns(adapter, keys, jobs);
        false
    }

    /// The page holds rows `lo..=hi`. Returns whether the published rows changed.
    pub(super) fn hubs_hold(&mut self, sec: usize, lo: usize, hi: usize,
        adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys, jobs: &mut Vec<StoreJob>) -> bool {
        let changed = self.state_mut(sec).is_some_and(|st| st.hubs.set_hold(lo, hi));
        self.hubs_pump_spawns(adapter, keys, jobs);
        changed
    }

    pub(super) fn hubs_commit_staged(&mut self, sec: usize, may_move: bool) -> bool {
        self.state_mut(sec)
            .map(|s| s.hubs.commit_staged(may_move))
            .unwrap_or(false)
    }

    pub(super) fn hubs_set_watched_local(&mut self, at: &crate::viewstate::ViewAt, on: bool) -> bool {
        let mut hit = false;
        for st in self.states_mut() {
            if edit_watched(&mut st.hubs.committed, at, on) {
                st.hubs.revised();
                hit = true;
            }
            if let Some(staged) = st.hubs.staged.as_mut() {
                edit_watched(staged, at, on);
            }
        }
        hit
    }

    pub(super) fn hubs_left_the_deck(&mut self, sid: ServerId, rk: &str) -> bool {
        let mut hit = false;
        for st in self.states_mut() {
            if drop_from_deck(&mut st.hubs.committed, sid, rk) {
                st.hubs.revised();
                hit = true;
            }
            if let Some(staged) = st.hubs.staged.as_mut() {
                drop_from_deck(staged, sid, rk);
            }
        }
        hit
    }

    /// A notification from `sid`: its sections' shelves may have moved. No other server's section
    /// is touched and the failure ladder is kept (see [`SecHubs::invalidate_notified`]).
    pub(super) fn hubs_invalidate_server(&mut self, sid: ServerId, adapter: &Arc<super::BrowseAdapter>,
        keys: &ClientKeys, jobs: &mut Vec<StoreJob>) {
        for sec in 0..self.sections().len() {
            if self.section_sid(sec) == Some(sid) {
                if let Some(st) = self.state_mut(sec) {
                    st.hubs.invalidate_notified();
                }
            }
        }
        self.hubs_pump_spawns(adapter, keys, jobs);
    }

    pub(super) fn hubs_invalidate_all(&mut self, adapter: &Arc<super::BrowseAdapter>, keys: &ClientKeys,
        jobs: &mut Vec<StoreJob>) {
        for sec in 0..self.sections().len() {
            self.hubs_invalidate(sec, adapter, keys, jobs);
        }
        self.hubs_pump_spawns(adapter, keys, jobs);
    }
}

/// A refresh's rows, with the windows the old rows had walked to or given up carried over them.
fn carry_windows(old: &[Shelf], new: &mut [Shelf]) {
    for shelf in new {
        let Some(was) = old.iter().find(|was| was.is(&shelf.id, &shelf.key))
            .filter(|was| was.released() || was.offset > 0 || was.end > HUB_FETCH_COUNT as usize
                || was.row != RowState::default() && !matches!(was.row.mode, RowMode::Unprobed)) else { continue };
        let total = shelf.total;
        let (title, link, landscape) = (shelf.title.clone(), shelf.link.clone(), shelf.landscape);
        if was.released() && !plx_plex::plex::is_pageable_hub_key(&shelf.key) {
            // nothing but the refresh's own preview says what the row shows now; the ring gives it up again
            continue;
        }
        *shelf = was.clone();
        (shelf.title, shelf.link, shelf.landscape, shelf.total) = (title, link, landscape, total);
    }
}

/// The read describes the server from when it was requested: the view-state edits published since
/// ([`crate::viewledger`]) are made again on the shelves it brought — a marked card stays marked
/// and a card taken off Continue Watching stays off.
pub(super) fn replay_since_request(ledger: &crate::viewledger::ViewLedger, stamp: crate::viewledger::Stamp,
    shelves: &mut [Shelf]) {
    ledger.patch(stamp, shelves.iter_mut().flat_map(|shelf| shelf.items.iter_mut()));
    for (at, edit) in ledger.since(stamp) {
        if let crate::viewstate::ViewEdit::LeftDeck = edit {
            for shelf in shelves.iter_mut().filter(|shelf| shelf.is_continue) {
                shelf.items.retain(|item| !at.is_item(item.sid, &item.rk));
            }
        }
    }
}

fn edit_watched(shelves: &mut Arc<Vec<Shelf>>, at: &crate::viewstate::ViewAt, on: bool) -> bool {
    if !shelves
        .iter()
        .flat_map(|shelf| shelf.items.iter())
        .any(|item| at.covers(item))
    {
        return false;
    }
    for item in Arc::make_mut(shelves)
        .iter_mut()
        .flat_map(|shelf| shelf.items.iter_mut())
    {
        if at.covers(item) {
            crate::viewstate::ViewEdit::Watched(on).apply(item);
        }
    }
    true
}

fn drop_from_deck(shelves: &mut Arc<Vec<Shelf>>, sid: ServerId, rk: &str) -> bool {
    if !shelves
        .iter()
        .filter(|shelf| shelf.is_continue)
        .flat_map(|shelf| shelf.items.iter())
        .any(|item| plx_plex::plex::same_item((item.sid, &item.rk), (sid, rk)))
    {
        return false;
    }
    let shelves = Arc::make_mut(shelves);
    let mut hit = false;
    for shelf in shelves.iter_mut().filter(|shelf| shelf.is_continue) {
        let before = shelf.items.len();
        shelf
            .items
            .retain(|item| !plx_plex::plex::same_item((item.sid, &item.rk), (sid, rk)));
        hit |= shelf.items.len() != before;
    }
    shelves.retain(|shelf| !shelf.items.is_empty());
    hit
}

/// Publish a section's shelves directly, for the screen tests that need a document with a shelf
/// run above the grid. Not a fetch and not a landing: no worker, no gate, no ladder — the shape
/// only, which is all a geometry or focus-walk assertion is about.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_shelves_for_owner_test(
    state: &mut super::BrowseState,
    sec: usize,
    titles: &[&str],
    per_row: usize,
) {
    let rows: Vec<_> = titles.iter().map(|title| (*title, "", *title)).collect();
    seed_named_shelves_for_owner_test(state, sec, &rows, per_row);
}

/// [`seed_shelves_for_owner_test`] with each shelf's `(hubIdentifier, key, title)` named — a
/// promoted collection shelf needs its identifier and listing key.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_named_shelves_for_owner_test(
    state: &mut super::BrowseState,
    sec: usize,
    rows: &[(&str, &str, &str)],
    per_row: usize,
) {
    let sid = state.section_sid(sec).unwrap_or(ServerId::UNSET);
    let section = state.sections().get(sec).map_or(0, |s| s.key);
    let Some(st) = state.state_mut(sec) else {
        return;
    };
    st.hubs = SecHubs {
        revision: st.hubs.revision,
        ..Default::default()
    };
    st.hubs.armed = true;
    st.hubs.land_ok(
        rows
            .iter()
            .map(|(id, key, title)| Shelf {
                id: (*id).into(),
                key: (*key).into(),
                link: plx_plex::plex::collections::promoted_collection_link(sid, id, key, title, section),
                total: per_row,
                title: (*title).into(),
                is_continue: shelf_is_continue(id, key),
                landscape: false,
                items: (0..per_row)
                    .map(|index| PmsMovie {
                        rk: format!("{id}-{index}"),
                        title: format!("{title} {index}"),
                        ..Default::default()
                    })
                    .collect(),
                positions: (0..per_row).collect(),
                end: per_row,
                ..Default::default()
            })
            .collect(),
    );
    st.hubs.commit_staged(true);
}

/// Edit every card of every published shelf of section `sec` (a seeded card carries only a key and
/// a title): the facts a caption is worded from, or a resume point. The cards are told apart by
/// their `rk`, which [`seed_named_shelves_for_owner_test`] makes `{hubIdentifier}-{index}`.
#[cfg(any(test, feature = "test-support"))]
pub fn edit_shelf_items_for_owner_test(state: &mut super::BrowseState, sec: usize, edit: &dyn Fn(&mut PmsMovie)) {
    let Some(st) = state.state_mut(sec) else { return };
    for shelf in Arc::make_mut(&mut st.hubs.committed).iter_mut() {
        shelf.items.iter_mut().for_each(edit);
    }
    st.hubs.revised();
}

/// The reads a moved hold is owed, landed: every row in the held range that is a descriptor gets
/// back the cards it showed, keyed as [`seed_named_shelves_for_owner_test`] keys them, and the
/// result is published (the revision moves). What the worker does for a ring that moved onto rows
/// whose cards had been given up.
#[cfg(any(test, feature = "test-support"))]
pub fn land_held_rows_for_owner_test(state: &mut super::BrowseState, sec: usize) {
    let Some(st) = state.state_mut(sec) else { return };
    let (lo, hi) = st.hubs.held();
    for shelf in Arc::make_mut(&mut st.hubs.committed).iter_mut().skip(lo).take(hi + 1 - lo).filter(|shelf| shelf.released()) {
        shelf.items = (0..shelf.shown)
            .map(|index| PmsMovie { rk: format!("{}-{index}", shelf.id), title: format!("{} {index}", shelf.title), ..Default::default() })
            .collect();
        shelf.positions = (0..shelf.shown).collect();
        shelf.shown = 0;
    }
    st.hubs.revised();
}

/// The rows of section `sec` that gave their cards up to the ring (descriptors, drawn as placeholders)
/// and the band the store holds (`lo, hi`): what a route test reads to say the rows in view are published.
#[cfg(any(test, feature = "test-support"))]
pub fn released_rows_for_owner_test(state: &super::BrowseState, sec: usize) -> (Vec<usize>, (usize, usize)) {
    let Some(st) = state.states().get(sec) else { return (Vec::new(), (0, 0)) };
    let released = st.hubs.committed.iter().enumerate().filter(|(_, shelf)| shelf.released()).map(|(row, _)| row).collect();
    (released, st.hubs.held())
}

/// A read of row `row` of section `sec` lands through [`SecHubs::land_row`], the real landing: the
/// row's window becomes `count` cards with ratingKeys no earlier window had (`{id}-e{epoch}-{i}`),
/// its epoch moves, and the revision with it. What a page the reader walked to, or a held row read
/// again, does to ONE row of a long page. False when the row cannot take it.
#[cfg(any(test, feature = "test-support"))]
pub fn land_row_for_owner_test(state: &mut super::BrowseState, sec: usize, row: usize, count: usize) -> bool {
    let Some(st) = state.state_mut(sec) else { return false };
    let Some(current) = st.hubs.committed.get(row) else { return false };
    let ask = RowAsk { id: current.id.clone(), key: current.key.clone(), before: false, reload: current.released() };
    let epoch = current.epoch;
    let next = Shelf {
        id: current.id.clone(), key: current.key.clone(), title: current.title.clone(), landscape: current.landscape,
        items: (0..count).map(|index| PmsMovie { rk: format!("{}-e{epoch}-{index}", current.id), ..Default::default() }).collect(),
        positions: (0..count).collect(), end: count, total: count,
        ..Default::default()
    };
    st.hubs.land_row(&ask, Some(next))
}

/// Make a seeded section's pageable rows the first window of a listing of `total` cards, as a hub
/// the server pages: the rows have more, and a page ask is worth sending.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_paging_for_owner_test(state: &mut super::BrowseState, sec: usize, total: usize) {
    let Some(st) = state.state_mut(sec) else { return };
    for shelf in Arc::make_mut(&mut st.hubs.committed).iter_mut()
        .filter(|shelf| plx_plex::plex::is_pageable_hub_key(&shelf.key)) {
        shelf.total = total;
        shelf.more = true;
        shelf.end = shelf.items.len();
        shelf.positions = (0..shelf.items.len()).collect();
    }
    st.hubs.revised();
}

/// Replace the items of section `sec`'s first published shelf with one card per `rks` entry, in
/// that order, and publish the result (the revision moves). The content landing a hub shelf has on
/// the set (a refetch that reorders, inserts or drops a title) without a worker: the card's rating
/// key is the entry, so a test names which title moved.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_first_shelf_items_for_owner_test(
    state: &mut super::BrowseState,
    sec: usize,
    rks: &[String],
) {
    let Some(st) = state.state_mut(sec) else {
        return;
    };
    let mut shelves = st.hubs.committed.as_ref().clone();
    if let Some(first) = shelves.first_mut() {
        first.total = rks.len();
        first.items = rks
            .iter()
            .map(|rk| PmsMovie {
                rk: rk.clone(),
                title: rk.clone(),
                ..Default::default()
            })
            .collect();
    }
    st.hubs.land_ok(shelves);
    st.hubs.commit_staged(true);
}

/// A worker success held behind the Library's publication gate, without starting a worker.
#[cfg(any(test, feature = "test-support"))]
pub fn stage_shelves_for_owner_test(state: &mut super::BrowseState, sec: usize) {
    if let Some(st) = state.state_mut(sec) {
        st.hubs.land_ok(vec![Shelf { title: "staged".into(), ..Default::default() }]);
    }
}

/// Turn a seeded section's shelves into EPISODE shelves — landscape rows of `kind == 3` items with
/// a show behind them. The half [`seed_shelves_for_test`] deliberately does not do, because a
/// geometry test wants the poster shape it can name; a test about the episode TILE wants this.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_landscape_for_owner_test(
    state: &mut super::BrowseState,
    sec: usize,
    show: &str,
) {
    let Some(st) = state.state_mut(sec) else {
        return;
    };
    for shelf in Arc::make_mut(&mut st.hubs.committed).iter_mut() {
        shelf.landscape = true;
        for item in &mut shelf.items {
            item.kind = 3;
            item.show_title = show.into();
            item.season_index = 1;
            item.ep_index = 1;
        }
    }
    st.hubs.revised();
}

/// **Every published shelf is a store that can be DRAWING the item** — so it takes the same
/// optimistic edit `pms`, `metadata`, `browse`, `search` and `person` already take, and for the same
/// reason: the press writes correctly to the server and changes nothing on the screen the user is
/// looking at until a refetch, which reads as the row having done nothing.
///
/// This was the sixth store and it was not on the list; `viewstate::edit_local`'s five calls
/// predate the Library growing shelves of its own.
// ---- the fetch ------------------------------------------------------------------------------

/// One section's shelves, in flight. Gated on the client lifecycle for the reason
/// [`super::DirectoryResult`] is (`EPOCH` does not move when a slot is re-pointed or retokened),
/// but by `ClientKey` rather than a `&'static Client` and a `token_gen`.
pub(super) struct HubResult {
    /// The request id of the job this answers ([`browse_hubs_addr`]), so a superseded read's late
    /// answer is dropped and never releases a newer read's claim.
    gen: u32,
    epoch: u32,
    sec: usize,
    /// The client the step decided on; the landing is ours only while the registry still holds it.
    key: ClientKey,
    outcome: Outcome,
}

/// What a worker brought back. `None` inside is a FAILED fetch — kept distinguishable from a
/// successful answer that happens to be empty, which on this endpoint is a common and legitimate reply.
pub(super) enum Outcome {
    /// The section's whole hub list.
    List(Option<Parsed>),
    /// One row's window, moved by a page or re-read after the ring gave its cards up.
    Row { ask: RowAsk, shelf: Option<Shelf> },
}

/// Which read a step emitted, and so which state its refusal puts back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Flight {
    /// A section's whole hub list: its `owed` was cleared when the job left.
    List,
    /// One row's window: the row's `inflight` was set when the job left.
    Row,
}

/// The read the step has out: recorded by the step that emitted it and read by the next step,
/// which settles it if the runner refused it ([`super::BrowseState::hubs_settle`]) and drops it when
/// it lands. The record is the store's own memory of what it asked and is never recorded or replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HubsFlight {
    gen: u32,
    epoch: u32,
    sec: usize,
    flight: Flight,
}

/// What a [`BrowseHubsReq`] reads, captured whole by the step: a worker never reads the machine.
enum Read {
    /// The section's whole hub list, with the rows the page holds cards for.
    List { section: i64, hold: (usize, usize) },
    /// One row's window (`window` is the row as it stood when the step decided), moved by `ask`.
    Row { ask: RowAsk, window: Option<Shelf> },
}

impl PartialEq for Read {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Read::List { section: a, hold: b }, Read::List { section: c, hold: d }) => a == c && b == d,
            (Read::Row { ask: a, window: w }, Read::Row { ask: b, window: v }) => a == b && match (w, v) {
                (None, None) => true,
                (Some(w), Some(v)) => w.is(&v.id, &v.key) && (w.offset, w.end, w.items.len()) == (v.offset, v.end, v.items.len()),
                _ => false,
            },
            _ => false,
        }
    }
}

impl std::fmt::Debug for Read {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Read::List { section, hold } => write!(f, "List(section {section}, hold {hold:?})"),
            Read::Row { ask, window } => write!(f, "Row({ask:?}, window {:?})",
                window.as_ref().map(|w| (&w.id, &w.key, w.offset, w.end, w.items.len()))),
        }
    }
}

/// The one request a [`StoreJob::BrowseHubs`] carries: everything the worker needs, captured by the
/// step. `key` is the client the step decided on; the runner starts the request only if the registry
/// still holds that client at the launch, and the landing is ours only while it still does.
pub struct BrowseHubsReq {
    sid: ServerId,
    key: ClientKey,
    epoch: u32,
    sec: usize,
    read: Read,
}

impl BrowseHubsReq {
    /// The server to ask and the client the step decided to ask it through.
    pub fn server(&self) -> (ServerId, ClientKey) { (self.sid, self.key) }
}

impl PartialEq for BrowseHubsReq {
    fn eq(&self, other: &Self) -> bool {
        (self.sid, self.key, self.epoch, self.sec) == (other.sid, other.key, other.epoch, other.sec)
            && self.read == other.read
    }
}

impl std::fmt::Debug for BrowseHubsReq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrowseHubsReq({:?}, {:?}, epoch {}, section {}, {:?})", self.sid, self.key, self.epoch,
            self.sec, self.read)
    }
}

/// A request's answer, opaque to the runner: the result the step lands. The runner never names
/// [`HubResult`].
pub struct BrowseHubsAnswer(HubResult);

/// Where a request's ONE answer goes: the store's mailbox, captured with the job when it was
/// emitted. A store that has since been `Reset` holds a new adapter, and this port still reaches
/// the one the request was made from, the retired one nobody pumps. Two ports are equal when they
/// reach the same adapter.
#[derive(Clone)]
pub struct BrowseHubsPort(Arc<super::BrowseAdapter>);

impl BrowseHubsPort {
    pub(super) fn of(adapter: &Arc<super::BrowseAdapter>) -> Self { Self(Arc::clone(adapter)) }
}

impl PartialEq for BrowseHubsPort {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}

impl std::fmt::Debug for BrowseHubsPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BrowseHubsPort({:p})", Arc::as_ptr(&self.0))
    }
}

impl Port<BrowseHubsAnswer> for BrowseHubsPort {
    /// The request's answer (a panicked read answers a failure of its kind), stamped with the
    /// request's id so the pump drops the answer of a read it has since superseded.
    fn done(&self, addr: Addr, out: BrowseHubsAnswer) {
        let gen = addr.req.0;
        self.0.hubs.fetch.post(out.0, |old| old.gen < gen);
    }

    /// The request was never started, so nothing will land and nothing will release its claim:
    /// hand the claim back and remember the refusal ([`crate::stores::Fetch::refuse_for`]); the
    /// store settles it at the head of its next step. A refusal that comes after a newer request has
    /// claimed leaves that claim alone.
    fn refused(&self, addr: Addr) {
        self.0.hubs.fetch.refuse_for(addr.req.0);
    }
}

/// A request's address: the store and the id its step gave it. There is one read in flight across
/// every section (the shared claim), so no two are at one address.
fn browse_hubs_addr(generation: u32) -> Addr {
    Addr { to: MachineId::Store(crate::stores::StoreId::Browse.ord()), req: RequestId(generation) }
}

/// The request's IO body and its completion: read the server through `client` and post the answer
/// to `port`. The answer is posted OUTSIDE the guard, so a read that panics lands as a FAILED fetch
/// (`None` inside) and never as an empty answer.
pub fn finish_browse_hubs_job(port: &BrowseHubsPort, addr: Addr, client: &'static Client, req: BrowseHubsReq) {
    let BrowseHubsReq { sid, key, epoch, sec, read } = req;
    let outcome = match read {
        Read::List { section, hold } => {
            // `/hubs/sections/{id}` pages by hub (docs/pms-api.md, Paging, observed): the list is read
            // a window of hubs at a time and parsed into descriptors here, off the main thread, so
            // only the held rows and one window of 12-card previews are ever held.
            let shelves = catch_unwind(AssertUnwindSafe(|| {
                parse_hub_list(sid, section, hold, |page| match page {
                    Some(page) => client.library_hubs_page(section, HUB_FETCH_COUNT, page),
                    None => client.library_hubs(section, HUB_FETCH_COUNT),
                })
            }))
            .unwrap_or(None);
            Outcome::List(shelves)
        }
        Read::Row { ask, window } => {
            let shelf = catch_unwind(AssertUnwindSafe(|| {
                read_shelf(sid, window.as_ref()?, &ask,
                    |start, size| client.hub_items_paged(&ask.key, start as i64, size as i64),
                    |keys| client.metadata_many(&keys.iter().map(String::as_str).collect::<Vec<_>>()))
            }))
            .unwrap_or(None);
            Outcome::Row { ask, shelf }
        }
    };
    port.done(addr, BrowseHubsAnswer(HubResult { gen: addr.req.0, epoch, sec, key, outcome }));
}

#[cfg(test)]
impl BrowseHubsAnswer {
    /// A FAILED whole-list read, as a worker answers `job`.
    pub(super) fn failed_for_test(job: &Job<BrowseHubsReq>) -> Self {
        Self::of_job(job, Outcome::List(None))
    }

    /// A successful whole-list read carrying `shelves`, as a worker answers `job`.
    pub(super) fn list_for_test(job: &Job<BrowseHubsReq>, shelves: Vec<Shelf>) -> Self {
        Self::of_job(job, Outcome::List(Some(Parsed { shelves })))
    }

    /// A successful read of row `(id, key)`, one page forward, carrying `shelf`, as a worker
    /// answers `job`.
    pub(super) fn row_for_test(job: &Job<BrowseHubsReq>, id: &str, key: &str, shelf: Shelf) -> Self {
        let ask = RowAsk { id: id.into(), key: key.into(), before: false, reload: false };
        Self::of_job(job, Outcome::Row { ask, shelf: Some(shelf) })
    }

    /// The same answer, but for a client the registry has since replaced: its key is `key`.
    pub(super) fn from_client_for_test(mut self, key: ClientKey) -> Self {
        self.0.key = key;
        self
    }

    fn of_job(job: &Job<BrowseHubsReq>, outcome: Outcome) -> Self {
        let BrowseHubsReq { key, epoch, sec, .. } = job.req;
        Self(HubResult { gen: job.addr.req.0, epoch, sec, key, outcome })
    }
}

/// A row's window after one ask, on the sliding window the Home rows use ([`paging::read_row`]):
/// the preview rule, the re-anchor on the edge row, the ledger when the listing will not hold still.
/// `window` is the row as it stands; a re-read starts from where the window began, or, for a row that
/// kept the rating keys it showed, reads exactly those. `None` is a failed read.
pub(super) fn read_shelf(sid: ServerId, window: &Shelf, ask: &RowAsk,
    list: impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer>,
    many: impl FnMut(&[String]) -> Option<plx_plex::plex::MediaContainer>) -> Option<Shelf> {
    if ask.reload && !window.row.kept.is_empty() {
        let items = paging::kept_cards(sid, &window.row.kept, &[], &mut { many })?;
        let items: Vec<PmsMovie> = items.into_iter().map(|item| Arc::try_unwrap(item).unwrap_or_else(|shared| (*shared).clone())).collect();
        let positions = (window.offset..window.offset + items.len()).collect();
        return Some(Shelf { items, positions, shown: 0, row: RowState { kept: Vec::new(), ..window.row.clone() }, ..window.clone() });
    }
    let (rows, mut info) = window.window();
    let start = if ask.reload { info.end = info.offset; window.offset }
        else if ask.before { window.offset } else { window.end.max(window.offset + window.items.len()) };
    let query = Ask { start, before: ask.before, hidden: &[] };
    let (rows, info, row) = paging::read_row(sid, &query, Some((rows, info)), window.row.clone(), 0, list, many)?;
    let (positions, items): (Vec<usize>, Vec<Arc<PmsMovie>>) = rows.into_iter().unzip();
    let items = items.into_iter().map(|item| Arc::try_unwrap(item).unwrap_or_else(|shared| (*shared).clone())).collect();
    Some(Shelf { items, positions, offset: info.offset, end: info.end, more: info.more, total: info.total, row,
        shown: 0, ..window.clone() })
}

// ---- the pure half --------------------------------------------------------------------------

/// One response into published shelves. **Pure**, so every rule below is host-graded against the
/// fixture in this module's tests rather than inferred from a screenshot.
///
/// The filter is [`crate::pms`]'s: allow only the types this product can list, require a title
/// and a thumb, and read the ITEM's own `type` rather than the hub's — which can be `mixed`. An
/// EMPTY hub is dropped entirely, which §3a measured as the common case (one movie section
/// answered with 6 hubs of which 5 were empty), so this is required rather than tidy: a client
/// that draws what it is given draws five headings over nothing.
///
/// Every hub the server sends is published, each as a descriptor (heading, key, total, the count of
/// cards it showed) and, for the rows in `hold`, its preview cards; the ring ([`settle`]) gives the
/// others' cards up here so a 400-row library never holds 4,800 of them. Each row is its preview
/// and then its listing: a row whose key the pager reads starts `Unprobed` and its first forward
/// ask compares the preview with the listing ([`paging::read_row`]).
pub fn parse_hubs(mc: &plx_plex::plex::MediaContainer, sid: ServerId, section: i64) -> Parsed {
    parse_hubs_held(mc, sid, section, (0, HOLD_ROWS - 1))
}

/// One response's shelves.
pub struct Parsed {
    pub shelves: Vec<Shelf>,
}

/// [`parse_hubs`] with the rows the page holds cards for named.
pub fn parse_hubs_held(
    mc: &plx_plex::plex::MediaContainer,
    sid: ServerId,
    section: i64,
    hold: (usize, usize),
) -> Parsed {
    let mut out = Vec::new();
    parse_window(&mut out, &mc.hub, sid, section, hold);
    Parsed { shelves: out }
}

/// A section's hub list read in windows of hubs ([`paging::read_hub_list`]), each window parsed into
/// `out` as it arrives and its rows outside `hold` given up before the next is read: a long list
/// holds the held rows and one window of previews at once. `read(Some(req))` is one window and
/// `read(None)` the whole list, for a server that ignores the window.
pub fn parse_hub_list(
    sid: ServerId,
    section: i64,
    hold: (usize, usize),
    read: impl FnMut(Option<plx_plex::plex::PageReq>) -> Option<plx_plex::plex::MediaContainer>,
) -> Option<Parsed> {
    paging::read_hub_list(read, Vec::new, |out, hubs| parse_window(out, hubs, sid, section, hold))
        .map(|shelves| Parsed { shelves })
}

/// One run of the hub list appended to `out`, which holds the rows before it: a row's number is its
/// place in `out`, so the held range is exact however the list was cut into windows.
fn parse_window(out: &mut Vec<Shelf>, hubs: &[plx_plex::plex::Hub], sid: ServerId, section: i64, hold: (usize, usize)) {
    let base = out.len();
    #[cfg(test)]
    let before: usize = out.iter().map(|shelf| shelf.items.len()).sum();
    for hub in hubs {
        let (positions, items): (Vec<usize>, Vec<PmsMovie>) = hub
            .metadata
            .iter()
            .enumerate()
            .filter_map(|(i, m)| crate::pms::shelf_card(m, sid).map(|card| (i, card)))
            .take(MAX_SHELF_ITEMS)
            .unzip();
        if items.is_empty() {
            continue;
        }
        if hub.title.is_empty() {
            continue; // a row with no heading has nothing to say about what is in it
        }
        let pageable = plx_plex::plex::is_pageable_hub_key(&hub.key);
        if !pageable && (hub.more || hub.total() > hub.metadata.len()) {
            if let Some(line) = paging::unpaged_line(&hub.hub_identifier, &hub.key) { plx_base::eventlog::log(&line); }
        }
        out.push(Shelf { epoch: 0,
            is_continue: shelf_is_continue(&hub.hub_identifier, &hub.key),
            landscape: is_episode_shelf(&items),
            id: hub.hub_identifier.clone(),
            key: hub.key.clone(),
            link: plx_plex::plex::collections::promoted_collection_link(
                sid, &hub.hub_identifier, &hub.key, &hub.title, section,
            ),
            total: hub.total(),
            title: plx_plex::plex::hub_title::localized_hub_title(
                plx_plex::plex::hub_title::Scope::Section,
                &hub.hub_identifier,
                &hub.title,
            ),
            items,
            positions,
            offset: 0,
            end: hub.metadata.len(),
            more: pageable
                && (hub.more || hub.total() > hub.metadata.len() || hub.metadata.len() >= HUB_FETCH_COUNT as usize),
            shown: 0,
            row: paging::preview_state(&hub.hub_identifier, &hub.key),
        });
    }
    #[cfg(test)]
    PEAK_CARDS.with(|peak| peak.set(peak.get().max(before + out[base..].iter().map(|shelf| shelf.items.len()).sum::<usize>())));
    settle_from(out, base, hold);
}

// The most cards a read has held at once, as a test reads it back.
#[cfg(test)]
thread_local! { pub(crate) static PEAK_CARDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// Gives the cards of every row outside the held range up to its descriptor. A row the pager reads
/// comes back from its listing; any other comes back through the rating keys it showed
/// ([`paging::release_row`]), and a row that shows a card with no rating key has no way back and
/// keeps its cards. Returns whether any row changed.
fn settle(shelves: &mut [Shelf], hold: (usize, usize)) -> bool {
    settle_from(shelves, 0, hold)
}

/// [`settle`] for the rows from `base` on, which are the only ones a window just appended.
fn settle_from(shelves: &mut [Shelf], base: usize, hold: (usize, usize)) -> bool {
    let (lo, hi) = clamp_hold(hold);
    let mut changed = false;
    for (row, shelf) in shelves.iter_mut().enumerate().skip(base) {
        if (lo..=hi).contains(&row) || shelf.released() { continue; }
        let pageable = plx_plex::plex::is_pageable_hub_key(&shelf.key);
        if !paging::release_row(pageable, &mut shelf.row, shelf.items.iter().map(|item| item.rk.as_str())) { continue; }
        shelf.shown = shelf.items.len();
        shelf.items = Vec::new();
        shelf.positions = Vec::new();
        changed = true;
    }
    changed
}

/// Prints the shelf's identity and how many items it holds — `PmsMovie` has no `Debug` and a
/// derived one would dump a whole shelf of DTOs into a failing assertion, which is noise rather
/// than evidence. The count is what a layout question is ever about.
impl std::fmt::Debug for Shelf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Shelf({:?} {:?} cw={} items={})",
            self.id,
            self.title,
            self.is_continue,
            self.items.len()
        )
    }
}

/// **Is this row EPISODES?** — decided from the ITEMS, never from the hub.
///
/// A TV section's `Recently Released Episodes` row is the case this exists for: every item is an
/// episode, and an episode's `thumb` is deliberately substituted with its SHOW's poster
/// (`pms::parse_item`, so a portrait card is not filled with a 16:9 still). Drawn as posters, four
/// recent episodes of one show are four identical tiles — which answers "which show" and cannot
/// answer "which episode" at all. Drawn as stills they are four different pictures.
///
/// From the items because a hub's own `type` can be `mixed` and its identifier is not a promise
/// about its contents — the same reason `parse_hubs` reads each item's own `type`. ALL of them must
/// be episodes: one film in the row would be letterboxed into a landscape slot, and a row that
/// changes shape when a single item lands is worse than one that is consistently portrait.
fn is_episode_shelf(items: &[PmsMovie]) -> bool {
    !items.is_empty() && items.iter().all(|m| m.kind == 3)
}

/// Is this the section's own Continue Watching row?
///
/// **It is not `home.continue`**, and that is the whole reason this function exists rather than a
/// call to `pms::hub_is_continue`: that id is the WHOLE-SERVER hub's and does not appear on this
/// route at all. A per-section deck is `movie.inprogress.N` / `tv.inprogress.N`, or its
/// `key=/hubs/sections/N/continueWatching/items` (§3a). Both are matched, because the id is the
/// part a server could plausibly spell differently and the key is the route it must answer at.
///
/// **The key match is SECTION-SCOPED on purpose.** The whole-server deck's key is
/// `/hubs/continueWatching/items` — the same tail — so a bare `contains` on it answers `true` for a
/// hub belonging to a different question entirely. Nothing puts that hub on this route today, so
/// the loose form was harmless and would have stayed harmless right up until somebody reached for
/// this function from the `/hubs` side.
pub fn shelf_is_continue(id: &str, key: &str) -> bool {
    id.contains(".inprogress.")
        || (key.contains("/hubs/sections/") && key.contains("/continueWatching/items"))
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "section_hubs_paging_tests.rs"]
mod paging_tests;

#[cfg(test)]
#[path = "section_hubs_job_tests.rs"]
mod job_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_browse_state_values_share_no_core_state() {
        let mut first = super::super::BrowseState::default();
        let mut second = super::super::BrowseState::default();

        first
            .sources
            .push(super::super::a_source("machine-a", "", true));
        for (key, title) in [(7, "Films A"), (8, "Films B")] {
            first.sections.push(super::super::BrowseSection {
                src: 0,
                key,
                title: title.into(),
                kind: super::super::SecKind::Movie,
                count: 12,
                pinned: true,
            });
            first.states.push(super::super::SecState::default());
        }
        first.set_cur(1);
        first.want(60, 120, None);
        first.bump_gen();
        first.bump_sections_gen();
        first.src_facts_gen = first.src_facts_gen.wrapping_add(1);
        first
            .remembered
            .push((super::super::SecKind::Movie, "machine-a".into(), 7));
        first.recorded = Some(plx_plex::plex::session::HomePins {
            user: "profile-a".into(),
            asked: true,
            on: Vec::new(),
            off: Vec::new(),
            extensions: Default::default(),
        });
        first.retry_cd = 120;
        first.states[1].hubs.armed = true;
        first.states[1].hubs.land_ok(vec![Shelf {
            id: "movie.recent.8".into(),
            title: "Recently Added".into(),
            ..Default::default()
        }]);
        assert!(first.states[1].hubs.commit_staged(true));
        let first_tabs_gen = first.tabs_gen();

        second
            .sources
            .push(super::super::a_source("machine-b", "", false));
        second.sections.push(super::super::BrowseSection {
            src: 0,
            key: 70,
            title: "Other Films".into(),
            kind: super::super::SecKind::Movie,
            count: 3,
            pinned: true,
        });
        second.states.push(super::super::SecState::default());
        second.states[0].hubs.armed = true;
        second.states[0].hubs.land_ok(vec![Shelf {
            id: "movie.recent.70".into(),
            title: "Other Recent".into(),
            ..Default::default()
        }]);
        assert!(second.states[0].hubs.commit_staged(true));
        second.want(1, 3, None);

        let retained_listing = first.listing_snapshot();
        let retained_hubs = first.hubs_snapshot(1);
        let second_hubs = second.hubs_snapshot(0);

        assert_eq!(first.sources().len(), 1);
        assert_eq!(first.sections().len(), 2);
        assert_eq!(first.states().len(), 2);
        assert_eq!(first.cur(), 1, "the test exercises a real selection move");
        assert_eq!(first.want, (60, 120));
        assert_eq!(first.query_gen(), 2);
        assert_eq!(first.sections_gen(), 1);
        assert_eq!(first.source_list_gen(), 2);
        assert!(first_tabs_gen > 1);
        assert_eq!(first.pinned_count(), 2);
        assert_eq!(first.remembered.len(), 1);
        assert!(first.recorded.is_some());
        assert_eq!(first.retry_cd, 120);
        assert_eq!(first.states[1].hubs.publication(), Publication::Committed);
        assert_eq!(first.states[1].hubs.shelves().len(), 1);

        assert_eq!(second.sources().len(), 1);
        assert_eq!(second.sections().len(), 1);
        assert_eq!(second.states().len(), 1);
        assert_eq!(second.cur(), 0);
        assert_eq!(second.want, (1, 3));
        assert_eq!(second.query_gen(), 0);
        assert_eq!(second.sections_gen(), 0);
        assert_eq!(second.source_list_gen(), 0);
        assert_eq!(second.tabs_gen, 1);
        assert_eq!(second.tab_shape, u32::MAX);
        assert_eq!(second.pinned_count(), 1);
        assert!(second.remembered.is_empty());
        assert!(second.recorded.is_none());
        assert_eq!(second.retry_cd, 0);

        first.reset();
        assert_eq!(first.query_gen(), 3);
        assert_eq!(first.sections_gen(), 2);
        assert_eq!(first.table_epoch(), 1);
        assert!(first.sections().is_empty());
        assert_eq!(
            first.want,
            (60, 120),
            "the extraction preserves legacy reset timing/state"
        );
        assert!(
            retained_listing.view().id().is_some(),
            "retained listing survives owner reset"
        );
        assert_eq!(
            retained_hubs.view().shelves().len(),
            1,
            "retained hubs survive owner reset"
        );

        assert_eq!(
            second.sections().len(),
            1,
            "resetting first cannot clear second"
        );
        assert_eq!(second.table_epoch(), 0);
        assert_eq!(second.want, (1, 3));
        assert_eq!(second.hubs_snapshot(0).view().shelves().len(), 1);
        assert_eq!(second_hubs.view().shelves().len(), 1);
    }

    /// **The measured shape**, trimmed from a live PMS 1.43.3 answer (`docs/pms-api.md` §3a) and
    /// held INLINE rather than as a captured fixture file: a real body carries this household's
    /// library into a public repo, which is the rule `plex/models.rs` states at its own fixtures.
    ///
    /// It carries the four things that decide code: a populated Continue Watching hub identified by
    /// `*.inprogress.*` AND its key, an EMPTY hub (the common case — one section answered 6 hubs of
    /// which 5 were empty), a `mixed`-typed hub whose items each carry their own type, and a
    /// collection, which is a hub like any other.
    const HUBS_JSON: &str = r#"{"MediaContainer":{"size":4,"librarySectionID":1,"Hub":[
      {"hubIdentifier":"movie.inprogress.1","title":"Continue Watching","type":"movie","size":1,
       "key":"/hubs/sections/1/continueWatching/items","Metadata":[
         {"ratingKey":"1001","type":"movie","title":"Alpha","thumb":"/t/1001",
          "librarySectionID":1,"duration":6124864,"viewOffset":1048421}]},
      {"hubIdentifier":"movie.by.actor.or.director.1.3932","title":"Top Movies with Somebody",
       "type":"movie","size":0,"key":"/library/sections/1/all?actor=3932"},
      {"hubIdentifier":"tv.recentlyadded.1","title":"Recently Added","type":"mixed","size":2,
       "key":"/hubs/sections/1/recentlyAdded/items","Metadata":[
         {"ratingKey":"2001","type":"show","title":"Beta","thumb":"/t/2001"},
         {"ratingKey":"2002","type":"episode","title":"Gamma","thumb":"/t/2002"}]},
      {"hubIdentifier":"custom.collection.1.14.14","title":"Toy Story Collection","type":"movie",
       "size":1,"key":"/library/collections/14/children","Metadata":[
         {"ratingKey":"3001","type":"movie","title":"Delta","thumb":"/t/3001"}]}
    ]}}"#;

    /// The one parse seam these tests need: the wire envelope, exactly as `Client::get_json`
    /// unwraps it, so the fixtures below are graded through the real deserializer rather than a
    /// hand-built container that could not have come off a socket.
    fn container(json: &str) -> plx_plex::plex::MediaContainer {
        serde_json::from_slice::<plx_plex::plex::Envelope>(json.as_bytes())
            .expect("the fixture parses")
            .media_container
    }

    fn parsed() -> Vec<Shelf> {
        parse_hubs(&container(HUBS_JSON), ServerId::from_raw(0), 1).shelves
    }

    /// The shelves come back in the SERVER OWNER's order — this endpoint quotes their
    /// *Manage → Libraries* table rather than composing anything — and the empty one is DROPPED.
    ///
    /// Dropping it is required rather than tidy: §3a measured one movie section answering with six
    /// hubs of which five were empty, and a client that draws what it is given draws five headings
    /// over nothing.
    #[test]
    fn an_empty_hub_is_dropped_and_the_rest_keep_the_owners_order() {
        let sh = parsed();
        let titles: Vec<&str> = sh.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            vec![
                "Continue Watching",
                "Recently Added",
                "Toy Story Collection"
            ],
            "the empty actor/director hub is not a heading over nothing"
        );
    }

    /// A per-section Continue Watching row is `*.inprogress.*` or the `continueWatching/items`
    /// key — **never `home.continue`**, which is `pms`'s whole-server id and does not appear on
    /// this route at all. Getting that wrong draws the deck as an ordinary shelf.
    #[test]
    fn the_section_deck_is_inprogress_and_not_home_continue() {
        let sh = parsed();
        assert!(sh[0].is_continue, "movie.inprogress.1 is the deck");
        assert!(!sh[1].is_continue);
        assert!(!sh[2].is_continue, "a collection is not a deck");

        // both halves of the match, independently
        assert!(shelf_is_continue("tv.inprogress.2", ""));
        assert!(shelf_is_continue(
            "",
            "/hubs/sections/1/continueWatching/items"
        ));
        // and the id that would have been reached for by habit
        assert!(
            !shelf_is_continue("home.continue", "/hubs/continueWatching/items"),
            "the whole-server hub is a different question and does not appear here"
        );
    }

    /// A hub's own `type` can be `mixed`, so the ITEM's type is the only one worth reading — which
    /// `pms::parse_item` already does. This pins that the mixed hub's two items survive with their
    /// own kinds rather than being taken for the hub's.
    #[test]
    fn a_mixed_hub_keeps_each_items_own_type() {
        let sh = parsed();
        let mixed = &sh[1];
        assert_eq!(mixed.items.len(), 2);
        // `PmsMovie::kind` is the app's own small enum (0 movie / 1 show / 2 season / 3 episode /
        // 4 collection),
        // and the point is that each item kept ITS OWN — a hub typed `mixed` cannot supply one
        assert_eq!(mixed.items[0].kind, 1, "a show");
        assert_eq!(mixed.items[1].kind, 3, "an episode");
    }

    /// The section-scope half of issue #12's fix (`plx_plex::plex::hub_title`): a library's own
    /// `/hubs/sections/{id}` Recently Added hub (`tv.recentlyadded.1`/`movie.recentlyadded.1`,
    /// PMS's own title is a plain "Recently Added" here — §3a) renders THIS client's `be` catalog
    /// string under a `be` UI, the same unconditional override Home already gets, while a hub
    /// this catalog does not recognize (a custom collection shelf) keeps drawing PMS's own title
    /// verbatim in any language — exactly `an_unrecognized_hub_identifier_keeps_the_pms_title_verbatim`'s
    /// rule in `pms_multi_source_merge_tests.rs`, proven here at the OTHER scope. Watched RED
    /// against a plain `hub.title.clone()` before the override was wired in.
    #[test]
    fn a_be_ui_localizes_the_section_recently_added_hub_and_leaves_an_unknown_one_alone() {
        let _thread_locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::Be);
        let json = r#"{"MediaContainer":{"Hub":[
          {"hubIdentifier":"tv.recentlyadded.1","title":"Recently Added","type":"mixed",
           "Metadata":[{"ratingKey":"2001","type":"show","title":"Beta","thumb":"/t/2001"}]},
          {"hubIdentifier":"custom.collection.1.14.14","title":"Toy Story Collection","type":"movie",
           "key":"/library/collections/14/children","Metadata":[
             {"ratingKey":"3001","type":"movie","title":"Delta","thumb":"/t/3001"}]}
        ]}}"#;
        let sh = parse_hubs(&container(json), ServerId::from_raw(0), 1).shelves;
        assert_eq!(sh.len(), 2);
        assert_eq!(
            sh[0].title, "Нядаўна дададзенае",
            "a be UI renders THIS client's be string for the section's own Recently Added hub, \
             not PMS's own title — and with no library name, unlike Home's form"
        );
        assert_eq!(
            sh[1].title, "Toy Story Collection",
            "an id this catalog does not enumerate keeps drawing PMS's own text verbatim"
        );
    }

    #[test]
    fn collection_metadata_is_dropped_but_collection_shelves_of_movies_survive() {
        let json = r#"{"MediaContainer":{"Hub":[
          {"hubIdentifier":"movie.recentlyadded.1","title":"Recently Added","type":"mixed",
           "Metadata":[{"ratingKey":"50001","type":"collection","title":"A Collection","thumb":"/c"}]},
          {"hubIdentifier":"custom.collection.1.50001.50001","title":"A Collection","type":"movie",
           "key":"/library/collections/50001/children","Metadata":[
             {"ratingKey":"11","type":"movie","title":"A Film","thumb":"/m"}]}]}}"#;
        let shelves = parse_hubs(&container(json), ServerId::from_raw(0), 1).shelves;
        assert_eq!(shelves.len(), 1, "a collection row must not become a movie card");
        assert_eq!(shelves[0].id, "custom.collection.1.50001.50001");
        assert_eq!(shelves[0].key, "/library/collections/50001/children",
            "a collection shelf keeps the listing key its linked heading opens");
        assert_eq!(shelves[0].items.len(), 1);
        assert_eq!(shelves[0].items[0].rk, "11");
        assert_eq!(shelves[0].items[0].kind, 0);
    }

    /// An item with no thumb is not drawable in a shelf and is dropped, and a hub left with nothing
    /// goes with it — the same filter `pms::project` applies to Home's rows (`pms::shelf_card`), so one
    /// library's shelf cannot follow a different rule from the same server's Home shelf. A missing
    /// TITLE is not a reason to drop: an item the server left unnamed is still a poster.
    #[test]
    fn an_item_with_no_artwork_is_dropped_and_can_empty_its_hub() {
        let json = r#"{"MediaContainer":{"Hub":[
          {"hubIdentifier":"movie.recentlyadded.1","title":"Recently Added","size":2,"Metadata":[
             {"ratingKey":"1","type":"movie","title":"No Art"},
             {"ratingKey":"2","type":"movie","title":"No Art Either","thumb":""}]}]}}"#;
        assert!(
            parse_hubs(&container(json), ServerId::from_raw(0), 1).shelves.is_empty(),
            "neither row has art: the hub has nothing to draw"
        );
        let json = r#"{"MediaContainer":{"Hub":[
          {"hubIdentifier":"movie.recentlyadded.1","title":"Recently Added","size":2,"Metadata":[
             {"ratingKey":"1","type":"movie","title":"No Art"},
             {"ratingKey":"2","type":"movie","title":"","thumb":"/t/2"}]}]}}"#;
        let shelves = parse_hubs(&container(json), ServerId::from_raw(0), 1).shelves;
        assert_eq!(
            shelves.iter().map(|s| s.items.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>()).collect::<Vec<_>>(),
            [vec!["2"]],
            "the artless row is dropped; the untitled one with art is still a card"
        );
    }

    /// `n_hubs` hubs of `per_hub` listable movies each, as the wire carries them.
    fn many_hubs(n_hubs: usize, per_hub: usize) -> plx_plex::plex::MediaContainer {
        let hubs: Vec<String> = (0..n_hubs)
            .map(|h| {
                let items: Vec<String> = (0..per_hub)
                    .map(|i| {
                        format!(
                            r#"{{"ratingKey":"{h}-{i}","type":"movie","title":"T{i}","thumb":"/t/{i}"}}"#
                        )
                    })
                    .collect();
                format!(
                    r#"{{"hubIdentifier":"h{h}","title":"Row {h}","Metadata":[{}]}}"#,
                    items.join(",")
                )
            })
            .collect();
        container(&format!(r#"{{"MediaContainer":{{"Hub":[{}]}}}}"#, hubs.join(",")))
    }

    /// Issue #412: the Library showed twelve shelves because this module stopped there. A server
    /// that sends fifteen hubs gets fifteen shelves, in its own order — a hub dropped here is a hub
    /// the person can never reach.
    #[test]
    fn every_hub_the_server_sends_is_published() {
        let sh = parse_hubs(&many_hubs(15, 3), ServerId::from_raw(0), 1).shelves;
        assert_eq!(sh.len(), 15, "there is no shelf cap");
        let ids: Vec<&str> = sh.iter().map(|s| s.id.as_str()).collect();
        let want: Vec<String> = (0..15).map(|h| format!("h{h}")).collect();
        assert_eq!(ids, want, "in the owner's order");
    }

    /// The per-shelf ceiling is the one cap that stays: it is the length of the card row's spring
    /// array, shared with Home.
    #[test]
    fn a_shelf_holds_at_most_max_shelf_items() {
        let sh = parse_hubs(&many_hubs(3, MAX_SHELF_ITEMS + 5), ServerId::from_raw(0), 1).shelves;
        assert_eq!(sh.len(), 3);
        assert!(sh.iter().all(|s| s.items.len() == MAX_SHELF_ITEMS), "every row is capped");
    }

    /// **A row of episodes is a LANDSCAPE row, and it is decided from the items.** The case this
    /// exists for is a TV section's `Recently Released Episodes`: `pms::parse_item` deliberately
    /// substitutes an episode's `thumb` with its SHOW's poster so portrait cards are not filled
    /// with 16:9 stills — so drawn as posters, four recent episodes of one show are four identical
    /// tiles.
    #[test]
    fn a_row_of_episodes_is_drawn_landscape_and_a_mixed_row_is_not() {
        let ep = |show: &str| PmsMovie {
            kind: 3,
            show_title: show.into(),
            ..Default::default()
        };
        let film = PmsMovie {
            kind: 0,
            ..Default::default()
        };
        // several episodes of ONE show — the hard case, and the one that looks broken today
        assert!(is_episode_shelf(&[
            ep("Caminandes"),
            ep("Caminandes"),
            ep("Caminandes")
        ]));
        // …and of different shows
        assert!(is_episode_shelf(&[ep("A"), ep("B")]));

        // ALL of them, though: one film in the row would be letterboxed into a landscape slot, and
        // a row that changes shape when a single item lands is worse than a consistently portrait one
        assert!(!is_episode_shelf(&[ep("A"), film.clone()]));
        assert!(!is_episode_shelf(&[film.clone()]));
        // …and an empty row is not a landscape row, it is a row that gets dropped
        assert!(!is_episode_shelf(&[]));
    }

    // ---- the publication machine, on the pure struct -----------------------------------------

    fn armed() -> SecHubs {
        SecHubs {
            armed: true,
            first_paint_left: FIRST_PAINT_FRAMES,
            ..Default::default()
        }
    }
    fn row(title: &str) -> Shelf {
        Shelf {
            id: title.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    fn seeded_section() -> (super::super::BrowseState, usize) {
        let mut state = super::super::BrowseState::default();
        super::super::seed_two_source_table_for_owner_test(&mut state);
        let sec = state.cur();
        seed_shelves_for_owner_test(&mut state, sec, &["published"], 3);
        (state, sec)
    }

    #[test]
    fn publication_revision_observes_in_place_deck_removal() {
        let _g = plx_base::testlock::serial();
        let (mut state, sec) = seeded_section();
        seed_shelves_for_owner_test(&mut state, sec, &["movie.inprogress.1"], 3);
        let before = state.hubs_snapshot(sec);
        let revision = before.view().revision();
        let allocation = Arc::as_ptr(before.data.as_ref().unwrap());
        let item = &before.view().shelves()[0].items[1];
        let (sid, rk) = (item.sid, item.rk.clone());
        drop(before); // No retained reader: the store can mutate the same allocation.
        assert!(state.hubs_left_the_deck(sid, &rk));
        let after = state.hubs_snapshot(sec);
        assert_eq!(Arc::as_ptr(after.data.as_ref().unwrap()), allocation);
        assert_eq!(after.view().shelves()[0].items.len(), 2);
        assert_ne!(
            after.view().revision(),
            revision,
            "pointer equality cannot detect this edit"
        );
    }

    #[test]
    fn publication_revision_tracks_commits_not_staging_or_snapshot_reads() {
        let _g = plx_base::testlock::serial();
        let (mut state, sec) = seeded_section();
        let before = state.hubs_snapshot(sec);
        let revision = before.view().revision();
        assert!(revision.is_some());
        assert_eq!(state.hubs_snapshot(sec).view().revision(), revision);
        state.state_mut(sec)
            .unwrap()
            .hubs
            .land_ok(vec![row("next")]);
        assert_eq!(state.hubs_snapshot(sec).view().revision(), revision);
        assert!(!state.hubs_commit_staged(sec, false));
        assert_eq!(state.hubs_snapshot(sec).view().revision(), revision);
        assert!(state.hubs_commit_staged(sec, true));
        assert_ne!(state.hubs_snapshot(sec).view().revision(), revision);
        assert_eq!(before.view().revision(), revision);
        assert_eq!(before.view().shelves()[0].title, "published");
        assert!(!state.hubs_commit_staged(sec, true));
        assert_eq!(state.hubs_snapshot(usize::MAX).view().revision(), None);
    }

    #[test]
    fn publication_revision_changes_for_visible_watch_edits_but_not_staged_only_edits() {
        let _g = plx_base::testlock::serial();
        let (mut state, sec) = seeded_section();
        let before = state.hubs_snapshot(sec);
        let revision = before.view().revision();
        let item = &before.view().shelves()[0].items[0];
        let (sid, rk) = (item.sid, item.rk.clone());
        assert!(!state.hubs_set_watched_local(&crate::viewstate::ViewAt::leaf(sid, "absent-item"), true));
        assert_eq!(state.hubs_snapshot(sec).view().revision(), revision);
        assert!(state.hubs_set_watched_local(&crate::viewstate::ViewAt::leaf(sid, &rk), true));
        let edited = state.hubs_snapshot(sec);
        assert_ne!(edited.view().revision(), revision);
        assert!(!before.view().shelves()[0].items[0].watched);
        assert!(edited.view().shelves()[0].items[0].watched);
        let staged = Shelf {
            items: vec![PmsMovie {
                sid,
                rk: "staged-only".into(),
                ..Default::default()
            }],
            ..row("staged")
        };
        state.state_mut(sec)
            .unwrap()
            .hubs
            .land_ok(vec![staged]);
        assert!(!state.hubs_set_watched_local(&crate::viewstate::ViewAt::leaf(sid, "staged-only"), true));
        assert_eq!(state.hubs_snapshot(sec).view().revision(), edited.view().revision());
        assert!(state.hubs_commit_staged(sec, true));
        let published = state.hubs_snapshot(sec);
        assert!(published.view().shelves()[0].items[0].watched);
        assert_ne!(published.view().revision(), edited.view().revision());
    }

    #[test]
    fn publication_revision_marks_empty_first_paint_once_and_survives_retry_only_changes() {
        let mut hubs = armed();
        let initial = hubs.revision;
        hubs.first_paint_left = 1;
        assert!(hubs.tick());
        assert_ne!(hubs.revision, initial);
        let committed = hubs.revision;
        assert!(!hubs.tick());
        hubs.land_fail();
        hubs.landing_was_not_ours();
        assert_eq!(hubs.revision, committed);
    }

    #[test]
    fn snapshot_acquisition_shares_the_publication_and_captures_its_real_identity() {
        let _g = plx_base::testlock::serial();
        let (state, sec) = seeded_section();
        let sid = state.section_sid(sec).unwrap();
        let section = state.sections()[sec].key;

        let first = state.hubs_snapshot(sec);
        let same = state.hubs_snapshot(sec);
        assert!(
            Arc::ptr_eq(first.data.as_ref().unwrap(), same.data.as_ref().unwrap()),
            "capturing a frame view must not clone shelves or items"
        );
        let view = first.view();
        let copied = view;
        assert_eq!(copied.shelves()[0].title, "published");
        assert_eq!(copied.publication(), Publication::Committed);
        assert_eq!(
            copied.id(),
            Some(HubsId {
                epoch: state.table_epoch(),
                sid,
                section,
            })
        );

        let missing_snapshot = state.hubs_snapshot(usize::MAX);
        let missing = missing_snapshot.view();
        assert_eq!(
            missing.id(),
            None,
            "absence has no invented section identity"
        );
        assert!(missing.shelves().is_empty());
        assert_eq!(missing.publication(), Publication::Fetching);
    }

    #[test]
    fn a_retained_snapshot_survives_commit_staged() {
        let _g = plx_base::testlock::serial();
        let (mut state, sec) = seeded_section();
        state.state_mut(sec)
            .unwrap()
            .hubs
            .land_ok(vec![row("replacement")]);

        let retained = state.hubs_snapshot(sec);
        let old = retained.view();
        assert_eq!(old.publication(), Publication::Staged);
        assert_eq!(old.shelves()[0].title, "published");
        assert!(state.hubs_commit_staged(sec, true));

        let current = state.hubs_snapshot(sec);
        assert!(!Arc::ptr_eq(
            retained.data.as_ref().unwrap(),
            current.data.as_ref().unwrap()
        ));
        assert_eq!(current.view().publication(), Publication::Committed);
        assert_eq!(current.view().shelves()[0].title, "replacement");
        assert_eq!(
            old.shelves()[0].title,
            "published",
            "the old frame stays valid"
        );
    }

    #[test]
    fn watched_edits_are_copy_on_write_and_an_unmatched_edit_copies_nothing() {
        let _g = plx_base::testlock::serial();
        let (mut state, sec) = seeded_section();
        let (sid, rk) = {
            let section = state.state_mut(sec).unwrap();
            section.hubs.staged = Some(section.hubs.committed.clone());
            let item = &section.hubs.committed[0].items[1];
            (item.sid, item.rk.clone())
        };

        let retained = state.hubs_snapshot(sec);
        let old = retained.view();
        let staged_before = state.state_mut(sec)
            .unwrap()
            .hubs
            .staged
            .as_ref()
            .unwrap()
            .clone();
        assert!(!state.hubs_set_watched_local(&crate::viewstate::ViewAt::leaf(sid, "not-present"), true));
        let after_miss = state.hubs_snapshot(sec);
        assert!(Arc::ptr_eq(
            retained.data.as_ref().unwrap(),
            after_miss.data.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            &staged_before,
            state.state_mut(sec)
                .unwrap()
                .hubs
                .staged
                .as_ref()
                .unwrap()
        ));

        assert!(state.hubs_set_watched_local(&crate::viewstate::ViewAt::leaf(sid, &rk), true));
        let edited = state.hubs_snapshot(sec);
        assert!(!Arc::ptr_eq(
            retained.data.as_ref().unwrap(),
            edited.data.as_ref().unwrap()
        ));
        assert!(
            !old.shelves()[0].items[1].watched,
            "retained data is immutable"
        );
        assert!(edited.view().shelves()[0].items[1].watched);
        assert!(state.hubs_commit_staged(sec, true));
        assert!(
            state.hubs_snapshot(sec).view().shelves()[0].items[1].watched,
            "held data was edited too"
        );
    }

    #[test]
    fn a_retained_snapshot_survives_reset_and_the_reused_index_gets_a_new_identity() {
        let _g = plx_base::testlock::serial();
        let (mut state, sec) = seeded_section();
        let retained = state.hubs_snapshot(sec);
        let old = retained.view();
        let old_id = old.id().unwrap();

        state.reset();
        assert_eq!(state.hubs_snapshot(sec).view().id(), None);
        super::super::seed_two_source_table_for_owner_test(&mut state);
        let replacement_snapshot = state.hubs_snapshot(sec);
        let replacement = replacement_snapshot.view();
        assert_ne!(replacement.id().unwrap().epoch, old_id.epoch);
        assert_eq!(old.shelves()[0].title, "published");
        assert_eq!(old.id(), Some(old_id));
    }

    /// **The first-paint window expiring publishes ZERO shelves**, and the page becomes usable at
    /// the offset it already has. It has to be a separate policy from the retry ladder, because
    /// that ladder is INFINITE (2/4/8/16/30s then 30s forever) — "commit when the fetch is
    /// terminal" never arrives on a dead server, and the page would wait for it.
    #[test]
    fn the_first_paint_window_commits_nothing_and_makes_the_page_usable() {
        let mut h = armed();
        assert_eq!(h.publication(), Publication::Fetching);
        for _ in 0..FIRST_PAINT_FRAMES - 1 {
            assert!(!h.tick(), "still inside the window");
        }
        assert!(h.tick(), "the window closed, and that is a repaint");
        assert_eq!(h.publication(), Publication::Committed);
        assert!(h.shelves().is_empty(), "committed to nothing, deliberately");
        assert!(
            !h.tick(),
            "…and it settles rather than reporting every frame"
        );
    }

    /// **A landing INSIDE the window still asks whether the ground may move.** It used to commit
    /// itself, on the reasoning that a page with no layout yet has nothing to protect — but the
    /// window is four SECONDS, and `/tmp/plxnative-library` plus two presses is a viewer standing
    /// in the grid well inside it. So the first paint takes the same permission every later one
    /// does; at the head that is granted in the same frame the landing pumps, which is why nothing
    /// waits in the ordinary case.
    #[test]
    fn the_first_answer_inside_the_window_still_asks_before_it_moves_the_ground() {
        let mut h = armed();
        h.land_ok(vec![row("A"), row("B")]);
        assert_eq!(h.publication(), Publication::Staged);
        assert!(
            h.shelves().is_empty(),
            "a viewer already in the grid does not get the document moved under them"
        );

        assert!(!h.commit_staged(false), "…not while they are looking at it");
        assert!(h.commit_staged(true), "…and at the head it goes in at once");
        assert_eq!(h.publication(), Publication::Committed);
        assert_eq!(h.shelves().len(), 2);
    }

    /// …and the window is not what holds a staged first landing, so it neither publishes over it
    /// nor keeps reporting a change nobody can see.
    #[test]
    fn the_window_expiring_over_a_staged_first_landing_publishes_nothing() {
        let mut h = armed();
        h.land_ok(vec![row("A")]);
        for _ in 0..FIRST_PAINT_FRAMES + 5 {
            assert!(
                !h.tick(),
                "the window has nothing to say about a held answer"
            );
        }
        assert_eq!(h.publication(), Publication::Staged);
        assert!(h.shelves().is_empty());
        assert!(h.commit_staged(true));
        assert_eq!(h.shelves().len(), 1, "and the answer was not lost");
    }

    /// **A late success STAGES rather than moving a grid the user is looking at.** This is the
    /// case the whole machine exists for: the shelf count sets the grid's absolute offset, so a
    /// 12-shelf landing after the window closed would move a focused row by 6,588px.
    #[test]
    fn a_late_success_is_staged_and_commits_only_when_the_ground_may_move() {
        let mut h = armed();
        for _ in 0..FIRST_PAINT_FRAMES {
            h.tick();
        }
        h.land_ok(vec![row("A")]);
        assert_eq!(h.publication(), Publication::Staged);
        assert!(h.shelves().is_empty(), "the document has not moved");

        assert!(
            !h.commit_staged(false),
            "the user is looking at it: nothing publishes"
        );
        assert_eq!(h.publication(), Publication::Staged);

        assert!(h.commit_staged(true), "…and at the floor it does");
        assert_eq!(h.publication(), Publication::Committed);
        assert_eq!(h.shelves().len(), 1);
        assert!(!h.commit_staged(true), "exactly once");
    }

    /// A NEWER success replaces an older staged one — the freshest answer is the one worth showing
    /// when the page finally lets it through.
    #[test]
    fn a_newer_landing_replaces_an_older_staged_set() {
        let mut h = armed();
        h.land_ok(vec![row("first")]);
        h.land_ok(vec![row("second")]);
        h.land_ok(vec![row("third")]);
        assert_eq!(h.publication(), Publication::Staged);
        h.commit_staged(true);
        assert_eq!(h.shelves()[0].title, "third");
    }

    /// **A failure subtracts nothing**, from either set. It arms the backoff and leaves the
    /// committed shelves and any staged success exactly where they were — the same trade
    /// `pms::Src::last` makes, and the reason a wifi hiccup cannot empty a populated page.
    ///
    /// It is also the ORDINARY path here rather than an edge case: §3a measured two hubs changing
    /// identity between requests, so a refetch legitimately returns a different set with nothing
    /// changed on the server.
    #[test]
    fn a_failed_refresh_keeps_both_the_committed_and_the_staged_set() {
        let mut h = armed();
        h.land_ok(vec![row("committed")]);
        h.commit_staged(true); // the page was at its head, so the first paint went in
        h.land_ok(vec![row("staged")]);
        h.land_fail();
        assert_eq!(h.shelves()[0].title, "committed", "nothing was subtracted");
        assert_eq!(h.publication(), Publication::Staged);
        assert!(h.retry_left > 0, "…and the ladder is armed");
        h.commit_staged(true);
        assert_eq!(h.shelves()[0].title, "staged");
    }

    /// **Removing an item from Continue Watching removes it from the HELD set too.** The
    /// optimistic edit took the committed shelves and not the staged ones, so a refresh waiting to
    /// publish still carried the row: the tile vanished on the press and came back the moment the
    /// ground was next allowed to move, which reads as the press having failed. Its sibling
    /// `set_watched_local` already edited both.
    ///
    /// Driven through the real `left_the_deck` against seeded `browse` state, because the defect
    /// is precisely which of the two sets that function reaches.
    #[test]
    fn removing_from_the_deck_reaches_the_staged_set_as_well() {
        let _g = plx_base::testlock::serial();
        let _t = plx_plex::plex::session::TempSession::new("deckstage");
        _t.watching("u-deckstage");
        let mut state = super::super::BrowseState::default();
        super::super::seed_two_source_table_for_owner_test(&mut state);
        let sec = state.cur();

        // a committed deck…
        seed_shelves_for_owner_test(&mut state, sec, &["movie.inprogress.1"], 3);
        let (sid, rk) = {
            let st = state.state_mut(sec).expect("a seeded section");
            let m = &st.hubs.committed[0].items[1];
            (m.sid, m.rk.clone())
        };

        // …and an older refresh held behind it, carrying the same item
        {
            let st = state.state_mut(sec).expect("a seeded section");
            st.hubs.staged = Some(st.hubs.committed.clone());
        }

        let retained = state.hubs_snapshot(sec);
        let old = retained.view();

        assert!(state.hubs_left_the_deck(sid, &rk), "the visible deck loses the row");
        let edited = state.hubs_snapshot(sec);
        assert!(!Arc::ptr_eq(
            retained.data.as_ref().unwrap(),
            edited.data.as_ref().unwrap()
        ));
        assert_eq!(
            old.shelves()[0].items.len(),
            3,
            "the retained deck is immutable"
        );
        assert_eq!(edited.view().shelves()[0].items.len(), 2);
        {
            let st = state.state_mut(sec).unwrap();
            assert!(st.hubs.committed[0].items.iter().all(|m| m.rk != rk));
        }

        // publishing the held answer must not bring it back
        assert!(state.hubs_commit_staged(sec, true));
        {
            let st = state.state_mut(sec).unwrap();
            assert!(
                st.hubs
                    .committed
                    .iter()
                    .flat_map(|sh| sh.items.iter())
                    .all(|m| m.rk != rk),
                "the staged set resurrected a row the user removed"
            );
        }
    }

    /// **A landing rejected on the client's LIFECYCLE leaves the section still owed a fetch.**
    /// `hubs_spawn` clears `owed` when it emits the read, and an answer that arrives is not the same
    /// as "the answer will be usable". `land` discards a response whose server slot was re-pointed
    /// or retokened since the read was emitted, and `kick` re-arms neither a published section nor
    /// an unpublished one with `fails == 0`. So an ordinary `sync_roster` re-point or a token
    /// refresh could leave a library's shelves empty, or a refresh stale, for the rest of the
    /// session — silently, with no failure anywhere to read.
    ///
    /// **Graded here on `landing_was_not_ours` alone.** The end-to-end rejection, a landing whose
    /// `ClientKey` moved, is `section_hubs_job_tests.rs`'s
    /// `a_landing_whose_client_moved_is_not_ours_and_the_section_is_still_owed`; the state change
    /// lives in a named method so this one can drive it without a registered client.
    #[test]
    fn a_lifecycle_rejected_landing_leaves_the_section_owed() {
        let mut h = armed();

        // the state a successful CLAIM leaves behind: armed, nothing outstanding
        h.owed = false;
        assert_eq!(h.fails, 0);
        assert_eq!(h.publication(), Publication::Fetching);

        h.landing_was_not_ours();

        assert!(
            h.owed,
            "the section is still owed its shelves — nothing else will ever re-arm it"
        );
        assert_eq!(h.fails, 0, "a lifecycle rejection is not a failure");
        assert_eq!(h.retry_left, 0, "…so the ladder is not armed either");
        assert!(h.committed.is_empty(), "and nothing was published");

        // …and a section nobody has asked for shelves yet is not made to want them
        let mut cold = SecHubs::default();
        cold.landing_was_not_ours();
        assert!(!cold.owed, "an unarmed section stays dormant");
    }

    /// The ladder is the shared 2/4/8/16/30s one and it CLIMBS, then holds. A hoist of the pure
    /// function only: `pms`'s `landed_ok`/`landed_fail` beside it carry Home-specific policy.
    #[test]
    fn the_retry_ladder_climbs_and_then_holds() {
        let mut h = armed();
        h.land_fail();
        let a = h.retry_left;
        h.land_fail();
        let b = h.retry_left;
        assert!(b > a, "2s then 4s");
        for _ in 0..8 {
            h.land_fail();
        }
        assert_eq!(
            h.retry_left,
            backoff_frames(99),
            "…and holds at the ceiling forever, which is why the first-paint window exists"
        );
    }

    /// **A refresh blocked behind another section's fetch is not LOST.** The single flight is one
    /// BrowseStore-wide claim, so `spawn` returns early while another section in the same owner is
    /// out — and before 2026-09-05 nothing recorded that this section still wanted one, after which
    /// `kick` also returned early because the section WAS published and had no failures. Its
    /// Continue Watching row then stayed stale for the rest of the session.
    ///
    /// Graded on the pure bits; the step path is driven end to end in `section_hubs_job_tests.rs`.
    #[test]
    fn an_invalidate_blocked_by_another_sections_fetch_stays_owed() {
        let mut h = armed();
        h.land_ok(vec![row("A")]);
        h.commit_staged(true);
        h.owed = false; // the claim was taken and the answer landed
        assert_eq!(h.publication(), Publication::Committed);

        // playback ends: this section's deck is stale
        h.fails = 0;
        h.retry_left = 0;
        h.owed = true;

        // …another section holds the flight for a while. Nothing here may clear the bit.
        for _ in 0..600 {
            h.tick();
        }
        assert!(
            h.owed,
            "the refresh survives being blocked, or the deck is stale for the session"
        );
        assert_eq!(
            h.publication(),
            Publication::Committed,
            "…and nothing about the page moved while it waited"
        );
    }

    /// A FAILED landing is still owed an answer: the ladder decides when, `owed` decides that it
    /// happens at all. Without the bit a section that failed once and then lost the race for the
    /// flight would never ask again.
    #[test]
    fn a_failure_leaves_the_section_owed() {
        let mut h = armed();
        h.owed = false;
        h.land_fail();
        // `land()` sets the bit beside this call; the ladder is what holds it off
        h.owed = true;
        assert!(h.retry_left > 0, "the backoff is armed");
        assert!(h.owed, "…and the request is still outstanding");
    }

    /// A section nobody armed is inert: it neither ticks toward publishing nor counts a backoff
    /// down. That is what makes this module DORMANT while `pump` already calls `tick_all` — the
    /// wiring is real, the behaviour is not reachable until a screen calls `kick`.
    #[test]
    fn an_unarmed_section_is_inert() {
        let mut h = SecHubs::default();
        for _ in 0..FIRST_PAINT_FRAMES * 2 {
            assert!(!h.tick());
        }
        assert_eq!(h.publication(), Publication::Fetching);
        assert!(h.shelves().is_empty());
    }

    // ---- a notification's invalidate: one server, no ladder reset, spaced ---------------------

    const SERVER_A: ServerId = ServerId::from_raw(3);
    const SERVER_B: ServerId = ServerId::from_raw(4);

    /// Four published sections: two on server A, two on server B, nothing owed.
    fn two_servers() -> (super::super::BrowseState, Arc<super::super::BrowseAdapter>) {
        let (mut state, _) = seeded_section();
        state.source_mut(0).unwrap().sid = SERVER_A;
        state.source_mut(1).unwrap().sid = SERVER_B;
        for sec in 0..state.sections().len() {
            seed_shelves_for_owner_test(&mut state, sec, &["row"], 1);
            state.state_mut(sec).unwrap().hubs.owed = false;
        }
        (state, Arc::new(super::super::BrowseAdapter::default()))
    }

    fn owed_by_server(state: &super::super::BrowseState, sid: ServerId) -> Vec<bool> {
        (0..state.sections().len()).filter(|i| state.section_sid(*i) == Some(sid))
            .map(|i| state.states()[i].hubs.owed).collect()
    }

    #[test]
    fn a_notified_invalidate_leaves_the_other_servers_sections_alone() {
        let _g = plx_base::testlock::serial();
        let (mut state, adapter) = two_servers();
        state.hubs_invalidate_server(SERVER_A, &adapter, &ClientKeys::default(), &mut Vec::new());
        assert_eq!(owed_by_server(&state, SERVER_A), [true, true], "the notifying server is re-read");
        assert_eq!(owed_by_server(&state, SERVER_B), [false, false], "another server's shelves are untouched");
    }

    #[test]
    fn a_notified_invalidate_does_not_reset_the_failure_ladder() {
        let _g = plx_base::testlock::serial();
        let (mut state, adapter) = two_servers();
        let sec = (0..state.sections().len()).find(|i| state.section_sid(*i) == Some(SERVER_A)).unwrap();
        {
            let h = &mut state.state_mut(sec).unwrap().hubs;
            h.fails = 4;
            h.retry_left = 900;
        }
        state.hubs_invalidate_server(SERVER_A, &adapter, &ClientKeys::default(), &mut Vec::new());
        let h = &state.states()[sec].hubs;
        assert_eq!((h.fails, h.retry_left), (4, 900), "a stranger's stop does not forgive a failing server");
        assert!(h.owed, "the read is still owed behind the ladder");
        // …whereas the view-state invalidate (the user's own press) does reset it
        state.hubs_invalidate_all(&adapter, &ClientKeys::default(), &mut Vec::new());
        let h = &state.states()[sec].hubs;
        assert_eq!((h.fails, h.retry_left), (0, 0));
    }

    #[test]
    fn a_burst_of_notified_invalidates_is_one_read_per_spacing() {
        let _g = plx_base::testlock::serial();
        let (mut state, adapter) = two_servers();
        state.hubs_invalidate_server(SERVER_A, &adapter, &ClientKeys::default(), &mut Vec::new());
        assert_eq!(owed_by_server(&state, SERVER_A), [true, true]);
        for sec in 0..state.sections().len() {
            state.state_mut(sec).unwrap().hubs.owed = false; // the reads were claimed
        }
        for _ in 0..10 {
            state.hubs_invalidate_server(SERVER_A, &adapter, &ClientKeys::default(), &mut Vec::new()); // …and a burst follows inside the spacing
        }
        assert_eq!(owed_by_server(&state, SERVER_A), [false, false], "coalesced, not read again at once");
        let spacing = NOTIFY_SPACING_FRAMES as usize;
        for _ in 0..spacing - 1 {
            state.hubs_tick_all(&adapter, &ClientKeys::default(), &mut Vec::new());
        }
        assert_eq!(owed_by_server(&state, SERVER_A), [false, false], "still inside the spacing");
        state.hubs_tick_all(&adapter, &ClientKeys::default(), &mut Vec::new());
        assert_eq!(owed_by_server(&state, SERVER_A), [true, true], "the whole burst is ONE read once the spacing is over");
        for sec in 0..state.sections().len() {
            state.state_mut(sec).unwrap().hubs.owed = false;
        }
        for _ in 0..spacing * 2 {
            state.hubs_tick_all(&adapter, &ClientKeys::default(), &mut Vec::new());
        }
        assert_eq!(owed_by_server(&state, SERVER_A), [false, false], "and no more than one");
    }
}
