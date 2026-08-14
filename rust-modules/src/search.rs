//! search — the search screen's data layer (the store `ui/search/` draws).
//!
//! **STUB.** The types and the query state are real, so the whole screen can be built and
//! host-tested against them; [`pump`] lands nothing yet. The fetch body arrives with the store
//! unit, and the shape it must take is written down here rather than left to be rediscovered.
//!
//! ## The endpoint, and what it actually returns
//!
//! `GET /hubs/search?query=…&limit=…` ([`crate::plex::Client::search`], written long before this
//! screen and dead until now). The spec says it "is intended to be very fast, and called as the
//! user types", which is the design this store is built for. Three things were measured against
//! PMS 1.43.3 rather than taken from the spec, each of which decides something here:
//!
//! - **A one-character query returns every hub empty.** So [`MIN_QUERY`] is 2, and the first
//!   keystroke of every search costs no round trip at all.
//! - **Hub ORDER moves per query** — `sta` ranks people first, `star` ranks films first. So the
//!   shelf order here is FIXED ([`KINDS`]) and ranking is honoured only *inside* a shelf.
//!   Reordering shelves per keystroke would move the row under the user's focus while they type.
//! - **Items arrive in two different containers** — see [`crate::plex::Hub::directory`]. That is
//!   why [`Item`] has two variants instead of being one struct.
//!
//! ## Multi-source
//!
//! Search is single-server: `/hubs/search` answers for the machine you asked, and
//! `docs/shared-servers.md` states that nothing aggregates server-side — the merge is the client's
//! job. So this fans out one query per [`crate::plex::server_ids`] and merges into the shelves
//! below, which is why every [`Item`] carries its own `ServerId`.
//!
//! ## The idiom to copy
//!
//! `person.rs`, not `browse.rs`: generation + a **monotone** mailbox write + `supersede` + a
//! per-fetch in-flight flag and retry backoff, pumped once a frame. As-you-type guarantees
//! overlapping workers, and a monotone mailbox is what stops a slow answer for `wal` repopulating
//! the results for `wallace`. Debounce is `ui/detail.rs`'s `season_settle` accumulator. Two rules
//! that are easy to miss and both wedge the screen forever if missed: release the in-flight flag
//! when `spawn_small` REFUSES, and call [`crate::ui::idle::invalidate`] on every landing including
//! the failure branch.
#![allow(dead_code)]

use crate::plex::ServerId;
use crate::pms::PmsMovie;

/// Below this many characters the server answers with nothing, so asking is pure latency.
/// Measured, not guessed — see the module doc.
pub(crate) const MIN_QUERY: usize = 2;

/// The typed shelves, in the order they are drawn — **fixed**, never the server's ranking.
/// `Search Screen.dc.html`: "Results are ranked inside a shelf, never across them."
pub(crate) const KINDS: [Kind; 5] = [Kind::Movie, Kind::Show, Kind::Episode, Kind::Person, Kind::Collection];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Movie,
    Show,
    Episode,
    /// Cast **and** crew: the server splits these into an `actor` hub and a `director` hub, and
    /// the design draws one shelf. Merging them here rather than in the UI keeps "what is a
    /// shelf" a data question.
    Person,
    Collection,
}

impl Kind {
    /// The shelf heading.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Kind::Movie => "Movies",
            Kind::Show => "TV Shows",
            Kind::Episode => "Episodes",
            Kind::Person => "Cast & Crew",
            Kind::Collection => "Collections",
        }
    }
    /// The count read-out beside it. People are counted as people; everything else as results.
    pub(crate) fn count_word(self, n: usize) -> &'static str {
        match (self, n) {
            (Kind::Person, 1) => "person",
            (Kind::Person, _) => "people",
            (_, 1) => "result",
            (_, _) => "results",
        }
    }
    /// Which hub identifiers feed this shelf.
    pub(crate) fn hubs(self) -> &'static [&'static str] {
        match self {
            Kind::Movie => &["movie"],
            Kind::Show => &["show"],
            Kind::Episode => &["episode"],
            Kind::Person => &["actor", "director"],
            Kind::Collection => &["collection"],
        }
    }
}

