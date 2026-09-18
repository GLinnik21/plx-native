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
//! Off a `MetadataView` (`store.view()`, borrowing the owner as `&'a`) unless marked otherwise —
//! unqualified free-function names below are `crate::metadata::*`:
//! - `current() -> Option<&'a Detail>` — the loaded item, if any.
//! - `now_playing() -> Option<&'a NowPlaying>` — the HUD/Info-card descriptor of what is
//!   actually playing (may differ from `current()`: a show/season load leaves it untouched).
//! - `playing() -> Option<&'a PlayingItem>` — the playing leaf's own streams/markers/chapters
//!   (`route.rs`'s track menu and skip/Up Next controls read this, not `current()`).
//! - `playing_markers() -> &'a [Marker]`, `playing_chapters() -> &'a [Chapter]` — the
//!   playing item's own lists, unqualified by anything else.
//! - `cached_playing(sid: ServerId, rk: &str) -> Option<PlayingItem>` — an in-memory-only lookup
//!   (checks `current()` alone; never touches the network). Its `fetch_playing_item` NEIGHBOUR
//!   below looks similar and is not this: see the WRITE surface's note on it.
//! - `detail_loading() -> bool`, `season_loading() -> bool` — status flags for a spinner/read-out.
//! - `detail_request_status(sid, rk) -> Option<bool>` — the addressed detail request: `None` for
//!   another target, `Some(true)` while pending, `Some(false)` after success or failure settles.
//! - `active_marker(ps: &route::PlaybackSession) -> Option<Marker>`,
//!   `synthesized_tail_marker(ps: &route::PlaybackSession, has_next: bool) -> Option<Marker>` — the
//!   skip-segment/Up-Next window logic; both also read `player::is_playing`/`playpos_ns`/
//!   `duration_ns` (cross-module reads, still no mutation anywhere). Two free-function neighbours,
//!   `tail_marker(pos_ms: i64, dur_ms: i64) -> Option<Marker>` and `marker_at(markers: &[Marker],
//!   pos_ms: i64) -> Option<Marker>`, take their state explicitly and stay plain `crate::metadata`
//!   functions — no owner to borrow from.
//! - `audio_ordinal(audio: &[Stream], i: usize) -> i32`, `sub_render_ordinal(subs: &[Stream], i:
//!   usize) -> i32` — container-ordinal projections for the track picker; free functions, no state.
//! - `resume_ns(resume_ms: i64, dur_ms: i64) -> i64`, `friendly_codec(codec: &str) -> String` —
//!   pure formatting/policy, no state at all; free functions.
//!
//! **Two names that LOOK like reads and are not, on purpose — read the exclusion, not just the
//! list.** `fetch_playing_item(sid, rk) -> Option<PlayingItem>` performs a BLOCKING `plex::client`
//! network call (`route.rs`'s one caller runs it on the resolve WORKER, never the main thread); a
//! screen must route it through a request/pump, never call it from `step`/`draw`. `sync_now_playing()`
//! mutates `NOW` and its one remaining caller is already inside the WRITE surface below
//! (`install_landed_detail`) — no external caller reaches it directly today, and none should start
//! to; it is internal machinery of the async detail landing, not a third thing a screen names.
//! (Phase 12/D7: `MetadataCmd::LoadDetailNow`/`metadata::load_detail_now` — a synchronous
//! `sync_now_playing` caller of their own — were deleted here. `app/input.rs`'s `activate_card`
//! was the last direct caller; it now issues `RequestDetail` and defers its play-vs-open decision
//! to a landing-driven continuation (`input::menu_play_tick`), so the blocking load has no
//! caller left at all.)
//!
//! ## 2. The WRITE surface — every [`MetadataCmd`] variant, and what it wraps
//!
//! - `RequestDetail{sid, rk}` → `metadata::request_detail` — supersede any in-flight load, fetch
//!   off-thread; lands through the identity-keyed `DETAIL_LANDING` and `pump_detail`.
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
//! route. `MetadataStore::step`'s `StoreEv::Pump` arm below folds all three through the same
//! `note(StoreId::Metadata, …)` wrapper, but nothing ever SENDS a `StoreEv::Pump` to this store in
//! production (only `Bridge`'s owned `BrowseStore` receives one, via `StoreWork::Browse`) — the
//! arm is unreachable there. The actual per-frame callers are direct, unconditional calls in
//! `app/run.rs`'s `update()`: `pump_detail()` and `pump_season()` sit side by side, and
//! `pump_alt_sources_with_directory()` a few lines below. `pump_season()`'s call site was the one
//! of the three that went missing across the phase-7 owned-screens migration (`ui/detail.rs`'s
//! deleted route-gated `update()` used to drain it) and stayed missing — with nothing here to
//! prove it was ever restored — until it was added back beside `pump_detail()` in `run.rs`; see
//! `pump_wiring_tests` below for the regression pin a fixture-less `run.rs` cannot otherwise get.
//!
//! ## 3. `Spot`'s new location and shape
//!
//! `Spot` moved from `ui::detail` to `crate::metadata` (this phase; see `crate::metadata::Spot`'s
//! own doc for the field-by-field rationale). `ui::detail` re-exports it
//! (`pub(crate) use crate::metadata::Spot;`) so no other module's imports moved. Section ids are
//! 0 hero, 1 tabs, 2 episodes, 3 related, 4 cast, 5 about, 6 extras. `saved_col` is indexed by
//! that id, so the seventh slot is extras, not a spare.
//! ```text
//! pub(crate) struct Spot {
//!     pub(crate) section: c_int,       // 0 hero … 6 extras
//!     pub(crate) col: c_int,           // focused item within that section
//!     pub(crate) ep_text: bool,        // episode filmstrip: still (false) vs. its text block (true)
//!     pub(crate) saved_col: [c_int; 7],// per-section focus memory, indexed by section id
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
//! the pre-migration equivalent (`ui::trail::Node::Detail{sid, rk, spot}` — identity plus
//! position, bundled) was EXACTLY that pair with `spot` on the `ReturnState` half: `(sid, rk)` is
//! identity (`ScreenArg`), `spot` is position (`ReturnState`). Keeping `Spot` in tier 3 would put
//! position data on the identity side of that pair, which is the distinction §5.1 exists to draw.
//! **Phase 12 (D1) made that split literal**: the trail is deleted, the identity is
//! `ContentArg::Detail{sid, rk}` and the `Spot` is `PageMemory::Detail`'s, on the entry.
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
//! `a_detail_return_names_the_item_that_was_mounted` is unaffected by any of this — it graded
//! `app::nav::return_page`'s `Node` identity comparison, which never touched `Spot`, and grades
//! the `EntryId` origin that replaced it. It moved with its subject in D1: it is
//! `app::playback::player_return_tests`' now, not `app/mod.rs`'s.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use crate::plex::ServerId;
use crate::ui::machine::{Cx, Effects, Handled, Host, Machine};

