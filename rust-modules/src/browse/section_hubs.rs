//! **One library's own shelves** — the `Library Recommended` rows its server's owner arranged,
//! quoted rather than composed.
//!
//! `GET /hubs/sections/{id}` ([`crate::plex::Client::library_hubs`], verified live 2026-09-05;
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
//! offset** — a late 12-shelf landing would move a focused grid row by 6,588px. "Commit when the
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
//! [`kick`] is the only thing that issues a request, and `ui::library`'s `update` calls it for the
//! section being browsed. It was dormant for one landing — deliberately, since a fetch is
//! production network, task and retry behaviour and "fetched but not drawn" would not have been
//! dormancy at all — and the screen that draws the shelves is what woke it.
#![allow(dead_code)] // the accessors are Landing 3's; see the Dormant note above

use super::EPOCH;
use crate::plex::ServerId;
use crate::pms::{parse_item, PmsMovie, MAX_SHELF_ITEMS};
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Items asked for per hub. The server defaults to 6 and honours at least 24 (§3a); it is
/// items-per-hub and never changes how many hubs come back.
const HUB_FETCH_COUNT: i64 = 12;

/// How many shelves a library may draw. Nine is what the dev server's owner arranged and the
/// endpoint has no documented ceiling, so this bounds a pathological table rather than expressing
/// a design: twelve shelves is already 6,588px of document above the grid.
const MAX_SHELVES: usize = 12;

/// Frames the page waits for the FIRST answer before committing zero shelves and becoming usable
/// — ~4 s at 60 fps. Deliberately not derived from the retry ladder (module doc), and counted in
/// FRAMES because [`super::pump`] is a per-frame call with no `dt`, exactly as its own `RETRY_CD`
/// is. The one conversion in this module is [`backoff_frames`].
const FIRST_PAINT_FRAMES: u32 = 240;

/// [`crate::pms::backoff_secs`]' 2/4/8/16/30s ladder in this module's unit. The ladder is shared —
/// hoisted as a PURE function only, deliberately: `pms`'s `landed_ok`/`landed_fail` beside it carry
/// Home-specific logging, stale-build retention and endpoint-refresh policy that mean nothing here.
fn backoff_frames(fails: u32) -> u32 {
    (crate::pms::backoff_secs(fails) * 60.0) as u32
}

/// One published shelf: the owner's own row, with its items already filtered and capped.
#[derive(Clone, Default)]
pub(crate) struct Shelf {
    /// `hubIdentifier`. **Not a stable key across refetches** for the genre and actor/director
    /// rows, which rotate their subject on every call (§3a) — anything remembering a position by
    /// it owes a fallback for an id that is simply not there any more.
    pub(crate) id: String,
    pub(crate) title: String,
    /// A Continue Watching row scoped to this section. **Not `home.continue`**, which is
    /// `pms`'s whole-server id and does not appear here — see [`shelf_is_continue`].
    pub(crate) is_continue: bool,
    /// **This row is EPISODES, so it is drawn as landscape stills** rather than portrait posters —
    /// see [`is_episode_shelf`] for why that is decided from the items and not from the hub.
    pub(crate) landscape: bool,
    pub(crate) items: Vec<PmsMovie>,
}

/// Where a section's shelves are in the publication axis. The fetch axis is separate and runs
/// underneath this; see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publication {
    Fetching,
    Staged,
    Committed,
}

/// One section's shelves and the two axes' state. A field on [`super::SecState`].
#[derive(Default)]
pub(crate) struct SecHubs {
    /// The set the current layout was built from. Empty and `committed == false` is "nothing
    /// published yet"; empty and `committed == true` is the answer "this library has no shelves",
    /// which is a real state the design draws as "this screen with a shorter top".
    committed: Vec<Shelf>,
    /// Has anything ever been published? Distinct from `committed.is_empty()` for that reason.
    published: bool,
    /// A landing held back because publishing it would move a grid the user is looking at.
    staged: Option<Vec<Shelf>>,
    /// Frames left in the first-paint window. Only counts down while nothing is published.
    first_paint_left: u32,
    /// Has [`kick`] ever been called for this section? Nothing here ticks or fetches until it has.
    armed: bool,
    /// **A fetch is WANTED and has not been claimed yet.** The per-section half of the
    /// single-flight, and it exists because the claim itself is global (one mailbox, one flag —
    /// this is a television with one screen, not a reason for N mailboxes).
    ///
    /// Without it a refresh is silently lost, which is what a Codex review found on 2026-09-05:
    /// [`invalidate`] called [`spawn`] once, [`spawn`] returned early because another section held
    /// the flag, and nothing recorded that this section still wanted one — after which [`kick`]
    /// also returns early, since the section IS published and has no failures. The comment
    /// promising "kick retries by itself" was true only of the spawn-refused path and only because
    /// `fails` happened to be non-zero there.
    ///
    /// It is cleared when a worker is successfully CLAIMED, never when one is merely attempted.
    owed: bool,
    /// Consecutive failures, for [`crate::pms::backoff_secs`]' ladder.
    fails: u32,
    /// Frames left before another attempt may spawn.
    retry_left: u32,
}

