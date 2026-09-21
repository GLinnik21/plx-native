//! The physically owned Search model and fetch transport (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`SearchStore`]; no free selector can connect two Bridges.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

pub(crate) use crate::search::view::SearchSnapshot;

#[derive(Clone, Debug)]
pub(crate) enum SearchCmd {
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
    SetWatchedLocal { sid: crate::plex::ServerId, rk: String, on: bool },
}

/// One Search owner: logical state, the worker adapter all current fetches capture, and notice.
pub(crate) struct SearchStore {
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

    pub(crate) fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub(crate) fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    /// Capture the store publication at the dispatcher frame boundary, not during paint.
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> SearchSnapshot {
        self.state.snapshot()
    }

    pub(crate) fn snapshot_with_directory(
        &self,
        directory: crate::stores::browse::DirectoryView<'_>,
    ) -> SearchSnapshot {
        self.state.snapshot_with_directory(directory)
    }

    #[cfg(test)]
    pub(crate) fn query(&self) -> &str {
        self.state.query()
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> crate::search::State {
        self.state.state()
    }

    #[cfg(test)]
    pub(crate) fn query_gen(&self) -> u32 {
        self.state.query_gen()
    }

    /// Synchronous addressed command path. Reset rotates the adapter before clearing state, so an
    /// old worker can only finish into the retired mailbox it captured.
    #[cfg(test)]
    pub(crate) fn run(&mut self, cmd: SearchCmd) -> bool {
        if matches!(&cmd, SearchCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let answer = self.state.run(&self.adapter, cmd);
        self.bump();
        answer
    }

    /// Synchronous command path with the Browse owner publication captured by the application.
    /// Query admission snapshots its favourite-library ranking from this directory.
    pub(crate) fn run_with_directory(
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
    pub(crate) fn pump_with_directory_and_gate(
        &mut self,
        dt: f32,
        directory: crate::stores::browse::DirectoryView<'_>,
        gate: &crate::ui::landgate::Gate,
    ) -> bool {
        let changed = self.state.pump_with_directory_and_gate(&self.adapter, dt, directory, gate);
        if changed {
            self.bump();
        }
        changed
    }

    /// Test-only compatibility pump for fixtures without a retained directory.
    #[cfg(test)]
    pub(crate) fn pump(&mut self, dt: f32) -> bool {
        let changed = self.state.pump(&self.adapter, dt);
        if changed {
            self.bump();
        }
        changed
    }

    #[cfg(test)]
    pub(crate) fn publish_shelves_for_test(&mut self, shelves: Vec<crate::search::Shelf>) {
        self.state.publish_shelves_for_test(shelves);
        self.bump();
    }

    #[cfg(test)]
    pub(crate) fn settling(&self) -> bool {
        self.state.settling()
    }

    #[cfg(test)]
    pub(crate) fn debounce_elapsed_for_test(&self) -> f32 {
        self.state.debounce_elapsed_for_test()
    }

    #[cfg(test)]
    pub(crate) fn adapter_for_test(&self) -> Arc<crate::search::SearchAdapter> {
        Arc::clone(&self.adapter)
    }
}
