//! **The Watchlist, application side** (docs/watchlist-plan.md §1.2, §1.4, §2.4 W45D): what feeds
//! the page from the store and what the page's requests do to it.
//!
//! A child of `bridge`, like the test modules, because the feed reads the rig's private fields.
//! The page is a generic screen (`screens::watchlist::page`) that names no store and no sibling;
//! the application owns the whole loop around it:
//!
//! * **Feed.** Once a frame, [`Bridge::watchlist_feed`] finds the mounted page and hands it
//!   what the store holds ([`WatchlistIn`]): the list state, the options, the ordered and
//!   filtered rows, and the matched copy's facts. It sends nothing when the store's generation has
//!   not moved, so a settled page costs a comparison. **While a menu holds the page it sends
//!   nothing at all** ([`page_veiled`](plx_ui::containers::modal::page_veiled)): the page does not
//!   move under the hold menu or the account menu, so a Remove's tile or a re-read waits and lands
//!   with the first frame after the menu has let go. The Sort & Filter menu casts no veil
//!   (`Screen::lets_host_reflow`): its value reaches the page on the frame after the click and the
//!   grid reflows under the open menu. The feed runs at the head of the frame, ahead of the page's
//!   tick (`bridge::frame_ingest`), so a landing begins from the slots the old cards stood in.
//! * **Open.** A page that mounts opens the store (`WatchlistCmd::Open`): the Bridge reads the
//!   account's identity (`Bridge::watchlist_input`) and the store takes it, the list read starts, a populated list is never blanked.
//! * **Window.** The titles near the focus are the store's hold window: the matched copies whose
//!   watch state the application owes a read-back (`Bridge::held_keys`). A window, not a cap: the
//!   focus moves it over every row.
//! * **Presses.** A bookmark press or the hold menu's Remove goes to the store as an intent
//!   ([`Bridge::watchlist_press`]); the store flips at once, a refused or unreachable write puts the
//!   flip back with no toast (Q8), and a debug build refuses the write unless the discover
//!   stand-in or the owner's `plxnative-watchlist-live` switch is armed (Q11,
//!   `plex::watchlist::permit_write`); there, and only in a `devtriggers` build, the page header
//!   says the write was blocked (`WatchlistIn::DevBlocked`).
//! * **Re-read.** A burst of landed writes owes one re-read (`take_reread_owed`), issued two
//!   seconds later so the burst is over.
//!
//! What the application does with the page's requests is `content::watchlist_requests`.
use std::collections::HashMap;

use plx_data::stores::watchlist::{Status, WatchlistCmd};
use plx_data::watchlist::ledger::Intent;
use plx_screens::registry::{AppArg, WatchlistMenuArg};
use plx_screens::watchlist::options::WatchlistOptionsState;
use plx_screens::watchlist::page::{ListState, MatchedFacts, WatchTile, WatchlistIn, WatchlistOut, WatchlistScreen};

use super::*;

/// How long after a burst of landed writes the list is read again.
const REREAD_DELAY_MS: u32 = 2_000;
/// Titles kept in the hold window either side of the focus: three screens of six-wide rows.
const WINDOW_BEHIND: usize = 18;
const WINDOW_AHEAD: usize = 36;

/// What the feed last handed the page, so a frame with nothing new hands it nothing.
#[derive(Default)]
pub(crate) struct Feed {
    /// The mounted page the rest was sent to; a new instance is sent everything again.
    instance: Option<InstanceId>,
    /// The store generation the rows were built from.
    generation: Option<u32>,
    list: Option<ListState>,
    /// The art server as the page was last told (outer `None`: not told yet).
    art_server: Option<Option<plx_plex::plex::ServerId>>,
    options: Option<WatchlistOptionsState>,
    window: Option<std::ops::Range<usize>>,
    /// Facts per guid as the page was last told, to send only what moved.
    facts: HashMap<String, MatchedFacts>,
    /// A tile the user just removed: the page plays its landing before the rows replace it.
    removed: Option<String>,
    reread_at: Option<u32>,
    /// The debug cue as the page was last told (`WatchlistIn::DevBlocked`). Fed only in a
    /// `devtriggers` build.
    #[cfg(feature = "devtriggers")]
    dev_blocked: Option<bool>,
}

