//! The detail page's item, its seasons and the playing item, as a machine over
//! `crate::metadata` (`docs/stores-as-machines.md`).
//!
//! **The Detail store's read/write contract, frozen for restructure phase 7** (spec §13: "7a read/
//! write split of `DetailView`" — this module IS that split; the split is a DOCUMENTED BOUNDARY
//! over `crate::metadata`'s existing functions, not a rewrite of them). This is the barrier: every
//! other phase-7 package (the Detail/Person/Filmography screens, their reconcile ladders) builds
//! against what is written here rather than re-deciding it, exactly as `docs/stores-as-machines.md`
//! is the contract every other store's migration already builds against.
//!
//! ## 1. The READ surface — safe, pure, non-mutating; a screen may call these directly from
//! `step`/`draw`/`Focusable`
//!
//! Off `crate::metadata` (unqualified names below are `crate::metadata::*`):
//! - `current() -> Option<&'static Detail>` — the loaded item, if any.
//! - `now_playing() -> Option<&'static NowPlaying>` — the HUD/Info-card descriptor of what is
//!   actually playing (may differ from `current()`: a show/season load leaves it untouched).
//! - `playing() -> Option<&'static PlayingItem>` — the playing leaf's own streams/markers/chapters
//!   (`route.rs`'s track menu and skip/Up Next controls read this, not `current()`).
//! - `playing_markers() -> &'static [Marker]`, `playing_chapters() -> &'static [Chapter]` — the
//!   playing item's own lists, unqualified by anything else.
//! - `cached_playing(sid: ServerId, rk: &str) -> Option<PlayingItem>` — an in-memory-only lookup
//!   (checks `current()` alone; never touches the network). Its `fetch_playing_item` NEIGHBOUR
//!   below looks similar and is not this: see the WRITE surface's note on it.
//! - `detail_loading() -> bool`, `season_loading() -> bool` — status flags for a spinner/read-out.
//! - `detail_request_status(sid, rk) -> Option<bool>` — the addressed detail request: `None` for
//!   another target, `Some(true)` while pending, `Some(false)` after success or failure settles.
//! - `active_marker() -> Option<Marker>`, `synthesized_tail_marker(has_next: bool) -> Option<Marker>`,
//!   `tail_marker(pos_ms: i64, dur_ms: i64) -> Option<Marker>`, `marker_at(markers: &[Marker], pos_ms:
//!   i64) -> Option<Marker>` — the skip-segment/Up-Next window logic; the first two also read
//!   `player::is_playing`/`playpos_ns`/`duration_ns` (cross-module reads, still no mutation anywhere).
//! - `audio_ordinal(audio: &[Stream], i: usize) -> i32`, `sub_render_ordinal(subs: &[Stream], i:
//!   usize) -> i32` — container-ordinal projections for the track picker.
//! - `resume_ns(resume_ms: i64, dur_ms: i64) -> i64`, `friendly_codec(codec: &str) -> String` —
//!   pure formatting/policy, no state at all.
//!
//! **Two names that LOOK like reads and are not, on purpose — read the exclusion, not just the
//! list.** `fetch_playing_item(sid, rk) -> Option<PlayingItem>` performs a BLOCKING `plex::client`
//! network call (`route.rs`'s one caller runs it on the resolve WORKER, never the main thread); a
//! screen must route it through a request/pump, never call it from `step`/`draw`. `sync_now_playing()`
//! mutates `NOW` and has exactly two callers, both already inside the WRITE surface below
//! (`load_detail_now`, `install_landed_detail`) — no external caller reaches it directly today, and
//! none should start to; it is internal machinery of `LoadDetailNow`/the detail landing, not a
//! third thing a screen names.
//!
//! ## 2. The WRITE surface — every [`MetadataCmd`] variant, and what it wraps
//!
//! - `RequestDetail{sid, rk}` → `metadata::request_detail` — supersede any in-flight load, fetch
//!   off-thread; lands through the identity-keyed `DETAIL_LANDING` and `pump_detail`.
//! - `LoadDetailNow{sid, rk}` → `metadata::load_detail_now` — the BLOCKING load, for a caller that
//!   acts on the item in the same frame (also calls `sync_now_playing` internally).
//! - `Clear` → `metadata::clear` — drop the loaded item, supersede everything in flight.
//! - `LoadSeason(usize)` → `metadata::load_season` — flip the season strip optimistically, fetch
//!   the episodes off-thread (debounced landing through `pump_season`).
//! - `LoadSeasonNow(usize)` → `metadata::load_season_now` — the BLOCKING season load.
//! - `SetNowPlaying(Option<NowPlaying>)` → `metadata::set_now_playing`.
//! - `SetWatchedLocal{sid, rk, on}` → `metadata::set_watched_local` — the optimistic half of a
//!   view-state write, answers whether it actually changed anything.
//! - `InstallPlaying(Option<PlayingItem>)` → `metadata::install_playing` — the playback plan's leaf
//!   (`route.rs`).
//! - `MarkSkipped(Marker)` → `metadata::mark_skipped`.
//! - `RetirePlaying` → `metadata::retire_playing`, `RetirePlayingItem` → `metadata::retire_playing_item`
//!   — see that function's doc for why the two descriptions of "what was playing" must retire
//!   together.
//!
//! Two route-unconditional per-frame landings sit beside `MetadataCmd` rather than inside it,
//! because they are PUMPS (drain a mailbox, install what has landed) rather than requests a screen
//! raises: [`pump_detail`], [`pump_season`], [`pump_alt_sources`] — called every frame regardless of
//! route by `MetadataStore::step`'s `StoreEv::Pump` arm, exactly like every other store.
//!
//! ## 3. `Spot`'s new location and shape
//!
//! `Spot` moved from `ui::detail` to `crate::metadata` (this phase; see `crate::metadata::Spot`'s
//! own doc for the field-by-field rationale, unchanged from its previous home). `ui::detail`
//! re-exports it (`pub(crate) use crate::metadata::Spot;`) so no other module's imports moved. Its
//! shape is exactly what it was:
//! ```text
//! pub(crate) struct Spot {
//!     pub(crate) section: c_int,       // 0 hero, 1 tabs, 2 episodes, 3 related, 4 cast, 5 about
//!     pub(crate) col: c_int,           // focused item within that section
//!     pub(crate) ep_text: bool,        // episode filmstrip: still (false) vs. its text block (true)
//!     pub(crate) saved_col: [c_int; 6],// per-section focus memory
//!     pub(crate) season: Option<i64>,  // the selected season's NUMBER (not its list position)
//! }
//! ```
//! It moved here rather than to `ui::screen` because it is APPLICATION data (Detail-page-shaped),
//! and `ui::screen::ReturnState<K, M>` is a LIBRARY type the layer rule (spec §2.1) forbids from
//! naming it directly — see §4.
//!
//! ## 4. The Spot-vs-ReturnState decision, with rationale
//!
//! **Spot moves onto `ReturnState` — spec §6.1 tier 2 — not tier 3.** The rule tier 3 states (§6.1:
//! "state keyed by a server-side object a worker can append to or renumber lives with that
//! object") does not fit it: `Spot` is not itself renumbered by a refetch (its `season` field is
//! already immune to that, by being a NUMBER rather than a list position — see `Spot`'s own doc),
//! and every tier-3 example the spec names (browse's per-section view, search's query/shelves,
//! metadata's *current item*) is data that OUTLIVES a single page visit and is read back by a
//! DIFFERENT mechanism than "put the page back where it was" (a fresh route entry, not a BACK). A
//! `Spot` has exactly one reader — the same page, restored — which is tier 2's own definition
//! ("what an evicted entry remounts from"). The player's origin makes the same point structurally:
//! spec §5.1 already says the origin is `EntryId` + `Descriptor = (ScreenArg, ReturnState)`, and
//! today's pre-migration equivalent (`ui::trail::Node::Detail{sid, rk, spot}` — identity plus
//! position, bundled) is EXACTLY that pair with `spot` on the `ReturnState` half: `(sid, rk)` is
//! identity (`ScreenArg`), `spot` is position (`ReturnState`). Keeping `Spot` in tier 3 would put
//! position data on the identity side of that pair, which is the distinction §5.1 exists to draw.
//!
//! **The mechanism** (landed this phase, in `ui/machine.rs`/`ui/screen.rs`, NOT this module — this
//! module owns the DECISION and the DATA SHAPE, not the generic plumbing): the layer rule still
//! forbids `ui::screen::ReturnState<K>` from naming `metadata::Spot` directly, so `ReturnState`
//! gained a second, DEFAULTED type parameter — `ReturnState<K, M = ()>` — mirroring `Host::Init`'s
//! existing pattern for crossing the same layer boundary, and a new `Host::Memory` associated type
//! (`Clone + Default + Debug + LogicalState + 'static`) names the one payload type a whole app's heterogeneous
//! screens share (one `NavStack<H, T>` holds every `Entry<H>`, so it cannot carry a different
//! concrete memory type per screen). `Screen<H>::memory_at(focus)` receives the engine's current
//! focus at request-time capture. `AppHost::Memory` is `screens::registry::PageMemory`; Detail
//! contributes `DetailMemory`, containing its `Spot` and stable item-key registry. Person and
//! Filmography contribute their own identity/return payloads. The container hashes these even
//! after eviction, and delivers `RestoreMemory` before `Enter(Restored)` for both live and
//! remounted bodies. Hosts without screen-specific state still use `()`.
//! `a_detail_return_names_the_item_that_was_mounted`
//! (`app/mod.rs`) is unaffected by any of this — it grades `app::nav::return_page`'s `Node`
//! identity comparison, which never touched `Spot` — and must keep passing unmodified.

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::{note, StoreEv, StoreId};

