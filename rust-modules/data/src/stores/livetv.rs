//! The physical owner of the Live TV model. Its outbox is drained in the same call that steps it.
use crate::livetv::{LiveTvState, LiveTvView};
pub use crate::livetv::LiveTvCmd;
use plx_machine::machine::{Cx, Effects, Handled, Host, Machine};
use plx_plex::plex::ClientKeys;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Default)]
pub struct LiveTvStore {
    state: LiveTvState,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}
impl LiveTvStore {
    fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }
    pub fn gen(&self) -> u32 { self.notice_gen.load(Ordering::Relaxed) }
    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }
    pub fn view(&self) -> LiveTvView<'_> { self.state.view() }
    pub fn run(&mut self, cmd: LiveTvCmd, gate: &plx_machine::landgate::Gate, keys: &ClientKeys, now: i64) -> bool {
        let changed = self.state.run(cmd, gate, keys, now);
        if changed { self.bump(); }
        changed
    }
    pub fn pump(&mut self, gate: &plx_machine::landgate::Gate, keys: &ClientKeys, now: i64) -> bool {
        let changed = self.state.pump(gate, keys, now);
        if changed { self.bump(); }
        changed
    }
    #[must_use]
    pub fn take_jobs(&mut self) -> Vec<super::StoreJob> { std::mem::take(&mut self.state.jobs) }
    pub fn has_jobs(&self) -> bool { !self.state.jobs.is_empty() }
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_for_test(&mut self, channels: Vec<crate::livetv::Channel>) { self.state.install_for_test(channels); self.bump(); }
}
impl<H: Host> Machine<H> for LiveTvStore {
    type Ev = super::StoreEv<LiveTvCmd>;
    fn step(&mut self, _ev: &Self::Ev, _cx: &Cx<'_, H>, _fx: &mut Effects<'_, H>) -> Handled {
        debug_assert!(false, "Live TV requires captured client keys and time from its owner");
        plx_base::eventlog::log("livetv: step without client keys and time — dropped");
        Handled::Yes
    }
}