/// A person or collection result: the `Directory[]` shape, which carries no `ratingKey` and — for
/// a collection — no artwork either. Kept distinct from [`PmsMovie`] rather than flattened into
/// it, because the two open different screens and a struct with half its fields permanently empty
/// invites code that forgets which half it is holding.
///
/// It is a projection of [`crate::plex::Tag`] — the SAME record the detail page's cast row is
/// built from, which is what lets a search hit be handed to the person page unchanged (and lets
/// `Tag::is_person` match one against a credit).
#[derive(Clone, Default)]
pub(crate) struct TagHit {
    pub(crate) sid: ServerId,
    pub(crate) name: String,
    /// plex.tv's global person guid — the only portable identity here, and the only id
    /// `discover.provider.plex.tv` answers to. Empty on a collection.
    pub(crate) tag_key: String,
    /// The server-local numeric tag id, as a string. Dense from 1 and meaningless off this server.
    pub(crate) id: String,
    /// Absolute for a person (`metadata-static.plex.tv`, so it needs the PMS photo transcoder),
    /// empty for a collection.
    pub(crate) thumb: String,
    /// The listing this tag opens — `/library/sections/1/all?collection=6068`.
    pub(crate) key: String,
    /// How many items carry it, for the caption line.
    pub(crate) count: i64,
}

#[derive(Clone)]
pub(crate) enum Item {
    /// A movie, show or episode: the ordinary card DTO, so every existing tile path draws it
    /// unchanged — resume bar, watched mark, ambient blur and all.
    Media(PmsMovie),
    Tag(TagHit),
}

impl Item {
    pub(crate) fn title(&self) -> &str {
        match self {
            Item::Media(m) => &m.title,
            Item::Tag(t) => &t.name,
        }
    }
    pub(crate) fn sid(&self) -> ServerId {
        match self {
            Item::Media(m) => m.sid,
            Item::Tag(t) => t.sid,
        }
    }
}

pub(crate) struct Shelf {
    pub(crate) kind: Kind,
    pub(crate) items: Vec<Item>,
}

/// What the screen should be saying, which is not the same question as "are there items".
///
/// The distinction `browse.rs` had to learn the hard way: `Ready` with nothing in it is an ANSWER
/// (the server has no match) and reads as "No results"; `Failed` is a fault and reads as one. An
/// empty store alone cannot tell them apart.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// No query, or one below [`MIN_QUERY`]. Nothing has been asked, so nothing is pending.
    Idle,
    /// A query is settling or in flight and no answer for it has arrived yet.
    Searching,
    /// The server answered. Possibly with nothing.
    Ready,
    /// The request failed. Whatever is in the store is the PREVIOUS answer and is left alone.
    Failed,
}

static mut QUERY: String = String::new();
static mut SHELVES: Vec<Shelf> = Vec::new();
static mut STATE: State = State::Idle;

/// The query as typed, verbatim — trailing space and all, because the FIELD draws this.
pub(crate) fn query() -> &'static str {
    unsafe { &*std::ptr::addr_of!(QUERY) }
}

/// Replace the query. Idempotent on an unchanged string, so a caller may hand it every frame.
pub(crate) fn set_query(q: &str) {
    unsafe {
        let cur = &mut *std::ptr::addr_of_mut!(QUERY);
        if cur == q {
            return;
        }
        cur.clear();
        cur.push_str(q);
        // A new query invalidates the old answer immediately. Leaving the previous shelves up
        // while the next lands would show results for a string that is no longer on screen.
        (*std::ptr::addr_of_mut!(SHELVES)).clear();
        *std::ptr::addr_of_mut!(STATE) = if q.trim().chars().count() < MIN_QUERY { State::Idle } else { State::Searching };
    }
    crate::ui::idle::invalidate();
}

pub(crate) fn state() -> State {
    unsafe { *std::ptr::addr_of!(STATE) }
}

/// The shelves, already in [`KINDS`] order, with empty ones omitted — an empty type draws nothing
/// at all, so the UI never has to test for it.
pub(crate) fn shelves() -> &'static [Shelf] {
    unsafe { &*std::ptr::addr_of!(SHELVES) }
}

/// Advance the debounce and land whatever arrived. Called once a frame from the screen's update.
/// Returns whether anything changed, so the caller can re-clamp focus.
pub(crate) fn pump(_dt: f32) -> bool {
    false
}

/// Drop everything — the account changed, so both the query and the results belong to someone
/// else. Called beside `browse::reset()`.
pub(crate) fn reset() {
    unsafe {
        (*std::ptr::addr_of_mut!(QUERY)).clear();
        (*std::ptr::addr_of_mut!(SHELVES)).clear();
        *std::ptr::addr_of_mut!(STATE) = State::Idle;
    }
}
