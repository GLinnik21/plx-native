//! **Re-reading the pages the viewer is not looking at** (layer-2 view state, L2-5).
//!
//! A page under another page, or a page on screen when the app comes back to the foreground, can
//! be out of date by the time it is seen again: the server moved on while it waited. This module
//! owes each such page ONE bounded re-read, spent only when the network may be used and a recording
//! is not armed:
//!
//! * A page COVERED for [`COVER_FRAMES`] loop iterations, or covered when the resync epoch moves
//!   ([`Reread::resync`]), is re-read once per cover episode. Uncovering a page starts nothing and
//!   forgets the episode.
//! * On foreground ([`Reread::resume`]) the epoch moves, so every covered page follows that rule,
//!   and the VISIBLE page is read back in place: a list page the keys it holds (patches only), a
//!   Detail page its own item, which owes it the obligation-only re-read.
//!
//! "Re-read" is per page kind ([`action`]): the Library family (the grid and its Collections and
//! Recommended pages), Search, Person, Filmography and Collection owe the read-back of the keys they hold (`Stores::owe_read_back`, the layer-1 reconcile worker,
//! never a stop and never an end-of-stream candidate); Home owes one quiet refetch per source
//! (`HubsCmd::RefetchSourceQuiet`); the Watchlist owes its read-back AND the plex.tv list read
//! again, which re-matches it (`Action::WatchlistRead`); a Detail page under the top is told `Deferred`, which the page
//! turns into its own re-read when it owns the Metadata slot again. The read-backs of a covered
//! list page are quiet (they publish facts, they owe no Detail page: `enqueue_reread`).
//!
//! Time is counted in frames (the wall gate forbids clocks here): [`Reread::step`] is called once
//! per loop iteration. What is owed is committed as spent only when `allowed`, so a blocked frame
//! retries on the next one.

use super::*;
use plx_machine::machine::{Delivery, EntryId, Fx, MachineId};
use plx_screens::registry::{AppMsg, ContentArg, DetailRefreshPhase};
use plx_ui::screen::ScreenEvent;

/// Frames a page stays covered before it is re-read. About two seconds of loop iterations.
pub(crate) const COVER_FRAMES: u32 = 120;

/// One page's cover episode.
struct Cover {
    id: EntryId,
    frames: u32,
    /// The resync epoch this page last was current with (when it was covered, or last re-read).
    epoch: u32,
    /// The episode's re-read has been spent.
    spent: bool,
}

/// What [`Reread::step`] decided is owed this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owed {
    /// A page under the top: re-read it.
    Covered(EntryId),
    /// The page on top after a resume: read-back patches only.
    Visible(EntryId),
}

#[derive(Default)]
pub(crate) struct Reread {
    epoch: u32,
    resume_owed: bool,
    covers: Vec<Cover>,
}

impl Reread {
    /// The resync epoch moved (a lost stream, a long gap): every page covered now is owed a re-read.
    /// Layer 2's server-updates stream calls this when it loses its place.
    pub(crate) fn resync(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// The app came back to the foreground: the epoch moves and the visible page is owed patches.
    pub(crate) fn resume(&mut self) {
        self.resync();
        self.resume_owed = true;
    }

    /// One loop iteration. `stack` is the page stack bottom to top; the last entry is visible.
    pub(crate) fn step(&mut self, stack: &[EntryId], allowed: bool) -> Vec<Owed> {
        self.covers.retain(|c| stack.contains(&c.id));
        let mut owed = Vec::new();
        let Some((&top, under)) = stack.split_last() else { return owed };
        // Uncovering ends the episode and starts nothing.
        self.covers.retain(|c| c.id != top);
        for &id in under {
            let at = match self.covers.iter().position(|c| c.id == id) {
                Some(at) => at,
                None => {
                    self.covers.push(Cover { id, frames: 0, epoch: self.epoch, spent: false });
                    self.covers.len() - 1
                }
            };
            let cover = &mut self.covers[at];
            cover.frames = cover.frames.saturating_add(1);
            let due = (cover.frames >= COVER_FRAMES && !cover.spent) || cover.epoch != self.epoch;
            if due && allowed {
                cover.spent = true;
                cover.epoch = self.epoch;
                owed.push(Owed::Covered(id));
            }
        }
        if self.resume_owed && allowed {
            self.resume_owed = false;
            owed.push(Owed::Visible(top));
        }
        owed
    }
}

/// What a page owes when it is re-read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// One quiet refetch per Home source.
    HomeQuiet,
    /// The read-back of the keys the page holds.
    ReadBack,
    /// The Watchlist page: the read-back of its matched copies, and the plex.tv list read again
    /// (which re-matches it), so a title added elsewhere or newly on a server shows up.
    WatchlistRead,
    /// The catalog page: the discover record read again, and the plex.tv list.
    CatalogRead,
    /// Tell a Detail page under the top to re-read when it owns its slot.
    DetailDeferred,
    /// The visible Detail page, after a resume: its own item read back, which owes the page its
    /// in-place re-read (`content::reconcile_details`, obligation only: no focus or scroll move).
    DetailRead,
    Nothing,
}