#[derive(Clone)]
pub(crate) enum MetadataCmd {
    /// Supersede any in-flight load and fetch `(sid, rk)` off-thread; lands through `pump_detail`.
    RequestDetail { sid: ServerId, rk: String },
    /// The BLOCKING load, for the callers that act on the item in the same frame.
    LoadDetailNow { sid: ServerId, rk: String },
    /// Close the page: drop the item and supersede everything in flight.
    Clear,
    /// The season strip: flip optimistically, fetch the episodes off-thread.
    LoadSeason(usize),
    /// The BLOCKING season load, for a caller that indexes the episodes in the same frame.
    LoadSeasonNow(usize),
    SetNowPlaying(Option<crate::metadata::NowPlaying>),
    /// The optimistic half of a view-state write on the loaded item, its episodes and Related.
    SetWatchedLocal { sid: ServerId, rk: String, on: bool },
    /// The playback plan's leaf (`route.rs`).
    InstallPlaying(Option<crate::metadata::PlayingItem>),
    MarkSkipped(crate::metadata::Marker),
    RetirePlaying,
    RetirePlayingItem,
}

pub(crate) struct MetadataStore;

/// The shim: step the store NOW through the one vocabulary and answer as the mutator did.
pub(crate) fn apply(cmd: MetadataCmd) -> bool {
    super::apply(super::StoreCmd::Metadata(cmd))
}