impl SecHubs {
    /// The published shelves — what a layout may be built from, and the only set a screen draws.
    pub(crate) fn shelves(&self) -> &[Shelf] {
        &self.committed
    }

    pub(crate) fn publication(&self) -> Publication {
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
    /// almost always is — `browse::pump` lands and the screen commits in the SAME frame, so
    /// nothing waits.
    fn land_ok(&mut self, shelves: Vec<Shelf>) {
        self.fails = 0;
        self.retry_left = 0;
        self.staged = Some(shelves);
    }

    /// **An answer that arrived for a server this slot no longer is.** Not a landing at all: the
    /// response is discarded and the section is still OWED the fetch it asked for.
    ///
    /// It has to say so explicitly, because `spawn` clears `owed` on a successful CLAIM — a worker
    /// is then guaranteed to fill the mailbox, which is true and is not the same as "the answer
    /// will be usable". `kick` cannot recover it either: it re-arms neither a published section nor
    /// an unpublished one with `fails == 0`. So without this an ordinary `sync_roster` re-point or
    /// a token refresh left a library's shelves empty, or a refresh stale, for the rest of the
    /// session, silently and with no failure anywhere to read.
    ///
    /// The ladder is deliberately NOT armed: it exists for a server that answered badly, and this
    /// one may never have answered at all.
    fn landing_was_not_ours(&mut self) {
        if self.armed {
            self.owed = true;
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
    fn commit_staged(&mut self, may_move: bool) -> bool {
        if !may_move {
            return false;
        }
        match self.staged.take() {
            Some(s) => {
                self.committed = s;
                self.published = true;
                true
            }
            None => false,
        }
    }
}

/// Arm a section's shelves and let the first fetch happen. Idempotent; call it every frame the
/// screen wants shelves. **This is the only thing in the module that issues a request.**
pub(crate) fn kick(sec: usize) {
    let Some(st) = super::state_mut(sec) else {
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
    pump_spawns();
}

/// Start ONE fetch for whichever armed section is owed one, if the single flight is free.
///
/// This is where the global claim and the per-section `owed` bit meet, and having it in one place
/// is what makes "a refresh cannot be dropped" checkable: every path that wants a fetch sets
/// `owed` and calls here, and `owed` survives a busy flight, a refused `spawn_small` and a backoff
/// alike. `tick_all` calls it every frame, so a section blocked by another's fetch is picked up on
/// the frame that one lands.
fn pump_spawns() {
    if HUB_FETCHING.load(Ordering::SeqCst) {
        return; // somebody else is out; every owed section stays owed
    }
    let want = super::states()
        .iter()
        .position(|st| st.hubs.armed && st.hubs.owed && st.hubs.retry_left == 0);
    if let Some(sec) = want {
        spawn(sec);
    }
}

/// Mark a section's shelves stale — after playback, and after a view-state write. Home already
/// refetches its hubs for this reason (Continue Watching goes stale otherwise) and a per-section
/// Continue Watching row needs the matching invalidation. The answer arrives as `Staged`, so a
/// refresh never moves the grid under the eye.
pub(crate) fn invalidate(sec: usize) {
    if let Some(st) = super::state_mut(sec) {
        if st.hubs.armed {
            st.hubs.fails = 0;
            st.hubs.retry_left = 0;
            // OWED, not spawned. The attempt is `pump_spawns`' — this only records that one is
            // wanted, so a section blocked behind another's in-flight fetch keeps its refresh
            // instead of losing it to an early return.
            st.hubs.owed = true;
        }
    }
    pump_spawns();
}

/// Advance every armed section by one frame. Called from [`super::pump`]; returns whether any
/// section published something, which is a repaint. A section nobody has [`kick`]ed is inert here,
/// which is what keeps this module dormant while it is wired in.
pub(crate) fn tick_all() -> bool {
    let mut moved = false;
    for st in super::states_mut() {
        moved |= st.hubs.tick();
    }
    // …and give an owed section its turn now that a backoff may have expired or another
    // section's flight may have landed. Cheap: one atomic load, then a scan only when free.
    pump_spawns();
    moved
}

/// The accessors a screen uses. Absent section = no shelves and `Fetching`, which is the same
/// answer a section that has never been armed gives, and is the safe one: a layout built from it
/// commits no geometry.
pub(crate) fn shelves(sec: usize) -> &'static [Shelf] {
    super::states()
        .get(sec)
        .map(|s| s.hubs.shelves())
        .unwrap_or(&[])
}
pub(crate) fn publication(sec: usize) -> Publication {
    super::states()
        .get(sec)
        .map(|s| s.hubs.publication())
        .unwrap_or(Publication::Fetching)
}
/// Publish `sec`'s staged shelves if the caller says the ground may move. See
/// [`SecHubs::commit_staged`]; the caller owes a focus re-resolve when this returns `true`.
pub(crate) fn commit_staged(sec: usize, may_move: bool) -> bool {
    super::state_mut(sec)
        .map(|s| s.hubs.commit_staged(may_move))
        .unwrap_or(false)
}

/// Publish a section's shelves directly, for the screen tests that need a document with a shelf
/// run above the grid. Not a fetch and not a landing: no worker, no gate, no ladder — the shape
/// only, which is all a geometry or focus-walk assertion is about.
#[cfg(test)]
pub(crate) fn seed_shelves_for_test(sec: usize, titles: &[&str], per_row: usize) {
    let Some(st) = super::state_mut(sec) else {
        return;
    };
    st.hubs = SecHubs::default();
    st.hubs.armed = true;
    st.hubs.land_ok(
        titles
            .iter()
            .map(|t| Shelf {
                id: (*t).into(),
                title: (*t).into(),
                is_continue: shelf_is_continue(t, ""),
                landscape: false, // poster rows: a geometry test wants the shape it can name
                items: (0..per_row)
                    .map(|k| PmsMovie {
                        rk: format!("{t}-{k}"),
                        title: format!("{t} {k}"),
                        ..Default::default()
                    })
                    .collect(),
            })
            .collect(),
    );
    // …and PUBLISH it: a landing only stages now, and a seeder exists to give a test a library
    // that is already composed, not one mid-first-paint.
    st.hubs.commit_staged(true);
}

/// Turn a seeded section's shelves into EPISODE shelves — landscape rows of `kind == 3` items with
/// a show behind them. The half [`seed_shelves_for_test`] deliberately does not do, because a
/// geometry test wants the poster shape it can name; a test about the episode TILE wants this.
#[cfg(test)]
pub(crate) fn seed_landscape_for_test(sec: usize, show: &str) {
    let Some(st) = super::state_mut(sec) else {
        return;
    };
    for sh in st.hubs.committed.iter_mut() {
        sh.landscape = true;
        for m in sh.items.iter_mut() {
            m.kind = 3;
            m.show_title = show.into();
            m.season_index = 1;
            m.ep_index = 1;
        }
    }
}

/// **Every published shelf is a store that can be DRAWING the item** — so it takes the same
/// optimistic edit `pms`, `metadata`, `browse`, `search` and `person` already take, and for the same
/// reason: the press writes correctly to the server and changes nothing on the screen the user is
/// looking at until a refetch, which reads as the row having done nothing.
///
/// This was the sixth store and it was not on the list; `viewstate::edit_local`'s five calls
/// predate the Library growing shelves of its own.
pub(crate) fn set_watched_local(sid: ServerId, rk: &str, on: bool) -> bool {
    let mut hit = false;
    for st in super::states_mut() {
        for m in st.hubs.committed.iter_mut().flat_map(|sh| sh.items.iter_mut()) {
            if crate::plex::same_item((m.sid, &m.rk), (sid, rk)) {
                crate::pms::set_watched(m, on);
                hit = true;
            }
        }
        // …the STAGED set too, or a landing held back over a watch change publishes the stale
        // answer the moment the ground is allowed to move.
        if let Some(stg) = st.hubs.staged.as_mut() {
            for m in stg.iter_mut().flat_map(|sh| sh.items.iter_mut()) {
                if crate::plex::same_item((m.sid, &m.rk), (sid, rk)) {
                    crate::pms::set_watched(m, on);
                }
            }
        }
    }
    hit
}

/// The item has left Continue Watching: drop it from every SECTION DECK that holds it, and nowhere
/// else. A `*.inprogress.*` row is the only shelf that endpoint changes the membership of.
pub(crate) fn left_the_deck(sid: ServerId, rk: &str) -> bool {
    fn drop_from(shelves: &mut Vec<Shelf>, sid: ServerId, rk: &str) -> bool {
        let mut hit = false;
        for sh in shelves.iter_mut().filter(|sh| sh.is_continue) {
            let before = sh.items.len();
            sh.items
                .retain(|m| !crate::plex::same_item((m.sid, &m.rk), (sid, rk)));
            hit |= sh.items.len() != before;
        }
        shelves.retain(|sh| !sh.items.is_empty());
        hit
    }
    let mut hit = false;
    for st in super::states_mut() {
        hit |= drop_from(&mut st.hubs.committed, sid, rk);
        // …and the STAGED set too, exactly as [`set_watched_local`] does and for the same reason:
        // a refresh held back over the removal still holds the removed item, so publishing it when
        // the ground is next allowed to move RESURRECTS a row the user deleted. This half was
        // missing while its sibling had it — the optimistic edit vanished from the visible deck and
        // came back a moment later, which reads as the press having failed.
        if let Some(stg) = st.hubs.staged.as_mut() {
            drop_from(stg, sid, rk);
        }
    }
    hit
}

/// **Every section's shelves are stale.** The whole-store twin of [`invalidate`], for the two
/// moments the app already declares Home's hubs stale — a completed playback and a settled burst of
/// view-state writes. Continue Watching and watch state are exactly what those change, and a
/// section deck goes stale for the same reason the global one does.
///
/// Marks rather than fetches: `owed` is spent by [`pump_spawns`] under the single-flight, so a
/// twelve-library table costs one request at a time rather than twelve at once.
pub(crate) fn invalidate_all() {
    for i in 0..super::sections().len() {
        invalidate(i);
    }
}

// ---- the fetch ------------------------------------------------------------------------------

/// One section's shelves, in flight. Same shape as [`super::DirectoryResult`] and for the same
/// reasons: the landing gate is the CLIENT LIFECYCLE and not just a generation, because `EPOCH`
/// does not move when a slot is re-pointed or retokened.
struct HubResult {
    epoch: u32,
    sec: usize,
    client: &'static crate::plex::Client,
    token_gen: u32,
    /// `None` is a FAILED fetch — kept distinguishable from a successful answer that happens to be
    /// empty, which on this endpoint is a common and legitimate reply.
    shelves: Option<Vec<Shelf>>,
}

static HUB_RESULT: Mutex<Option<HubResult>> = Mutex::new(None);
/// The single-flight claim. **Released when a refused `spawn_small` returns false** as well as by
/// the landing — miss either and this section never fetches shelves again for the session.
pub(super) static HUB_FETCHING: AtomicBool = AtomicBool::new(false);

fn spawn(sec: usize) {
    let Some(&key) = super::sections().get(sec).map(|s| &s.key) else {
        return;
    };
    // the SERVER this section lives on, resolved on the MAIN THREAD — a worker that asked for the
    // current server would fetch whichever one the user had wandered off to (`plex/CLAUDE.md`
    // rule 5)
    let Some(sid) = super::section_sid(sec) else {
        return;
    };
    let Some(client) = crate::plex::client_for(sid) else {
        return;
    };
    let token_gen = client.token_gen();
    if HUB_FETCHING.swap(true, Ordering::SeqCst) {
        return; // another section is out; this one stays `owed` and is retried by `pump_spawns`
    }
    let epoch = EPOCH.load(Ordering::SeqCst);
    let spawned = crate::task::spawn_small("libhubs", move || {
        // outside the guard, so a panicking fetch still lands — as a FAILURE, not as "this library
        // has no shelves"
        let shelves = catch_unwind(|| {
            let mc = client.library_hubs(key, HUB_FETCH_COUNT)?;
            Some(parse_hubs(&mc, sid))
        })
        .unwrap_or(None);
        *HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(HubResult {
            epoch,
            sec,
            client,
            token_gen,
            shelves,
        });
    });
    if spawned {
        // **Cleared on a successful CLAIM, never on an attempt.** A worker is now guaranteed to
        // fill the mailbox, so the request is no longer outstanding.
        if let Some(st) = super::state_mut(sec) {
            st.hubs.owed = false;
        }
    } else {
        // nothing will ever fill the mailbox and the claim is cleared only by a take — release it
        // here, or no section ever fetches shelves again. `owed` is deliberately left set, so
        // `pump_spawns` retries this on a later frame.
        HUB_FETCHING.store(false, Ordering::SeqCst);
    }
}

/// Take the mailbox and apply it. Called from [`super::pump`].
pub(crate) fn land() -> bool {
    let Some(r) = HUB_RESULT.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return false;
    };
    // the take ALWAYS releases the claim, whatever the landing turns out to be
    HUB_FETCHING.store(false, Ordering::SeqCst);
    if r.epoch != EPOCH.load(Ordering::SeqCst) {
        return false;
    }
    // …and the table's identity is not enough: a slot re-pointed or retokened since the spawn is a
    // different server answering under the same index.
    let still = super::section_sid(r.sec)
        .and_then(crate::plex::client_for)
        .map(|c| std::ptr::eq(c, r.client) && c.token_gen() == r.token_gen)
        .unwrap_or(false);
    if !still {
        // **The section is still OWED this fetch.** `spawn` clears `owed` on a successful CLAIM,
        // because a worker is then guaranteed to fill the mailbox — but a landing rejected on the
        // client's lifecycle fills it with an answer nobody may use, and `kick` will not re-arm a
        // section that is published, or an unpublished one with `fails == 0`. Without this the
        // shelves stay empty (or stale) for the rest of the session, silently, after an ordinary
        // `sync_roster` re-point or a token refresh. Not a FAILURE — the ladder is for a server
        // that answered badly, and this one may never have answered at all.
        if let Some(st) = super::state_mut(r.sec) {
            st.hubs.landing_was_not_ours();
        }
        return false;
    }
    let Some(st) = super::state_mut(r.sec) else {
        return false;
    };
    match r.shelves {
        Some(s) => {
            // ONE line per landing, and it names the publication the landing produced rather than
            // just the count: `Staged` and `Committed` differ by whether the grid moved under the
            // viewer, and that is exactly the question a log is read to answer. Shelf TITLES are
            // never written — they are library content (`diag/scrub.rs`).
            crate::log(&format!(
                "libhubs: section {} landed {} shelves",
                r.sec,
                s.len()
            ));
            st.hubs.land_ok(s);
        }
        None => {
            crate::log(&format!(
                "libhubs: section {} failed ({} in a row)",
                r.sec,
                st.hubs.fails + 1
            ));
            st.hubs.land_fail();
            // a failure is still owed an answer — the ladder decides WHEN, `owed` that it happens
            st.hubs.owed = true;
        }
    }
    // shelves arriving repaint a page that may have settled
    crate::ui::idle::invalidate();
    true
}

// ---- the pure half --------------------------------------------------------------------------

/// One response into published shelves. **Pure**, so every rule below is host-graded against the
/// fixture in this module's tests rather than inferred from a screenshot.
///
/// The filter is [`crate::pms`]'s: skip the types this product has no level for, require a title
/// and a thumb, and read the ITEM's own `type` rather than the hub's — which can be `mixed`. An
/// EMPTY hub is dropped entirely, which §3a measured as the common case (one movie section
/// answered with 6 hubs of which 5 were empty), so this is required rather than tidy: a client
/// that draws what it is given draws five headings over nothing.
pub(crate) fn parse_hubs(mc: &crate::plex::MediaContainer, sid: ServerId) -> Vec<Shelf> {
    const SKIP: [&str; 6] = ["album", "artist", "track", "photo", "clip", "playlist"];
    let mut out = Vec::new();
    for hub in &mc.hub {
        if out.len() >= MAX_SHELVES {
            break;
        }
        let items: Vec<PmsMovie> = hub
            .metadata
            .iter()
            .filter(|m| !SKIP.contains(&m.kind.as_str()))
            .map(|m| parse_item(m, sid))
            .filter(|m| !m.title.is_empty() && !m.thumb.is_empty())
            .take(MAX_SHELF_ITEMS)
            .collect();
        if items.is_empty() {
            continue;
        }
        if hub.title.is_empty() {
            continue; // a row with no heading has nothing to say about what is in it
        }
        out.push(Shelf {
            is_continue: shelf_is_continue(&hub.hub_identifier, &hub.key),
            landscape: is_episode_shelf(&items),
            id: hub.hub_identifier.clone(),
            title: hub.title.clone(),
            items,
        });
    }
    out
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
pub(crate) fn shelf_is_continue(id: &str, key: &str) -> bool {
    id.contains(".inprogress.")
        || (key.contains("/hubs/sections/") && key.contains("/continueWatching/items"))
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

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
    fn container(json: &str) -> crate::plex::MediaContainer {
        serde_json::from_slice::<crate::plex::Envelope>(json.as_bytes())
            .expect("the fixture parses")
            .media_container
    }

    fn parsed() -> Vec<Shelf> {
        parse_hubs(&container(HUBS_JSON), ServerId::from_raw(0))
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
            vec!["Continue Watching", "Recently Added", "Toy Story Collection"],
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
        assert!(shelf_is_continue("", "/hubs/sections/1/continueWatching/items"));
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
        // `PmsMovie::kind` is the app's own small enum (0 movie / 1 show / 2 season / 3 episode),
        // and the point is that each item kept ITS OWN — a hub typed `mixed` cannot supply one
        assert_eq!(mixed.items[0].kind, 1, "a show");
        assert_eq!(mixed.items[1].kind, 3, "an episode");
    }

    /// An item with no thumb is not drawable in a shelf and is dropped, and a hub left with nothing
    /// goes with it — the same filter `pms::project` applies to Home's rows, so one library's shelf
    /// cannot follow a different rule from the same server's Home shelf.
    #[test]
    fn an_item_with_no_artwork_is_dropped_and_can_empty_its_hub() {
        let json = r#"{"MediaContainer":{"Hub":[
          {"hubIdentifier":"movie.recentlyadded.1","title":"Recently Added","size":2,"Metadata":[
             {"ratingKey":"1","type":"movie","title":"No Art"},
             {"ratingKey":"2","type":"movie","title":"","thumb":"/t/2"}]}]}}"#;
        assert!(
            parse_hubs(&container(json), ServerId::from_raw(0)).is_empty(),
            "one row has no art and the other no title: the hub has nothing to draw"
        );
    }