use super::StoreEv;

#[derive(Clone)]
pub(crate) enum MetadataCmd {
    /// Supersede any in-flight load and fetch `(sid, rk)` off-thread; lands through `pump_detail`.
    RequestDetail { sid: ServerId, rk: String },
    /// Close the page: drop the item and supersede everything in flight.
    Clear,
    /// The server/profile switch: drop the COMPLETE owned state — including the `now`/`playing`
    /// that `Clear` deliberately spares (D3) — and rotate the worker adapter, so a worker spawned
    /// before the reset can only land into the retired `Arc`, never the replacement's mailbox.
    /// Unlike `Clear`, this one must not leave a previous profile's playback descriptor or track
    /// store reachable from the next profile's Bridge.
    Reset,
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
    /// Install the copies resolved for `(sid, rk)` by the alt-sources worker — the Also-Available
    /// panel's own store, addressed separately from the loaded item because a resolve lands one
    /// round trip later than the page mounts (D3: `metadata::alt_install` had no `Cmd` at all
    /// before this). Production only ever reaches `alt_install` through `pump_alt_sources`
    /// landing a real resolve (same-file call, not this `Cmd`) — this variant exists so tests can
    /// seed the store the way a landed resolve would, without a private-fn escape hatch.
    #[cfg(test)]
    AltInstall {
        sid: ServerId,
        rk: String,
        copies: Vec<crate::metadata::AltCopy>,
    },
    /// Re-read every retained copy's credit from the registry — `metadata::alt_restamp_owners`,
    /// test-only for the same reason as `AltInstall`: production reaches it from
    /// `pump_alt_sources` directly (a facts-epoch move), never as a user command.
    #[cfg(test)]
    AltRestampOwners,
}

