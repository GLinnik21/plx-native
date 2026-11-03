//! The physically owned Search model and fetch transport (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`SearchStore`]; no free selector can connect two Bridges.
//!
//! The store starts no worker. Its pump emits each source's fetch as a
//! [`StoreJob::Search`](super::StoreJob::Search) and a recents edit emits its write as a
//! [`StoreJob::SaveRecents`](super::StoreJob::SaveRecents), both on [`SearchStore::take_jobs`]; the
//! `Bridge` takes them in the same call and the application's runner starts them
//! (`docs/sans-io.md`, "The Search lane").

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

pub use crate::search::view::SearchSnapshot;

#[derive(Clone, Debug)]
pub enum SearchCmd {
    /// The field's text; a change of the TRIMMED terms supersedes the answer and restarts the
    /// debounce, a change of whitespace only repaints.
    SetQuery(String),
    /// An owned draft cannot submit into a replacement profile's Search store.
    SetQueryScoped { profile_generation: u32, query: String },
    /// A submitted search, not each keystroke; history is scoped to the active profile.
    RememberRecent { profile_generation: u32, term: String },
    ClearRecents { profile_generation: u32 },
    /// Sign-out / profile switch: drop the query and every shelf.
    Reset,
    /// The optimistic half of a view-state change, on the result shelves.
    ApplyView { at: crate::viewstate::ViewAt, edit: crate::viewstate::ViewEdit },
    /// Slide a row's window half a window on (`before` false) or back. Every hit of the row is
    /// reached by repeating it; the cards move when the sources that must read have answered.
    /// `seen` is the row's window in the view the ask was computed from, its start
    /// (`Shelf::window.start`) and the cards it held: a frame captures its views before its
    /// landings are delivered, so the ask can come from a window the store has since slid or
    /// filled, and such an ask is refused (where focus stood among the preview's twelve cards says
    /// nothing about the twenty-four that replaced them). The new window's publication differs
    /// from `seen`, which lets the screen ask again from it.
    Page { kind: crate::search::Kind, before: bool, seen: (usize, usize) },
    /// Withdraw the slide `Page` asked for on a row: focus left the edge it was asked from, and the
    /// window moving now would take the card it stands on out of it. `seen` is the window start the
    /// slide was asked from; a slide that has since landed makes this a no-op.
    PageCancel { kind: crate::search::Kind, seen: usize },
}