/// The re-read of the page for `arg`; `covered` says it is under the top.
pub(crate) fn action(arg: &AppArg, covered: bool) -> Action {
    match arg {
        AppArg::Home => Action::HomeQuiet,
        // The Watchlist page reads back the matched copies it holds (`WatchlistStore::held_keys`).
        AppArg::Watchlist => Action::WatchlistRead,
        // The catalog page of a title no server holds owes the re-match for its guid: the plex.tv
        // list read again (a title newly on a server, or added elsewhere, re-derives its status
        // line and bookmark face), and the record itself when the first read did not land.
        AppArg::Content(ContentArg::Catalog { .. }) => Action::CatalogRead,
        AppArg::Search => Action::ReadBack,
        // The grid and the two pages its header pushes over it (Collections, Recommended) are one
        // screen family over one store listing: the same read-back is owed on each
        // (`Bridge::held_keys`). Naming `AppArg::Library` alone left the other two owed nothing.
        arg if arg.is_library_family() => Action::ReadBack,
        AppArg::Content(ContentArg::Person { .. } | ContentArg::Filmography { .. } | ContentArg::Collection(_)) =>
            Action::ReadBack,
        AppArg::Content(ContentArg::Detail { .. }) if covered => Action::DetailDeferred,
        AppArg::Content(ContentArg::Detail { .. }) => Action::DetailRead,
        _ => Action::Nothing,
    }
}