/// The membership of `hex` in `view`: member (intents in flight counted), absent from a read list,
/// or not known yet.
pub(super) fn membership_in(view: &plx_data::stores::watchlist::WatchlistView<'_>, hex: &[u8; 12]) -> plx_data::catalog::Membership {
    use plx_data::catalog::Membership;
    if view.is_member(hex) {
        Membership::On
    } else if view.status() == Status::Ready {
        Membership::Off
    } else {
        Membership::Unknown
    }
}

/// Push the catalog page of `hex`, carrying the opener's `ret` when it froze one.
pub(crate) fn open_catalog(
    d: &mut Dispatcher<AppHost>, hex: [u8; 12], ret: Option<ReturnState<u32, PageMemory>>,
) {
    use plx_ui::screen::ScreenArg;
    let arg = Bridge::open_catalog_arg(hex);
    // A second press inside the push's dip-out is inert against its twin.
    if d.nav.tabs.stack.pending_dest().is_some_and(|pending| pending.same_instance(&arg)) {
        return;
    }
    match ret {
        Some(ret) => nav_push_with_return(d, arg, ret),
        None => nav_push(d, arg),
    }
}

/// The catalog id of a `plex://movie|show/<24 hex>` guid.
pub(crate) fn hex_of_guid(guid: &str) -> Option<[u8; 12]> {
    plx_data::watchlist::decode_hex(guid.rsplit('/').next()?)
}

fn list_state(view: &plx_data::stores::watchlist::WatchlistView<'_>) -> ListState {
    match view.status() {
        Status::NoAccount => ListState::Unavailable,
        // The whole list, the pages still to arrive included, so the count beside the title does
        // not climb as they land.
        Status::Ready => ListState::Ready { total: view.list_len() + view.unloaded() },
        // The first read failed and no list had landed (a populated list is never replaced by
        // this): the page-placed read-out with Try again.
        Status::Failed(_) => ListState::Failed,
        // A first read that has not landed.
        Status::Idle | Status::Loading => ListState::Loading,
    }
}

fn matched_facts(facts: plx_data::stores::watchlist::TileFacts, listing: plx_plex::plex::ListingFacts) -> MatchedFacts {
    MatchedFacts {
        thumb: String::new(),
        watched: facts.watched,
        resume: (facts.resume_ms > 0 && facts.dur_ms > 0)
            .then(|| (facts.resume_ms as f32 / facts.dur_ms as f32).clamp(0.0, 1.0)),
        blur: None,
        listing,
    }
}

