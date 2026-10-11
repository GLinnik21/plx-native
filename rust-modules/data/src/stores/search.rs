//! The physically owned Search model and fetch transport (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`SearchStore`]; no free selector can connect two Bridges.

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
    /// The optimistic half of a view-state write, on the result shelves.
    SetWatchedLocal { sid: plx_plex::plex::ServerId, rk: String, on: bool },
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

    pub fn snapshot_with_directory(
        &self,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> SearchSnapshot {
        self.state.snapshot_with_directory(directory)
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
        let answer = self.state.run(&self.adapter, cmd);
        self.bump();
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
        let answer = self.state.run_with_directory(&self.adapter, cmd, directory);
        self.bump();
        answer
    }

    /// Route-unconditional landing/spawn pass for this owner's adapter.
    pub fn pump_with_directory_and_gate(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
        gate: &plx_machine::landgate::Gate,
    ) -> bool {
        let changed = self.state.pump_with_directory_and_gate(&self.adapter, dt, directory, gate);
        if changed {
            self.bump();
        }
        changed
    }

    /// Test-only compatibility pump for fixtures without a retained directory.
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
