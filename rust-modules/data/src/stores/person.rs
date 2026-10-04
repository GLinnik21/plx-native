//! The physically owned Person model and fetch transport (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`PersonStore`]; no free selector can connect two Bridges.

use plx_plex::plex::ServerId;
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;

#[derive(Clone, Debug)]
pub enum PersonCmd {
    /// Mount a person from the header a cast row handed in; the fetches spawn on the next pump.
    Open {
        sid: ServerId,
        key: String,
        guid: String,
        name: String,
        thumb: String,
    },
    Close,
    /// The profile/account switch.
    Reset,
    /// The optimistic half of a view-state write, on the person's shelves.
    SetWatchedLocal { sid: ServerId, rk: String, on: bool },
}

/// One Person owner: logical state, the worker adapter all current requests capture, and notice.
pub struct PersonStore {
    state: crate::person::PersonState,
    adapter: Arc<crate::person::PersonAdapter>,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for PersonStore {
    fn default() -> Self {
        Self {
            state: Default::default(),
            adapter: Arc::new(Default::default()),
            notice_gen: AtomicU32::new(0),
            notice_dirty: AtomicBool::new(false),
        }
    }
}

impl PersonStore {
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

    pub fn view(&self) -> crate::person::PersonView<'_> {
        self.state.view()
    }

    /// Synchronous addressed command path. Reset rotates the adapter before clearing state, so an
    /// old worker can only finish into the retired mailbox it captured.
    pub fn run(&mut self, cmd: PersonCmd) -> bool {
        if matches!(&cmd, PersonCmd::Reset) {
            self.adapter = Arc::new(Default::default());
        }
        let changed = self.state.run(&self.adapter, cmd);
        if changed {
            self.bump();
        }
        changed
    }

    /// Route-unconditional landing/spawn pass for this owner's adapter.
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate) -> bool {
        let changed = self.state.pump_with_gate(&self.adapter, gate);
        if changed {
            self.bump();
        }
        changed
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(
        &mut self,
        movies: Vec<crate::pms::PmsMovie>,
        shows: Vec<crate::pms::PmsMovie>,
    ) {
        self.state.install_for_test(movies, shows);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_source_for_test(
        &mut self,
        sid: ServerId,
        movies: Vec<crate::pms::PmsMovie>,
        shows: Vec<crate::pms::PmsMovie>,
    ) {
        self.state.install_source_for_test(sid, movies, shows);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_credits_for_test(&mut self, groups: &[(&str, usize)]) {
        self.state.install_credits_for_test(groups);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn seed_ownership_fixture_for_test(&mut self) {
        self.state.seed_ownership_fixture_for_test(&self.adapter);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn ownership_fixture_for_test(&self) -> crate::person::OwnershipFixture {
        self.state.ownership_fixture_for_test(&self.adapter)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn late_completion_for_test(&self) -> Box<dyn FnOnce()> {
        self.state.late_completion_for_test(&self.adapter)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn adapter_for_test(&self) -> Arc<crate::person::PersonAdapter> {
        Arc::clone(&self.adapter)
    }
}

impl<H: Host> Machine<H> for PersonStore {
    type Ev = StoreEv<PersonCmd>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            StoreEv::Cmd(command) => {
                self.run(command.clone());
            }
            StoreEv::Pump { .. } => {
                self.pump(&plx_machine::landgate::Gate::default());
            }
        }
        Handled::Yes
    }
}