impl Bridge {
    /// The page mounted on the stack, if any: its instance and the screen itself.
    fn watchlist_page<'a>(d: &'a mut Dispatcher<AppHost>) -> Option<(InstanceId, &'a mut WatchlistScreen)> {
        let id = d.nav.tabs.stack.entries.iter().find(|e| e.arg == AppArg::Watchlist)?.id;
        let inst = d.nav.entry_mut(id)?.inst.as_mut()?;
        let instance = inst.id;
        let screen = inst.screen.as_any_mut()?.downcast_mut::<WatchlistScreen>()?;
        Some((instance, screen))
    }

    /// Once a frame, route-unconditional: open the store for a page that just mounted, hand the
    /// page what changed, keep the hold window on the focus, and spend an owed re-read.
    pub(crate) fn watchlist_feed(&mut self, d: &mut Dispatcher<AppHost>) {
        let store = &mut self.stores.watchlist;
        let feed = &mut self.watchlist;
        let now = crate::app::clock::now();
        // The app's logical clock (ms, wrapping), not a wall-clock read: due once `now` passes `at`.
        if feed.reread_at.is_some_and(|at| now.wrapping_sub(at) as i32 >= 0) {
            feed.reread_at = None;
            store.run(WatchlistCmd::Reread);
        }
        if store.take_reread_owed() {
            feed.reread_at = Some(now.wrapping_add(REREAD_DELAY_MS));
        }
        let Some((instance, screen)) = Self::watchlist_page(d) else {
            // Popped: the next page starts from nothing, and the store's hold window is empty.
            if feed.instance.take().is_some() {
                *feed = Feed { reread_at: feed.reread_at, ..Feed::default() };
                store.run(WatchlistCmd::Window { wanted: 0..0 });
            }
            return;
        };
        if feed.instance != Some(instance) {
            *feed = Feed { instance: Some(instance), reread_at: feed.reread_at, ..Feed::default() };
            store.run_with_input(WatchlistCmd::Open, &Self::watchlist_input());
        }
        // The window follows the focus (the page's own cursor, a reserved cell past the loaded
        // tail included), over the whole list.
        let focus = screen.cursor();
        let wanted = focus.saturating_sub(WINDOW_BEHIND)..focus.saturating_add(WINDOW_AHEAD);
        if feed.window.as_ref() != Some(&wanted) {
            feed.window = Some(wanted.clone());
            store.run(WatchlistCmd::Window { wanted });
        }
        // **The page does not move while a menu that veils it is up** (opening, open or closing:
        // until the menu has let go of the host). What the store has landed, a Remove's tile
        // included, waits here: the screen is told once the menu is gone, so the grid's commit and
        // its landing start on the same frame and nothing under the menu is redrawn. Sort & Filter
        // casts no veil and is fed at once.
        if plx_ui::containers::modal::page_veiled() {
            return;
        }
        let generation = store.gen();
        let view = store.view();
        let mut changed = false;
        let art_server = view.art_server();
        if feed.art_server != Some(art_server) {
            feed.art_server = Some(art_server);
            screen.input(WatchlistIn::ArtServer(art_server));
            changed = true;
        }
        let options = WatchlistOptionsState::from_specs(view.sort(), view.filter());
        if feed.options != Some(options) {
            feed.options = Some(options);
            screen.input(WatchlistIn::Options(options));
            changed = true;
        }
        if feed.generation != Some(generation) {
            feed.generation = Some(generation);
            if let Some(guid) = feed.removed.take() {
                screen.input(WatchlistIn::Removed(guid));
                // The page lowered its own count; the store's is the truth, so say it again.
                feed.list = None;
            }
            let tiles: Vec<WatchTile> = (0..view.len())
                .filter_map(|i| Some(WatchTile { row: view.row(i)?.clone(), state: view.state(i)?, copy: view.copy(i) }))
                .collect();
            let mut now_facts = HashMap::new();
            for (i, tile) in tiles.iter().enumerate() {
                // A matched copy lends its watch state once the store has learnt it, and its
                // listing row's caption facts as soon as the lookup answered.
                let (watch, listing) = (view.facts(i), view.listing(i));
                if watch.is_some() || listing.is_some() {
                    now_facts.insert(tile.row.guid.clone(), matched_facts(watch.unwrap_or_default(), listing.cloned().unwrap_or_default()));
                }
            }
            screen.input(WatchlistIn::Rows(tiles));
            // A tile whose facts left the store (its copy went) is told nothing; one that moved
            // or arrived is told once.
            for (guid, facts) in &now_facts {
                if feed.facts.get(guid) != Some(facts) {
                    screen.input(WatchlistIn::Facts(guid.clone(), facts.clone()));
                }
            }
            feed.facts = now_facts;
            changed = true;
        }
        // After the rows: a removal lowers the page's own count by one, and the store's is the truth.
        let list = list_state(&view);
        if feed.list != Some(list) {
            feed.list = Some(list);
            screen.input(WatchlistIn::List(list));
            changed = true;
        }
        // DEBUG BUILDS ONLY: say on the page that the dev-build failsafe refused the last write.
        #[cfg(feature = "devtriggers")]
        {
            let blocked = plx_plex::plex::watchlist::last_write_refused();
            if feed.dev_blocked != Some(blocked) {
                feed.dev_blocked = Some(blocked);
                screen.input(WatchlistIn::DevBlocked(blocked));
                changed = true;
            }
        }
        if changed {
            plx_machine::idle::invalidate();
        }
    }

    /// **A title put on the Watchlist from anywhere** (a menu row, the Detail disc, the catalog
    /// pill): the write is queued like any press, and the strip's bookmark mark pulses
    /// (`ChromeSnapshot::pulse`, app-owned: no screen asks for it). Only an Add pulses; a removal
    /// has nothing to fly to the tab.
    pub(crate) fn watchlist_add(&mut self, hex: [u8; 12]) {
        self.watchlist_press(hex, Intent::Add);
        self.chrome.pulse(plx_ui::widgets::TAB_KEY_WATCHLIST);
    }

    /// What the catalog page's pill asked for (`AppFx::Catalog`): the same write the item menu's rows
    /// make, so an Add pulses the tab here too. The page flips nothing itself; the ledger does.
    pub(crate) fn catalog_effect(&mut self, out: plx_screens::catalog::CatalogOut) {
        use plx_screens::catalog::CatalogOut;
        match out {
            CatalogOut::Add { hex } => self.watchlist_add(hex),
            CatalogOut::Remove { hex } => self.watchlist_press(hex, Intent::Remove),
        }
    }

    /// A bookmark press (or the hold menu's Remove) on the catalog id `hex`.
    pub(crate) fn watchlist_press(&mut self, hex: [u8; 12], intent: Intent) {
        self.stores.watchlist.run(WatchlistCmd::Press { hex, intent });
        plx_machine::idle::invalidate();
    }

    /// Read the list again (and with it the matches): Try again on the unreachable read-out, and
    /// a resume or a long cover of a live Watchlist page (`reread.rs`).
    pub(crate) fn watchlist_reread(&mut self) {
        self.stores.watchlist.run(WatchlistCmd::Reread);
        plx_machine::idle::invalidate();
    }

    /// Try again on the unreachable read-out.
    pub(crate) fn watchlist_retry(&mut self) {
        self.watchlist_reread();
    }

    /// A Remove from a menu or the Detail disc: flip at once. `from_page` is true only when the
    /// Watchlist page itself is what the press was made on (`watchlist_page_on_top`): then the page
    /// plays its removal landing. From any other page (a Detail pushed over a retained Watchlist
    /// page, a card elsewhere) the page is told nothing, so a title added back before the page is
    /// uncovered is never faded out as removed.
    pub(crate) fn watchlist_remove(&mut self, guid: &str, from_page: bool) -> bool {
        let Some(hex) = hex_of_guid(guid) else { return false };
        self.watchlist_press(hex, Intent::Remove);
        if from_page {
            self.watchlist.removed = Some(guid.to_owned());
        }
        true
    }

    /// Is the Watchlist page the page the user is on (the newest stack entry)? A menu is a modal
    /// over its host and leaves the stack alone, so this names the host of a menu's action.
    pub(crate) fn watchlist_page_on_top(d: &Dispatcher<AppHost>) -> bool {
        d.nav.tabs.stack.entries.last().is_some_and(|e| e.arg == AppArg::Watchlist)
    }

    /// The Sort & Filter menu's value reaches the store (client-side ordering: no request).
    pub(crate) fn watchlist_set_options(&mut self, options: &WatchlistOptionsState) {
        let (sort, filter) = options.specs();
        self.stores.watchlist.run(WatchlistCmd::Sort(sort));
        self.stores.watchlist.run(WatchlistCmd::Filter(filter));
        plx_machine::idle::invalidate();
    }

    /// The menu argument for the Sort & Filter surface over the page `host`: the store's current
    /// value and the pill the panel morphs out of.
    pub(crate) fn watchlist_menu_arg(&self, d: &mut Dispatcher<AppHost>, host: EntryId) -> Option<WatchlistMenuArg> {
        let view = self.stores.watchlist.view();
        let options = WatchlistOptionsState::from_specs(view.sort(), view.filter());
        let (_, screen) = Self::watchlist_page(d)?;
        let pill = screen.pill_rect(&self.measure);
        Some(WatchlistMenuArg { host, options, anchor: [pill.x, pill.y, pill.w, pill.h].map(f32::to_bits) })
    }

    /// The hold menu's subject and anchor: the tile the page held, by guid (never by index alone,
    /// the list may have moved since the press), and the rect it was drawn at.
    pub(crate) fn watchlist_hold_arg(
        &self, d: &mut Dispatcher<AppHost>, entry: EntryId, focus: Option<FocusKey<u32>>,
        guid: &str, state: plx_data::watchlist::model::TileState,
        copy: Option<plx_data::watchlist::model::ServerMatch>,
    ) -> Option<plx_screens::registry::ItemMenuArg> {
        let rect = focus.filter(|key| key.entry == entry).and_then(|key| {
            let screen = &d.nav.entry(entry).filter(|e| e.arg == AppArg::Watchlist)?.inst.as_ref()?.screen;
            let parts = CxParts { tick: Tick { ms: 0, dt_us: 0 }, press: Default::default(),
                focus: plx_machine::machine::FocusRead { current: Some(key), ..Default::default() },
                owner: InputOwner::Entry(entry) };
            let cx = parts.cx::<AppHost>(self.views(), &self.measure);
            screen.focused_card_rect(&cx, Some(key), At::Drawn)
        });
        let (_, screen) = Self::watchlist_page(d)?;
        let tile = screen.tiles().iter().find(|t| t.row.guid == guid)?;
        let catalog = plx_screens::registry::CatalogRef {
            guid: guid.to_owned(), kind: tile.row.kind, title: tile.row.title.clone(),
        };
        // Plan §1.9: a playable title (and a coming-soon one with a reachable copy, Q6) is a
        // card of that copy plus Remove; any other title gets the catalog rows.
        let playable = matches!(state, plx_data::watchlist::model::TileState::Available | plx_data::watchlist::model::TileState::ComingSoonMatched);
        // A Catalog menu carries no PMS identity at all (§1.9: sid UNSET, rk empty) even when a copy
        // is known, as for a ServerOffline tile: the cached copy must not reach a PMS row.
        let (kind, sid, rk) = match copy.filter(|_| playable) {
            Some(c) => (
                ItemMenuKind::Card { row: Box::new(self.matched_card(&tile.row, c)), from_deck: false },
                c.server_id, c.rating_key.to_string(),
            ),
            None => (ItemMenuKind::Catalog { state }, plx_plex::plex::ServerId::UNSET, String::new()),
        };
        // A Catalog menu's membership row: the tile IS on the list, so it offers Remove.
        let watchlist = match (&kind, hex_of_guid(guid)) {
            (ItemMenuKind::Catalog { .. }, Some(hex)) => Some(plx_screens::item_menu::watchlist::Face {
                hex, member: plx_data::catalog::Membership::On, kind: tile.row.kind, from_episode: false,
            }),
            _ => None,
        };
        Some(plx_screens::registry::ItemMenuArg {
            sid,
            rk,
            kind,
            host: entry,
            focus,
            anchor: anchor_bits(rect),
            loaded_episode: false,
            from_home: false,
            catalog: Some(catalog),
            watchlist,
        })
    }

    /// The catalog row the PMS item menu is built over for a tile whose copy is `copy`: identity
    /// and the watch state the store holds for it (so Mark watched / unwatched read right).
    fn matched_card(
        &self, row: &plx_data::watchlist::model::WatchRow, copy: plx_data::watchlist::model::ServerMatch,
    ) -> plx_data::pms::PmsMovie {
        use plx_data::watchlist::model::Kind;
        let view = self.stores.watchlist.view();
        let facts = (0..view.len())
            .find(|&i| view.row(i).is_some_and(|r| r.guid == row.guid))
            .and_then(|i| view.facts(i))
            .unwrap_or_default();
        plx_data::pms::PmsMovie {
            sid: copy.server_id,
            rk: copy.rating_key.to_string(),
            title: row.title.clone(),
            kind: match row.kind { Kind::Movie => 0, Kind::Show => 1 },
            resume_ms: facts.resume_ms,
            dur_ns: facts.dur_ms.saturating_mul(1_000_000),
            watched: facts.watched,
            unwatched: !facts.watched && facts.resume_ms <= 0,
            ..Default::default()
        }
    }

    /// A page's request as the bridge would have collected it from the dispatcher (tests that drive
    /// `content::watchlist_requests` without the page's key path).
    #[cfg(all(test, feature = "hostsim"))]
    pub(crate) fn push_watchlist_req_for_test(&mut self, from: MachineId, req: WatchlistOut) {
        self.watchlist_reqs.push((from, req, ReturnState::default()));
    }

    pub(crate) fn take_watchlist_reqs(&mut self) -> Vec<(MachineId, WatchlistOut, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.watchlist_reqs)
    }

    /// What the Live TV page asked since the last drain (`content::livetv_requests`).
    pub(crate) fn take_livetv_reqs(&mut self) -> Vec<(MachineId, plx_screens::livetv::LiveTvOut, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.livetv_reqs)
    }

    /// Is this catalog id on the watchlist, intents in flight counted? (The Detail and catalog
    /// bookmark faces read it.)
    #[cfg(test)]
    pub(crate) fn watchlist_member(&self, hex: &[u8; 12]) -> bool {
        self.stores.watchlist.view().is_member(hex)
    }

    /// Read the list once for a page that needs membership (a Detail page, a menu) when nobody has:
    /// `Idle` only, so a failed or empty read is never retried per frame.
    pub(crate) fn watchlist_open_if_idle(&mut self) {
        if self.stores.watchlist.view().status() == Status::Idle {
            self.stores.watchlist.run_with_input(WatchlistCmd::Open, &Self::watchlist_input());
            plx_machine::idle::invalidate();
        }
    }

    /// **The Watchlist subject of an item menu, captured now** ([`plx_screens::item_menu::watchlist::Face`]).
    /// `None` when the title has no catalog id (`hex` unknown) or plex.tv could not be reached (no
    /// account, or the first read failed): the menu then omits its row rather than offer a press that
    /// can only fail. An unread list is `Unknown`, which wears the Add face.
    pub(crate) fn watchlist_face(
        &self, hex: Option<[u8; 12]>, kind: plx_data::watchlist::model::Kind, from_episode: bool,
    ) -> Option<plx_screens::item_menu::watchlist::Face> {
        let hex = hex?;
        let view = self.stores.watchlist.view();
        if matches!(view.status(), Status::NoAccount | Status::Failed(_)) {
            return None;
        }
        Some(plx_screens::item_menu::watchlist::Face {
            hex, member: membership_in(&view, &hex), kind, from_episode,
        })
    }

    /// The face for a PMS card row: a movie or show acts on itself, an episode or a season on its
    /// show (`from_episode` makes the labels say "Show").
    ///
    /// A row that carries no catalog id is resolved through its own server record
    /// (`client.metadata(rk or show_rk).guid`, `WatchlistStore::want_hex`): the read is asked for
    /// here and runs on a worker, so the menu being opened now omits the row and the next one
    /// offers it. A server that did not answer, or an item with no portable id, never does.
    pub(crate) fn watchlist_card_face(
        &self, item: &plx_data::pms::PmsMovie,
    ) -> Option<plx_screens::item_menu::watchlist::Face> {
        let (kind, known, from_episode) = item.watchlist_subject()?;
        let rk = if from_episode { item.show_rk.as_str() } else { item.rk.as_str() };
        let hex = known.or_else(|| {
            if rk.is_empty() || item.sid == plx_plex::plex::ServerId::UNSET {
                return None;
            }
            let store = &self.stores.watchlist;
            match store.view().resolved_hex(item.sid, rk) {
                Some(answer) => answer,
                None => {
                    store.want_hex(item.sid, rk);
                    None
                }
            }
        });
        self.watchlist_face(hex, kind, from_episode)
    }

    /// Once a frame, route-unconditional: the catalog store's landing pass, the catalog page's
    /// feed (open on mount, hand it the record, the membership and the art server), and the
    /// bookmark disc of every Detail page on the stack.
    pub(crate) fn catalog_feed(&mut self, d: &mut Dispatcher<AppHost>) {
        use plx_screens::registry::ContentArg;
        if self.catalog_pump() {
            plx_machine::idle::invalidate();
        }
        let mut catalog: Option<(EntryId, [u8; 12])> = None;
        let mut details: Vec<EntryId> = Vec::new();
        let mut homes: Vec<EntryId> = Vec::new();
        for e in &d.nav.tabs.stack.entries {
            match &e.arg {
                AppArg::Content(ContentArg::Catalog { hex }) => catalog = Some((e.id, *hex)),
                AppArg::Content(ContentArg::Detail { .. }) => details.push(e.id),
                _ if e.inst.as_ref().and_then(|i| i.screen.as_any()).is_some_and(|a| a.is::<plx_screens::home::HomeScreen>()) => homes.push(e.id),
                _ => {}
            }
        }
        for id in details {
            let Some(screen) = d.nav.entry_mut(id)
                .and_then(|e| e.inst.as_mut())
                .and_then(|i| i.screen.as_any_mut())
                .and_then(|a| a.downcast_mut::<plx_screens::detail::DetailScreen>()) else { continue };
            let Some(hex) = screen.watchlist_hex() else { continue };
            screen.set_watchlist(membership_in(&self.stores.watchlist.view(), &hex));
            self.watchlist_open_if_idle();
        }
        // The Home hero's bookmark disc, for the title the hero shows now.
        for id in homes {
            let Some(screen) = d.nav.entry_mut(id)
                .and_then(|e| e.inst.as_mut())
                .and_then(|i| i.screen.as_any_mut())
                .and_then(|a| a.downcast_mut::<plx_screens::home::HomeScreen>()) else { continue };
            let Some(hex) = screen.watchlist_hex() else { continue };
            screen.set_watchlist(hex, membership_in(&self.stores.watchlist.view(), &hex));
            self.watchlist_open_if_idle();
        }
        let Some((id, hex)) = catalog else {
            if self.catalog_seat.take().is_some() {
                self.catalog_reset();
            }
            return;
        };
        let Some(inst) = d.nav.entry_mut(id).and_then(|e| e.inst.as_mut()) else { return };
        let instance = inst.id;
        let Some(screen) = inst.screen.as_any_mut().and_then(|a| a.downcast_mut::<plx_screens::catalog::CatalogScreen>())
            else { return };
        if self.catalog_seat != Some(instance) {
            self.catalog_seat = Some(instance);
            self.catalog_open(hex);
            // The page's membership needs the list: read it again on every mount, like the tab does.
            self.stores.watchlist.run_with_input(WatchlistCmd::Open, &Self::watchlist_input());
        }
        // The status line follows the title's tile on the Watchlist (a match landing or a server
        // going offline moves it), the record's date rule only for a title not on the list read.
        let mut load: plx_screens::catalog::Load = (&self.stores.catalog.load).into();
        if let (plx_screens::catalog::Load::Loaded { status, .. }, Some(tile)) =
            (&mut load, self.stores.catalog.tile_status(&self.stores.watchlist))
        {
            *status = tile.into();
        }
        screen.input(plx_screens::catalog::CatalogViewModel {
            load,
            membership: self.stores.catalog.membership(&self.stores.watchlist).into(),
        });
        screen.set_art_server(self.stores.watchlist.view().art_server());
    }

    /// The catalog page's re-read (`reread.rs`): the list again, which re-matches the title, and
    /// the discover record when it never landed (a loaded record is kept, not blanked).
    pub(crate) fn catalog_reread(&mut self, hex: [u8; 12]) {
        self.watchlist_reread();
        if self.stores.catalog.hex == Some(hex)
            && !matches!(self.stores.catalog.load, plx_data::catalog::Load::Loaded(_))
        {
            self.catalog_open(hex);
        }
    }

    /// Open the catalog page of `hex` (a title no server holds). Shared by the Watchlist tile's OK
    /// and the hold menu's Go to row.
    pub(crate) fn open_catalog_arg(hex: [u8; 12]) -> AppArg {
        AppArg::Content(plx_screens::registry::ContentArg::Catalog { hex })
    }

    #[cfg(test)]
    pub(crate) fn watchlist_for_test(&mut self) -> &mut plx_data::stores::watchlist::WatchlistStore {
        &mut self.stores.watchlist
    }

    #[cfg(test)]
    pub(crate) fn catalog_for_test(&mut self) -> &mut plx_data::stores::catalog::CatalogStore {
        &mut self.stores.catalog
    }

    /// The removal landing the Watchlist page is owed, by guid.
    #[cfg(test)]
    pub(crate) fn removal_owed_for_test(&self) -> Option<&str> {
        self.watchlist.removed.as_deref()
    }

    /// The hold window the feed last sent the store.
    #[cfg(test)]
    pub(crate) fn watchlist_window_for_test(&self) -> Option<std::ops::Range<usize>> {
        self.watchlist.window.clone()
    }
}

/// Present the Sort & Filter surface (a compact menu hung off the header pill).
pub(crate) fn open_watchlist_menu(d: &mut Dispatcher<AppHost>, arg: WatchlistMenuArg) {
    if d.nav.modals.surfaces.iter().any(|s| matches!(s.entry.arg, AppArg::WatchlistMenu(_)) && s.phase != Phase::Hidden) {
        return;
    }
    d.nav.next_style = Style::Compact;
    d.request(MachineId::Nav, NavOp::Present(AppArg::WatchlistMenu(arg)));
}