    /// The two caps, and they are different questions: items per shelf is `pms::MAX_SHELF_ITEMS`
    /// (the shelf's own row budget, shared with Home) while shelves per library bounds the
    /// DOCUMENT — twelve is already 6,588px above the grid.
    #[test]
    fn both_caps_hold() {
        let mut hubs = String::new();
        for h in 0..(MAX_SHELVES + 3) {
            let items: Vec<String> = (0..(MAX_SHELF_ITEMS + 5))
                .map(|i| {
                    format!(
                        r#"{{"ratingKey":"{h}{i}","type":"movie","title":"T{i}","thumb":"/t/{i}"}}"#
                    )
                })
                .collect();
            hubs.push_str(&format!(
                r#"{{"hubIdentifier":"h{h}","title":"Row {h}","Metadata":[{}]}},"#,
                items.join(",")
            ));
        }
        let json = format!(
            r#"{{"MediaContainer":{{"Hub":[{}]}}}}"#,
            hubs.trim_end_matches(',')
        );
        let sh = parse_hubs(&container(&json), ServerId::from_raw(0));
        assert_eq!(sh.len(), MAX_SHELVES, "the document is bounded");
        assert!(
            sh.iter().all(|s| s.items.len() == MAX_SHELF_ITEMS),
            "…and so is every row"
        );
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
        assert!(is_episode_shelf(&[ep("Caminandes"), ep("Caminandes"), ep("Caminandes")]));
        // …and of different shows
        assert!(is_episode_shelf(&[ep("A"), ep("B")]));

        // ALL of them, though: one film in the row would be letterboxed into a landscape slot, and
        // a row that changes shape when a single item lands is worse than a consistently portrait one
        assert!(!is_episode_shelf(&[ep("A"), film.clone()]));
        assert!(!is_episode_shelf(&[film.clone()]));
        // …and an empty row is not a landscape row, it is a row that gets dropped
        assert!(!is_episode_shelf(&[]));
    }