/// Once per loop iteration: decide what is owed and spend it.
pub(super) fn run(
    reread: &mut Reread,
    pages: &mut plx_ui::dispatch::Dispatcher<bridge::AppHost>,
    bridge: &mut bridge::Bridge,
) {
    let stack: Vec<EntryId> = pages.nav.tabs.stack.entries.iter().map(|e| e.id).collect();
    let owed = reread.step(&stack, bridge.reread_allowed());
    for item in owed {
        let (id, covered) = match item { Owed::Covered(id) => (id, true), Owed::Visible(id) => (id, false) };
        let Some(entry) = pages.nav.tabs.stack.entries.iter().find(|e| e.id == id) else { continue };
        let arg = entry.arg.clone();
        let instance = entry.inst.as_ref().map(|i| i.id);
        match action(&arg, covered) {
            Action::HomeQuiet => for sid in bridge.hub_source_ids() {
                let _ = bridge.hubs_run(plx_data::stores::hubs::HubsCmd::RefetchSourceQuiet { sid });
            },
            Action::ReadBack => {
                let keys = bridge.held_keys(&arg);
                bridge.owe_read_back(keys);
            }
            Action::WatchlistRead => {
                let keys = bridge.held_keys(&arg);
                bridge.owe_read_back(keys);
                bridge.watchlist_reread();
            }
            Action::CatalogRead => if let AppArg::Content(ContentArg::Catalog { hex }) = &arg {
                bridge.catalog_reread(*hex);
            },
            Action::DetailDeferred => if let Some(instance) = instance {
                pages.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
                    Delivery::Screen(ScreenEvent::App(AppMsg::DetailReconcile {
                        refresh: DetailRefreshPhase::Deferred,
                    }))));
            },
            Action::DetailRead => if let AppArg::Content(ContentArg::Detail { sid, rk }) = &arg {
                bridge.owe_page_read(vec![(*sid, rk.clone())]);
            },
            Action::Nothing => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_plex::plex::ServerId;

    fn detail(rk: &str) -> AppArg {
        AppArg::Content(ContentArg::Detail { sid: ServerId::UNSET, rk: rk.into() })
    }

    fn ids(n: usize) -> Vec<EntryId> { (1..=n).map(|i| EntryId(i as _)).collect() }

    /// Run `frames` steps over `stack`, collecting what was owed.
    fn steps(r: &mut Reread, stack: &[EntryId], frames: u32, allowed: bool) -> Vec<Owed> {
        (0..frames).flat_map(|_| r.step(stack, allowed)).collect()
    }

    #[test]
    fn short_cover_no_reread() {
        let mut r = Reread::default();
        let stack = ids(2);
        assert!(steps(&mut r, &stack, COVER_FRAMES - 1, true).is_empty(), "one frame short of the cover time");
        assert!(steps(&mut r, &stack[..1], 1, true).is_empty(), "uncovered before it was due");
        assert!(steps(&mut r, &stack, COVER_FRAMES - 1, true).is_empty(), "a new episode starts from zero");
    }

    #[test]
    fn long_cover_rereads_held_keys_once() {
        let mut r = Reread::default();
        let stack = ids(3);
        let owed = steps(&mut r, &stack, COVER_FRAMES * 4, true);
        assert_eq!(owed, [Owed::Covered(stack[0]), Owed::Covered(stack[1])], "each covered page once, the top never");
        // A new cover episode is owed again: the pages came back to the top, then were covered.
        steps(&mut r, &stack[..1], 1, true);
        let again = steps(&mut r, &stack, COVER_FRAMES, true);
        assert_eq!(again, [Owed::Covered(stack[0]), Owed::Covered(stack[1])]);
    }

    #[test]
    fn epoch_move_while_covered_rereads() {
        let mut r = Reread::default();
        let stack = ids(2);
        assert!(steps(&mut r, &stack, 3, true).is_empty());
        r.resync();
        assert_eq!(steps(&mut r, &stack, 1, true), [Owed::Covered(stack[0])], "covered when the epoch moved");
        assert!(steps(&mut r, &stack, COVER_FRAMES * 2 - 1, true).is_empty(), "and it is then spent");
        r.resync();
        assert_eq!(steps(&mut r, &stack, 1, true), [Owed::Covered(stack[0])], "a second move is a second read");
    }

    #[test]
    fn uncover_never_starts_requery() {
        let mut r = Reread::default();
        let stack = ids(2);
        steps(&mut r, &stack, COVER_FRAMES - 1, true);
        assert!(steps(&mut r, &stack[..1], 5, true).is_empty(), "the page came back to the top");
        r.resync();
        assert!(steps(&mut r, &stack[..1], 5, true).is_empty(), "an epoch move owes the visible page nothing");
        assert!(steps(&mut r, &stack, 1, true).is_empty(), "a page covered AFTER the move is current with it");
    }

    #[test]
    fn resume_visible_page_patch_only() {
        let mut r = Reread::default();
        let stack = ids(2);
        steps(&mut r, &stack, 2, true); // the page has been covered for a while
        r.resume();
        let owed = steps(&mut r, &stack, 1, true);
        assert_eq!(owed, [Owed::Covered(stack[0]), Owed::Visible(stack[1])], "covered follow the rule, top is patched");
        assert!(steps(&mut r, &stack, 5, true).is_empty(), "once");
        assert_eq!(action(&AppArg::Home, false), Action::HomeQuiet, "a visible Home refetches quietly");
        assert_eq!(action(&detail("x"), false), Action::DetailRead,
            "a visible Detail is read back in place on resume");
    }

    #[test]
    fn held_while_network_held() {
        let mut r = Reread::default();
        let stack = ids(2);
        r.resume();
        assert!(steps(&mut r, &stack, COVER_FRAMES * 2, false).is_empty(), "nothing is spent while held");
        assert_eq!(steps(&mut r, &stack, 1, true), [Owed::Covered(stack[0]), Owed::Visible(stack[1])],
            "everything owed is spent on the first free frame, once");
        assert!(steps(&mut r, &stack, 1, true).is_empty());
    }

    #[test]
    fn the_per_page_actions() {
        let sid = ServerId::UNSET;
        let detail = AppArg::Content(ContentArg::Detail { sid, rk: "d".into() });
        assert_eq!(action(&detail, true), Action::DetailDeferred);
        assert_eq!(action(&AppArg::Library, true), Action::ReadBack);
        assert_eq!(action(&AppArg::Search, true), Action::ReadBack);
        assert_eq!(action(&AppArg::Watchlist, true), Action::WatchlistRead, "covered Watchlist");
        assert_eq!(action(&AppArg::Watchlist, false), Action::WatchlistRead, "visible Watchlist on resume");
        let catalog = AppArg::Content(ContentArg::Catalog { hex: [7; 12] });
        assert_eq!(action(&catalog, true), Action::CatalogRead, "covered catalog page owes its re-match");
        assert_eq!(action(&catalog, false), Action::CatalogRead, "visible catalog page on resume");
        assert_eq!(action(&AppArg::Player, true), Action::Nothing);
    }

    fn mounted() -> (plx_ui::dispatch::Dispatcher<bridge::AppHost>, bridge::Bridge, u32) {
        (plx_ui::dispatch::Dispatcher::<bridge::AppHost>::new(), bridge::Bridge::for_test(|| 0), 0)
    }

    fn frame(pages: &mut plx_ui::dispatch::Dispatcher<bridge::AppHost>, rig: &mut bridge::Bridge, n: &mut u32) {
        let (_, report) = bridge::frame(pages, rig, plx_machine::machine::Tick { ms: *n * 16, dt_us: 16_000 }, Vec::new());
        *n += 1;
        pages.prune(&report.unmounted);
    }

    fn search_results(rks: &[&str]) -> Vec<plx_data::search::Shelf> {
        vec![plx_data::search::Shelf { epoch: 0, window: Default::default(), kind: plx_data::search::Kind::Movie,
            items: rks.iter().map(|rk| plx_data::search::Item::Media(plx_data::pms::PmsMovie {
                sid: ServerId::UNSET, rk: (*rk).into(), ..Default::default() })).collect() }]
    }

    /// Search (covered by a Detail) owes the read-back of its results after the cover time, and
    /// only while the network is free and no recording is armed.
    #[test]
    fn covered_page_read_back_is_spent_only_when_allowed() {
        let _g = plx_base::testlock::serial();
        let (mut pages, mut rig, mut n) = mounted();
        let mut r = Reread::default();
        bridge::show_page(&mut pages, AppArg::Search);
        frame(&mut pages, &mut rig, &mut n);
        rig.publish_search_for_test(search_results(&["s1", "s2"]));
        bridge::nav_push(&mut pages, detail("d"));
        frame(&mut pages, &mut rig, &mut n);

        rig.hold_network(true);
        for _ in 0..COVER_FRAMES + 5 { run(&mut r, &mut pages, &mut rig); }
        assert!(rig.read_back_pending_for_test().is_empty(), "held_while_network_held: the player is on top");
        rig.hold_network(false);

        rig.landgate().arm_recording();
        for _ in 0..5 { run(&mut r, &mut pages, &mut rig); }
        assert!(rig.read_back_pending_for_test().is_empty(), "off_when_landgate_armed");
        rig.landgate().disarm();

        run(&mut r, &mut pages, &mut rig);
        let keys: Vec<_> = rig.read_back_pending_for_test().into_iter().map(|(_, k)| k).collect();
        assert_eq!(keys, ["s1", "s2"], "spent on the first free frame");
        for _ in 0..COVER_FRAMES { run(&mut r, &mut pages, &mut rig); }
        assert_eq!(rig.read_back_pending_for_test().len(), 2, "and not again in the same episode");
    }

    /// The Detail on top is read back (in place, obligation only) on a resume and not otherwise:
    /// merely being visible, or uncovered, reads nothing.
    #[test]
    fn visible_detail_is_read_back_on_resume_only() {
        let _g = plx_base::testlock::serial();
        let (mut pages, mut rig, mut n) = mounted();
        let mut r = Reread::default();
        bridge::show_page(&mut pages, AppArg::Search);
        frame(&mut pages, &mut rig, &mut n);
        bridge::nav_push(&mut pages, detail("d"));
        frame(&mut pages, &mut rig, &mut n);
        rig.hold_network(true); // keep the covered Search's own re-read out of the way
        for _ in 0..COVER_FRAMES * 2 { run(&mut r, &mut pages, &mut rig); }
        rig.hold_network(false);
        assert!(rig.read_back_pending_for_test().is_empty(), "visible and uncovered: nothing is read");
        r.resume();
        run(&mut r, &mut pages, &mut rig);
        let keys: Vec<_> = rig.read_back_pending_for_test().into_iter().map(|(_, k)| k).collect();
        assert_eq!(keys, ["d"], "the resume reads the visible Detail back");
    }

    /// With no owned server described there is no connection: a frame's pump owes no read-back and
    /// reports no gap, whatever pages are on the stack.
    #[test]
    fn notify_pump_without_an_owned_server_owes_nothing() {
        let _g = plx_base::testlock::serial();
        let (_pages, mut rig, _n) = mounted();
        let held = [ContentArg::Detail { sid: ServerId::UNSET, rk: "d".into() }];
        for _ in 0..5 {
            let (resynced, endpoints) = rig.notify_pump(&held, plx_data::stores::notify_map::Shown::ALL);
            assert!(!resynced, "no owned server, no feed, no gap");
            assert!(endpoints.is_empty());
        }
        assert!(rig.read_back_pending_for_test().is_empty());
    }

    /// A Detail page under the top is told `Deferred`; the top page is untouched.
    #[test]
    fn covered_detail_gets_deferred_reconcile() {
        let _g = plx_base::testlock::serial();
        let (mut pages, mut rig, mut n) = mounted();
        let mut r = Reread::default();
        bridge::show_page(&mut pages, detail("a"));
        frame(&mut pages, &mut rig, &mut n);
        let under = pages.nav.top_page().expect("A mounted").id;
        bridge::nav_push(&mut pages, detail("b"));
        frame(&mut pages, &mut rig, &mut n);
        let top = pages.nav.top_page().expect("B mounted").id;
        let phase = |pages: &plx_ui::dispatch::Dispatcher<bridge::AppHost>, id| pages.nav.entry(id)
            .and_then(|e| e.inst.as_ref()).and_then(|i| i.screen.as_any())
            .and_then(|s| s.downcast_ref::<plx_screens::detail::DetailScreen>())
            .expect("Detail").refresh_for_test();
        for _ in 0..COVER_FRAMES - 1 { run(&mut r, &mut pages, &mut rig); }
        frame(&mut pages, &mut rig, &mut n);
        assert_eq!(phase(&pages, under), DetailRefreshPhase::None, "short of the cover time");
        run(&mut r, &mut pages, &mut rig);
        frame(&mut pages, &mut rig, &mut n);
        assert_eq!(phase(&pages, under), DetailRefreshPhase::Deferred);
        assert_eq!(phase(&pages, top), DetailRefreshPhase::None, "the page on top starts nothing");
    }
}
