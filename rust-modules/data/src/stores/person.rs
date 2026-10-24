//! The physically owned Person model and fetch transport (`docs/stores-as-machines.md`). Each
//! production `Bridge` owns one [`PersonStore`]; no free selector can connect two Bridges.
//!
//! The store starts no worker. Its pump emits each request as a [`StoreJob`](super::StoreJob) that
//! its owner takes and owes to a runner (`Stores::person_pump`'s caller, `Bridge::person_pump`),
//! and the part of the registry it decides from, which client each server holds, arrives as
//! [`ClientKeys`], filled by the host right before the call.

use plx_plex::plex::{ClientKeys, ServerId};
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use super::StoreEv;

#[derive(Clone, Debug)]
pub enum PersonCmd {
    /// Mount a person from the header a cast row handed in; the next pump emits the fetches as jobs.
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
    /// The optimistic half of a view-state change, on the person's shelves.
    ApplyView { at: crate::viewstate::ViewAt, edit: crate::viewstate::ViewEdit },
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

    pub fn view(&self) -> crate::person::PersonView<'_> {
        self.state.view()
    }

    /// The items the page for `arg` shows (every shelf), or nothing when the store holds a different
    /// person. The store is single-current, so the identity check keeps a covered Person page from
    /// naming the items of the one that replaced it.
    pub fn held_keys(&self, arg: &super::content_arg::ContentArg) -> super::HeldKeys {
        use super::content_arg::ContentArg;
        let mut keys = super::HeldKeys::new();
        let Some(p) = self.state.view().current() else { return keys };
        let same = match arg {
            ContentArg::Person { .. } => arg.same_item(&ContentArg::Person {
                sid: p.sid, key: p.key.clone(), guid: p.guid.clone(), name: String::new(), thumb: String::new() }),
            ContentArg::Filmography { sid, key } => plx_plex::plex::same_item((p.sid, &p.key), (*sid, key)),
            _ => false,
        };
        if !same { return keys; }
        for m in p.shelves.iter().flat_map(|shelf| shelf.items.iter()) {
            super::push_held(&mut keys, m.sid, &m.rk);
        }
        keys
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

    /// Route-unconditional landing pass for this owner's adapter, and the requests the open page
    /// wants now. `keys` is the registry's answer as of this call. It does NOT start the requests
    /// it emitted: the caller takes them with [`PersonStore::take_jobs`] and hands them to a runner
    /// in the same turn (the one production caller is `Bridge::person_pump`; the application's test
    /// rigs fail a frame that ends with a job still here,
    /// `Bridge::assert_no_store_jobs_left_for_test`).
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate, keys: &ClientKeys) -> bool {
        let changed = self.state.pump_with_gate(&self.adapter, gate, keys);
        if changed {
            self.bump();
        }
        changed
    }

    /// Take the jobs the pump has emitted since the last take, in the order they were emitted. The
    /// caller OWES each one to a runner: a job dropped here is a request whose claim is never
    /// released.
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> {
        std::mem::take(&mut self.state.jobs)
    }

    /// Does the outbox hold a job nobody has taken yet?
    pub fn has_jobs(&self) -> bool {
        !self.state.jobs.is_empty()
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
    pub fn install_roles_for_test(&mut self, kind: usize, roles: Vec<String>) {
        self.state.install_roles_for_test(kind, roles);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_credits_for_test(&mut self, groups: &[(&str, usize)]) {
        self.state.install_credits_for_test(groups);
        self.bump();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn install_profile_for_test(&mut self) {
        self.state.install_profile_for_test();
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
                // Unreached: nothing sends `StoreEv::Pump`. A future sender must pass the host's
                // keys and drain the jobs the pump emits: these empty keys read as "no server
                // holds a client" (the per-server reads are held off), and the plex.tv reads it
                // does emit would be stranded on the outbox.
                self.pump(&plx_machine::landgate::Gate::default(), &ClientKeys::default());
            }
        }
        Handled::Yes
    }
}