/// One Metadata owner: logical state, the worker adapter every current fetch captures, and
/// notice. D3: unlike Hubs' `Reset`, `Clear` must NOT rotate the adapter — a Detail page can be
/// torn down and reopened with an alt-sources resolve still legitimately in flight for it, and
/// the tracker's admission ledger (`metadata::record::Tracker`) lives on this same adapter, so
/// rotating it on every `Clear` would also drop replay's in-flight bookkeeping.
pub(crate) struct MetadataStore {
    state: crate::metadata::MetadataState,
    adapter: Arc<crate::metadata::MetadataAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl MetadataStore {
    fn bump(&self) -> u32 {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub(crate) fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub(crate) fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    pub(crate) fn state(&self) -> &crate::metadata::MetadataState { &self.state }

    /// Test seam: reach this owner's own `MetadataState` to call a `_for_test` helper (e.g.
    /// `crate::metadata::set_current_for_test`) that needs `&mut MetadataState`. Mirrors
    /// `HubsStore::state_mut` (`stores/hubs.rs`).
    #[cfg(test)]
    pub(crate) fn state_mut(&mut self) -> &mut crate::metadata::MetadataState { &mut self.state }

    /// Test seam: state and the OWNING `Arc<MetadataAdapter>` borrowed together, for `_for_test`
    /// helpers (e.g. `crate::metadata::land_detail_for_test`, which forwards into
    /// `pump_detail`'s `install_landed_detail` -> `request_alt_sources`, and that last one
    /// clones the Arc to spawn a resolve worker) — a single `&mut self` split into its two
    /// disjoint fields, not a second way to reach either one.
    #[cfg(test)]
    pub(crate) fn split_for_test(&mut self) -> (&mut crate::metadata::MetadataState, &Arc<crate::metadata::MetadataAdapter>) {
        (&mut self.state, &self.adapter)
    }

    pub(crate) fn adapter_ref(&self) -> &crate::metadata::MetadataAdapter { &self.adapter }

    /// Arms this owner's replay-tracking `Tracker` for controlled-content recording/replay. Call
    /// exactly once, right after construction (`crate::metadata::record::arm`'s own doc has the
    /// full rationale and history) — the one caller is `Bridge::controlled_home`.
    pub(crate) fn arm_detail_tracker(&self, enabled: bool) {
        crate::metadata::record::arm(&self.adapter, enabled);
    }

    /// Synchronous command path over this owner's own state/adapter. D3: no adapter rotation for
    /// `Clear` — see the struct doc. `Reset` (the server/profile switch) DOES rotate, before the
    /// state clears, so a worker spawned before the reset can only land into the retired `Arc` —
    /// mirrors `PersonStore::run`/`SearchStore::run_with_directory`'s `Reset` arm.
    pub(crate) fn run(&mut self, cmd: MetadataCmd) -> bool {
        if matches!(&cmd, MetadataCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let changed = crate::metadata::run(&mut self.state, &self.adapter, cmd);
        self.bump();
        changed
    }

    #[cfg(test)]
    pub(crate) fn adapter_for_test(&self) -> Arc<crate::metadata::MetadataAdapter> {
        self.adapter.clone()
    }

    /// Route-unconditional landing/spawn pass across detail, season and alt-sources — the same
    /// three pumps `Machine::step`'s `StoreEv::Pump` arm already drives.
    pub(crate) fn pump(&mut self) -> bool {
        let detail = self.pump_detail();
        let season = self.pump_season();
        let alt = crate::metadata::pump_alt_sources(&mut self.state, &self.adapter);
        if alt { self.bump(); }
        detail || season || alt
    }

    /// The async detail landing alone — `app/run.rs`'s own call site, pumped before season.
    pub(crate) fn pump_detail(&mut self) -> bool {
        let changed = crate::metadata::pump_detail(&mut self.state, &self.adapter);
        if changed { self.bump(); }
        changed
    }

    /// The async season landing alone — `app/run.rs`'s own call site, pumped after detail.
    pub(crate) fn pump_season(&mut self) -> bool {
        let changed = crate::metadata::pump_season(&mut self.state, &self.adapter);
        if changed { self.bump(); }
        changed
    }

    /// The cross-source alt-sources resolve, scoped by the Bridge's retained Browse directory.
    pub(crate) fn pump_alt_sources_with_directory(
        &mut self,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> bool {
        let changed = crate::metadata::pump_alt_sources_with_directory(&mut self.state, &self.adapter, directory);
        if changed { self.bump(); }
        changed
    }

    /// Borrowed read handle, shaped like `crate::person::PersonStore::view`.
    pub(crate) fn view(&self) -> crate::metadata::MetadataView<'_> {
        crate::metadata::MetadataView::new(self)
    }
}

impl<H: Host> Machine<H> for MetadataStore {
    type Ev = StoreEv<MetadataCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(c) => {
                self.run(c.clone());
            }
            StoreEv::Pump { .. } => {
                self.pump();
            }
        }
        Handled::Yes
    }
}

#[cfg(test)]
mod pump_wiring_tests {
    //! `app/run.rs`'s route-unconditional per-frame block has no host fixture — `App` needs a real
    //! SDL/GL context — so this pins the source text itself, the same idiom
    //! `app::words::focusprobe_player_overlay_delegates_to_the_shared_overlay_word_function` uses
    //! for the identical reason. Why `pump_season()` needs this pin at all: see this module's own
    //! doc comment above, and the commit that added this test.
    #[test]
    fn the_route_unconditional_frame_update_pumps_the_season_landing_after_detail() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/run.rs"),
        )
        .expect("read run.rs");
        // The exact statement shape `pump_detail()`'s own call already has, indentation included:
        // a bare `.contains("pump_season()")` would still pass if the call were wrapped in a
        // route/feature gate, buried behind a route check, or merely mentioned in a comment (none
        // of which pump the mailbox route-unconditionally). Matching by WHOLE LINE (not a
        // substring search, which extra leading indentation would still satisfy) against the
        // literal `if app.bridge.metadata_pump_season() {` at the SAME indentation as
        // `metadata_pump_detail()`'s own `if` proves it is a sibling statement in the same block
        // — not nested one level deeper inside some other conditional.
        //
        // Stage C1 note: these two now name `Bridge`'s own owned-store wrappers
        // (`metadata_pump_detail`/`metadata_pump_season`), not the old free
        // `crate::stores::metadata::pump_detail`/`pump_season` — Metadata moved from a
        // crate-global dispatcher to a per-`Bridge` owner in Stage B, and `run.rs`'s call site
        // moved with it. The pin exists to catch exactly that kind of silent drop, so it must
        // track the real call shape rather than the pre-ownership one.
        const DETAIL_STMT: &str = "        if app.bridge.metadata_pump_detail() {";
        const SEASON_STMT: &str = "        if app.bridge.metadata_pump_season() {";
        let line_index = |needle: &str| {
            src.lines().position(|line| line == needle)
        };
        let detail_at = line_index(DETAIL_STMT)
            .expect("run.rs must still pump the async detail landing every frame, at this exact indentation");
        let season_at = line_index(SEASON_STMT).unwrap_or_else(|| {
            panic!(
                "metadata_pump_season() must be called route-unconditionally, at the same nesting \
                 depth as metadata_pump_detail() (found no `{SEASON_STMT}` line) — its call site \
                 went missing in the phase-7 owned-screens migration and nothing replaced it, so \
                 season_loading() never clears after a season switch: the episode row's spinner \
                 spins forever and every episode press is refused (episodes::action gates on \
                 season_loading())."
            )
        });
        // metadata_pump_detail() must run FIRST: a landed detail's `install_landed_detail` calls
        // `supersede_season()`, invalidating any season fetch for the item being replaced.
        // Pumping season first could apply a stale season landing to CURRENT in the one frame
        // before metadata_pump_detail() replaces it.
        assert!(
            season_at > detail_at,
            "metadata_pump_season() must be pumped AFTER metadata_pump_detail(), not before — \
             metadata_pump_detail() is what supersedes a stale in-flight season fetch when a \
             fresh detail lands"
        );
        // Both statements must be in the SAME enclosing function: no line starting a new `fn` —
        // a new function's own leading `fn`, not the word appearing mid-identifier — between them.
        let no_intervening_fn = src.lines().skip(detail_at + 1).take(season_at - detail_at - 1)
            .all(|line| !line.trim_start().starts_with("fn ")
                && !line.trim_start().starts_with("pub"));
        assert!(
            no_intervening_fn,
            "pump_detail() and pump_season() must be pumped from the same function — found what \
             looks like an intervening function boundary between them"
        );
    }
}