/// The store's own step, reached only through [`super::apply`].
pub(super) fn run(cmd: MetadataCmd) -> bool {
    let answer = match cmd {
        MetadataCmd::RequestDetail { sid, rk } => {
            crate::metadata::request_detail(sid, &rk);
            true
        }
        MetadataCmd::LoadDetailNow { sid, rk } => {
            crate::metadata::load_detail_now(sid, &rk);
            true
        }
        MetadataCmd::Clear => {
            crate::metadata::clear();
            true
        }
        MetadataCmd::LoadSeason(i) => {
            crate::metadata::load_season(i);
            true
        }
        MetadataCmd::LoadSeasonNow(i) => {
            crate::metadata::load_season_now(i);
            true
        }
        MetadataCmd::SetNowPlaying(np) => {
            crate::metadata::set_now_playing(np);
            true
        }
        MetadataCmd::SetWatchedLocal { sid, rk, on } => crate::metadata::set_watched_local(sid, &rk, on),
        MetadataCmd::InstallPlaying(p) => {
            crate::metadata::install_playing(p);
            true
        }
        MetadataCmd::MarkSkipped(m) => {
            crate::metadata::mark_skipped(m);
            true
        }
        MetadataCmd::RetirePlaying => {
            crate::metadata::retire_playing();
            true
        }
        MetadataCmd::RetirePlayingItem => {
            crate::metadata::retire_playing_item();
            true
        }
    };
    super::bump(StoreId::Metadata);
    answer
}

/// The three route-unconditional landings the loop runs every frame.
pub(crate) fn pump_detail() -> bool {
    note(StoreId::Metadata, crate::metadata::pump_detail())
}
pub(crate) fn pump_season() -> bool {
    note(StoreId::Metadata, crate::metadata::pump_season())
}
pub(crate) fn pump_alt_sources() -> bool {
    note(StoreId::Metadata, crate::metadata::pump_alt_sources())
}

impl<H: Host> Machine<H> for MetadataStore {
    type Ev = StoreEv<MetadataCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                run(c.clone());
            }
            StoreEv::Pump { .. } => {
                pump_detail();
                pump_season();
                pump_alt_sources();
            }
        }
        Handled::Yes
    }
}
