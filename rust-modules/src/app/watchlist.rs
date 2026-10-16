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
//!   not moved, so a settled page costs a comparison.
//! * **Open.** A page that mounts opens the store (`WatchlistCmd::Open`): the credential is
//!   captured, the list read starts, a populated list is never blanked.
//! * **Window.** The titles near the focus are the store's hold window: the matched copies whose
//!   watch state the application owes a read-back (`Bridge::held_keys`). A window, not a cap: the
//!   focus moves it over every row.
//! * **Presses.** A bookmark press or the hold menu's Remove goes to the store as an intent
//!   ([`Bridge::watchlist_press`]); the store flips at once, a refused or unreachable write puts the
//!   flip back with no toast (Q8), and a debug build refuses the write unless the discover
//!   stand-in is armed (Q11, `plex::watchlist::write_refused`).
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
    options: Option<WatchlistOptionsState>,
    window: Option<std::ops::Range<usize>>,
    /// Facts per guid as the page was last told, to send only what moved.
    facts: HashMap<String, MatchedFacts>,
    /// A tile the user just removed: the page plays its landing before the rows replace it.
    removed: Option<String>,
    reread_at: Option<u32>,
}

/// The catalog id of a `plex://movie|show/<24 hex>` guid.
pub(crate) fn hex_of_guid(guid: &str) -> Option<[u8; 12]> {
    plx_data::watchlist::decode_hex(guid.rsplit('/').next()?)
}

fn list_state(view: &plx_data::stores::watchlist::WatchlistView<'_>) -> ListState {
    match view.status() {
        Status::NoAccount => ListState::Unavailable,
        Status::Ready => ListState::Ready { total: view.list_len() },
        // A first read that has not landed, or failed: the page shows nothing and no read-out
        // (there is no Failed face yet, so a failure reads as a list that never arrives).
        Status::Idle | Status::Loading | Status::Failed(_) => ListState::Loading,
    }
}

fn matched_facts(facts: plx_data::stores::watchlist::TileFacts) -> MatchedFacts {
    MatchedFacts {
        thumb: String::new(),
        watched: facts.watched,
        resume: (facts.resume_ms > 0 && facts.dur_ms > 0)
            .then(|| (facts.resume_ms as f32 / facts.dur_ms as f32).clamp(0.0, 1.0)),
        blur: None,
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
            store.run(WatchlistCmd::Open);
        }
        // The window follows the focus (the page's own cursor), over the whole list.
        let focus = screen.focused().unwrap_or(0);
        let wanted = focus.saturating_sub(WINDOW_BEHIND)..focus.saturating_add(WINDOW_AHEAD);
        if feed.window.as_ref() != Some(&wanted) {
            feed.window = Some(wanted.clone());
            store.run(WatchlistCmd::Window { wanted });
        }
        let generation = store.gen();
        let view = store.view();
        let mut changed = false;
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
                if let Some(facts) = view.facts(i) {
                    now_facts.insert(tile.row.guid.clone(), matched_facts(facts));
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
        if changed {
            plx_machine::idle::invalidate();
        }
    }

    /// A bookmark press (or the hold menu's Remove) on the catalog id `hex`.
    pub(crate) fn watchlist_press(&mut self, hex: [u8; 12], intent: Intent) {
        self.stores.watchlist.run(WatchlistCmd::Press { hex, intent });
        plx_machine::idle::invalidate();
    }

    /// The hold menu's Remove: flip at once and let the page play the removal landing.
    pub(crate) fn watchlist_remove(&mut self, guid: &str) -> bool {
        let Some(hex) = hex_of_guid(guid) else { return false };
        self.watchlist_press(hex, Intent::Remove);
        self.watchlist.removed = Some(guid.to_owned());
        true
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
        Some(plx_screens::registry::ItemMenuArg {
            sid: copy.map_or(plx_plex::plex::ServerId::UNSET, |c| c.server_id),
            rk: copy.map(|c| c.rating_key.to_string()).unwrap_or_default(),
            kind: ItemMenuKind::Catalog { state },
            host: entry,
            focus,
            anchor: anchor_bits(rect),
            loaded_episode: false,
            from_home: false,
            catalog: Some(catalog),
        })
    }

    pub(crate) fn take_watchlist_reqs(&mut self) -> Vec<(MachineId, WatchlistOut, ReturnState<u32, PageMemory>)> {
        std::mem::take(&mut self.watchlist_reqs)
    }

    /// Is this catalog id on the watchlist, intents in flight counted? (The Detail and catalog
    /// bookmark faces read it.)
    #[allow(dead_code)]
    pub(crate) fn watchlist_member(&self, hex: &[u8; 12]) -> bool {
        self.stores.watchlist.view().is_member(hex)
    }

    #[cfg(test)]
    pub(crate) fn watchlist_for_test(&mut self) -> &mut plx_data::stores::watchlist::WatchlistStore {
        &mut self.stores.watchlist
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