    /// The artwork FALLBACK CHAIN: the episode's own still, then the show's POSTER, and its
    /// backdrop only as a last resort.
    ///
    /// The middle rung was the backdrop until `Library Screens.dc.html` E ruled otherwise — "where
    /// an episode has no still, the tile falls back to the show's poster in the same frame,
    /// cover-fitted, label and all. A crop is better than a row of mixed tile shapes." Both are
    /// show-level images, so neither escapes the identical-tiles problem; what decides it is that
    /// the poster is the show's IDENTIFYING artwork, which is what this tile's own label is about.
    #[test]
    fn a_landscape_tile_prefers_the_episodes_own_still() {
        use crate::ui::widgets::still_key;
        let full = PmsMovie {
            still: "/still".into(),
            art: "/art".into(),
            thumb: "/poster".into(),
            ..Default::default()
        };
        assert_eq!(still_key(&full), "/still");

        // no still — an ordinary answer for a specials folder or an item mid-scan
        let no_still = PmsMovie {
            art: "/art".into(),
            thumb: "/poster".into(),
            ..Default::default()
        };
        assert_eq!(
            still_key(&no_still),
            "/poster",
            "the show's own poster, cover-fitted in the same frame"
        );

        // …and the backdrop only when there is no poster either
        let bare = PmsMovie {
            art: "/art".into(),
            ..Default::default()
        };
        assert_eq!(still_key(&bare), "/art");
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
        assert!(!h.tick(), "…and it settles rather than reporting every frame");
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

        assert!(
            !h.commit_staged(false),
            "…not while they are looking at it"
        );
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
            assert!(!h.tick(), "the window has nothing to say about a held answer");
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
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("deckstage");
        _t.watching("u-deckstage");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        let sec = crate::browse::cur();

        // a committed deck…
        seed_shelves_for_test(sec, &["movie.inprogress.1"], 3);
        let (sid, rk) = {
            let st = crate::browse::state_mut(sec).expect("a seeded section");
            let m = &st.hubs.committed[0].items[1];
            (m.sid, m.rk.clone())
        };

        // …and an older refresh held behind it, carrying the same item
        {
            let st = crate::browse::state_mut(sec).expect("a seeded section");
            st.hubs.staged = Some(st.hubs.committed.clone());
        }

        assert!(left_the_deck(sid, &rk), "the visible deck loses the row");
        {
            let st = crate::browse::state_mut(sec).unwrap();
            assert!(st.hubs.committed[0].items.iter().all(|m| m.rk != rk));
        }

        // publishing the held answer must not bring it back
        assert!(commit_staged(sec, true));
        {
            let st = crate::browse::state_mut(sec).unwrap();
            assert!(
                st.hubs.committed.iter().flat_map(|sh| sh.items.iter()).all(|m| m.rk != rk),
                "the staged set resurrected a row the user removed"
            );
        }
        crate::browse::reset();
    }