/// One Search owner: logical state, the worker adapter all current fetches capture, and notice.
pub struct SearchStore {
    state: crate::search::SearchState,
    adapter: Arc<crate::search::SearchAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for SearchStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl SearchStore {
    /// Join the owner's view-state publish order (`Stores::view_ledger`).
    pub(crate) fn share_ledger(&mut self, ledger: &crate::viewledger::ViewLedger) {
        self.state.ledger = ledger.clone();
    }

    fn bump(&self) -> u32 {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    /// Capture the store publication at the dispatcher frame boundary, not during paint.
    #[cfg(any(test, feature = "test-support"))]
    pub fn snapshot(&self) -> SearchSnapshot {
        self.state.snapshot()
    }

    /// `household`: the signed-in profile's household member ids, read by the application's
    /// `Bridge` and handed in (`SearchState::snapshot_with_directory`).
    pub fn snapshot_with_directory(
        &self,
        directory: crate::stores::browse::DirectoryView<'_>,
        household: &[i64],
    ) -> SearchSnapshot {
        self.state.snapshot_with_directory(directory, household)
    }

    /// The ITEMS the result shelves hold, in shelf order. A tag or collection hit has no rating
    /// key of its own to read back, so only the media cards count.
    pub fn held_keys(&self) -> super::HeldKeys {
        let mut keys = super::HeldKeys::new();
        for item in self.state.shelves().iter().flat_map(|shelf| shelf.items.iter()) {
            if let crate::search::Item::Media(m) = item {
                super::push_held(&mut keys, m.sid, &m.rk);
            }
        }
        keys
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn query(&self) -> &str {
        self.state.query()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn state(&self) -> crate::search::State {
        self.state.state()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn query_gen(&self) -> u32 {
        self.state.query_gen()
    }

    /// Synchronous addressed command path. Reset rotates the adapter before clearing state, so an
    /// old worker can only finish into the retired mailbox it captured.
    #[cfg(any(test, feature = "test-support"))]
    pub fn run(&mut self, cmd: SearchCmd) -> bool {
        if matches!(&cmd, SearchCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let view_edit = matches!(&cmd, SearchCmd::ApplyView { .. });
        let answer = self.state.run(&self.adapter, cmd);
        // An `ApplyView` that matched no tile raises no notice; every other command does.
        if answer || !view_edit {
            self.bump();
        }
        answer
    }

    /// Synchronous command path with the Browse owner publication captured by the application.
    /// Query admission snapshots its favourite-library ranking from this directory.
    pub fn run_with_directory(
        &mut self,
        cmd: SearchCmd,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> bool {
        if matches!(&cmd, SearchCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let view_edit = matches!(&cmd, SearchCmd::ApplyView { .. });
        let answer = self.state.run_with_directory(&self.adapter, cmd, directory);
        if answer || !view_edit {
            self.bump();
        }
        answer
    }

    /// Route-unconditional landing/request pass for this owner's adapter. `keys` is the registry's
    /// answer as of this call. It does NOT start the requests it emitted: the caller takes them with
    /// [`SearchStore::take_jobs`] and hands them to a runner in the same turn (the one production
    /// caller is `Bridge::search_pump`; the application's test rigs fail a frame that ends with a
    /// job still here, `Bridge::assert_no_store_jobs_left_for_test`).
    pub fn pump_with_directory_and_gate(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
        gate: &plx_machine::landgate::Gate,
        keys: &plx_plex::plex::ClientKeys,
    ) -> bool {
        let changed = self.state.pump_with_directory_and_gate(&self.adapter, dt, directory, gate, keys);
        if changed {
            self.bump();
        }
        changed
    }

    /// Take the jobs the last step has emitted since the last take, in order. The caller OWES each
    /// one to a runner: a job dropped here is a fetch whose claim is never released.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> {
        std::mem::take(&mut self.state.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.state.jobs.is_empty()
    }

    /// Test-only compatibility pump for fixtures without a retained directory. The requests it
    /// emits wait on the outbox for the caller to take.
    #[cfg(any(test, feature = "test-support"))]
    pub fn pump(&mut self, dt: f32) -> bool {
        let changed = self.state.pump(&self.adapter, dt);
        if changed {
            self.bump();
        }
        changed
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn publish_shelves_for_test(&mut self, shelves: Vec<crate::search::Shelf>) {
        self.state.publish_shelves_for_test(shelves);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn publish_shelves_over_directory_for_test(&mut self, shelves: Vec<crate::search::Shelf>,
        directory: crate::stores::browse::DirectoryView<'_>) {
        self.state.publish_shelves_over_directory_for_test(shelves, directory);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn settling(&self) -> bool {
        self.state.settling()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn debounce_elapsed_for_test(&self) -> f32 {
        self.state.debounce_elapsed_for_test()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> Arc<crate::search::SearchAdapter> {
        Arc::clone(&self.adapter)
    }
}

#[cfg(test)]
mod view_edit_notice_tests {
    use super::*;
    use crate::search::{Item, Kind, Shelf};
    use crate::viewstate::{ViewAt, ViewEdit};

    /// An `ApplyView` that matched no tile raises no notice: the server's `Facts`/`Offset` that
    /// agree with what is drawn move nothing, and a store that bumped anyway would relayout every
    /// mounted consumer for it. A change that does reach a tile still raises one.
    #[test]
    fn a_view_edit_that_changed_nothing_raises_no_notice() {
        let _g = plx_base::testlock::serial();
        let sid = plx_plex::plex::ServerId::UNSET;
        let mut store = SearchStore::default();
        store.publish_shelves_for_test(vec![Shelf {
            epoch: 0, window: Default::default(), kind: Kind::Movie,
            items: vec![Item::Media(crate::pms::PmsMovie {
                sid, rk: "1".into(), unwatched: true, resume_ms: 20_000, ..Default::default()
            })],
        }]);
        let _ = store.take_notice();
        let facts = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0, sort: None };
        let apply = |store: &mut SearchStore, rk: &str| {
            store.run(SearchCmd::ApplyView { at: ViewAt::leaf(sid, rk), edit: facts })
        };
        assert!(apply(&mut store, "1"));
        assert!(store.take_notice().is_some(), "a changed tile is news");
        assert!(!apply(&mut store, "1"));
        assert_eq!(store.take_notice(), None, "facts that agree with the tile raise no notice");
        assert!(!apply(&mut store, "elsewhere"));
        assert_eq!(store.take_notice(), None);
    }
}