#[cfg(test)]
mod two_owner_tests {
    use super::*;

    /// **The two-owner regression (contract Required 3).** A worker captures the `Arc` of its
    /// owner's `MetadataAdapter` before it spawns (mirrors `HubsStore`'s own two-owner test,
    /// `stores/hubs.rs::a_landing_reaches_only_the_owner_whose_adapter_it_was_minted_from`), and
    /// its landing is applied into a `MetadataState` the caller supplies (`land_detail_for_test`).
    /// Two independently-owned `MetadataStore`s (as two `Bridge`s would be, one per signed-in
    /// session) must not observe each other's requests, landings or notice generations.
    ///
    /// This is deliberately NOT an assertion that would also pass against a shared adapter:
    /// checked by hand (simulated red, not left in the tree — a real historical pre-ownership
    /// commit predates this test harness and no longer builds against it) by routing B's
    /// `begin_detail_for_test` through A's `adapter_ref()` instead of its own, reproducing the
    /// shape a process-wide static or a shared `Arc` would have had before this layer's ownership
    /// port. With that single substitution the very first cross-owner assertion below
    /// (`a.view().detail_request_status(sid, a_rk)`) goes from `Some(true)` to `None`, because B's
    /// `begin` on the shared mailbox silently supersedes A's — this test could not have passed
    /// against the broken shape.
    #[test]
    fn a_landing_reaches_only_the_owner_whose_adapter_it_was_minted_from() {
        let _guard = crate::testlock::serial();
        let sid = crate::plex::ServerId::UNSET;
        let a_rk = "owner-a-item";
        let b_rk = "owner-b-item";

        let mut a = MetadataStore::default();
        let mut b = MetadataStore::default();

        let a_gen = crate::metadata::begin_detail_for_test(a.adapter_ref(), sid, a_rk);
        let b_gen = crate::metadata::begin_detail_for_test(b.adapter_ref(), sid, b_rk);

        // Each owner's own mailbox sees only the request it minted.
        assert_eq!(a.view().detail_request_status(sid, a_rk), Some(true));
        assert_eq!(a.view().detail_request_status(sid, b_rk), None, "A never saw B's request");
        assert_eq!(b.view().detail_request_status(sid, b_rk), Some(true));
        assert_eq!(b.view().detail_request_status(sid, a_rk), None, "B never saw A's request");

        let a_detail = crate::metadata::Detail { sid, rk: a_rk.into(), ..Default::default() };
        let b_detail = crate::metadata::Detail { sid, rk: b_rk.into(), ..Default::default() };

        {
            let (a_state, a_adapter) = a.split_for_test();
            crate::metadata::land_detail_for_test(a_state, a_adapter, sid, a_rk, a_gen, Some(a_detail.clone()));
        }
        {
            let (b_state, b_adapter) = b.split_for_test();
            crate::metadata::land_detail_for_test(b_state, b_adapter, sid, b_rk, b_gen, Some(b_detail.clone()));
        }

        // POSITIVE: each owner's landing settled its own mailbox and installed its own item, and
        // notice generations advanced independently (per-owner `AtomicU32`, not a shared counter).
        assert_eq!(a.view().detail_request_status(sid, a_rk), Some(false), "A's own landing settled A's mailbox");
        assert_eq!(a.view().current().map(|d| d.rk.as_str()), Some(a_rk), "A holds the item it landed");
        assert_eq!(b.view().detail_request_status(sid, b_rk), Some(false), "B's own landing settled B's mailbox");
        assert_eq!(b.view().current().map(|d| d.rk.as_str()), Some(b_rk), "B holds the item it landed");
        // Notice generations are independent counters, not a shared one: a command run against A
        // alone must bump only A's `gen()`. `land_detail_for_test` above drives the bare
        // `crate::metadata::pump_detail` free function directly (a test seam, not `MetadataStore`'s
        // own `run`/`pump_detail` wrapper), so it does not exercise `bump()` — a real command does.
        assert_eq!(a.gen(), 0);
        assert_eq!(b.gen(), 0);
        a.run(MetadataCmd::Clear);
        assert_eq!(a.gen(), 1, "A's own command must advance A's own notice generation");
        assert_eq!(b.gen(), 0, "A's command must not advance B's notice generation");
    }
}