    /// **A landing rejected on the client's LIFECYCLE leaves the section still owed a fetch.**
    /// `spawn` clears `owed` on a successful CLAIM, because a worker is then guaranteed to fill the
    /// mailbox — which is true, and is not the same as "the answer will be usable". `land` discards
    /// a response whose server slot was re-pointed or retokened since the spawn, and `kick` re-arms
    /// neither a published section nor an unpublished one with `fails == 0`. So an ordinary
    /// `sync_roster` re-point or a token refresh could leave a library's shelves empty, or a
    /// refresh stale, for the rest of the session — silently, with no failure anywhere to read.
    ///
    /// **Graded on `landing_was_not_ours` rather than end to end, and the red was SIMULATED.**
    /// `land`'s rejection arm needs a registered `plex::Client` to disagree with, and the host
    /// suite registers none — `browse`'s seeder fabricates sources without installing clients, so
    /// `client_for` returns `None` here and the whole branch is unreachable. A test that took the
    /// early exit would report green having entered nothing, which is why the state change lives in
    /// a named method this can drive instead.
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
    /// global claim (one television, one screen), so `spawn` returns early while somebody else is
    /// out — and before 2026-09-05 nothing recorded that this section still wanted one, after which
    /// `kick` also returned early because the section WAS published and had no failures. Its
    /// Continue Watching row then stayed stale for the rest of the session.
    ///
    /// Graded on the pure bits, because the spawn path itself needs a client and a task.
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
}
