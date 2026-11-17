//! Owned hold-learning persistence. UI steps emit typed input lessons; the runner owns
//! durable receipts and request lifetime. No process/thread-local lesson or frame-thread IO.
use plx_machine::machine::HoldHintRead;
use plx_plex::plex::session::HoldHintTaught;
use plx_screens::registry::PreferenceCmd;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}, mpsc::{self, Receiver, TryRecvError}};

struct Pending {
    taught: HoldHintTaught,
    answer: Receiver<bool>,
    live: Arc<AtomicBool>,
}
impl Drop for Pending {
    fn drop(&mut self) { self.live.store(false, Ordering::Release); }
}

enum Write {
    Idle,
    Pending(Pending),
    RetryAt(u32),
}

/// One application runtime's install fact and current profile-tenure request.
pub(crate) struct HintTaught {
    saved: HoldHintTaught,
    scope: Option<u32>,
    write: Write,
}
impl HintTaught {
    pub(crate) fn new(seed: HoldHintRead) -> Self {
        Self { saved: taught(seed), scope: None, write: Write::Idle }
    }

    pub(crate) fn poll(&mut self, scope: u32, read: &mut HoldHintRead, controlled: bool,
        now: u32, mut emit: impl FnMut(PreferenceCmd)) {
        if self.scope != Some(scope) {
            if self.scope.is_some() {
                // Dropping the old receipt cancels queued native work; a late result has no
                // route back to this tenure. Only authoritative install facts cross profiles.
                self.write = Write::Idle;
                *read = HoldHintRead { learned: false, card: self.saved.card, pill: self.saved.pill, context: scope };
            }
            self.scope = Some(scope);
            read.context = scope;
        }
        if controlled {
            self.write = Write::Idle;
            self.saved = self.saved.union(taught(*read));
            return;
        }
        if let Write::Pending(pending) = &self.write {
            match pending.answer.try_recv() {
                Ok(true) => {
                    self.saved = self.saved.union(pending.taught);
                    self.write = Write::Idle;
                }
                Ok(false) | Err(TryRecvError::Disconnected) => self.write = Write::RetryAt(now.wrapping_add(1000)),
                Err(TryRecvError::Empty) => return,
            }
        }
        if let Write::RetryAt(at) = self.write {
            if now.wrapping_sub(at) >= (1 << 31) { return; }
            self.write = Write::Idle;
        }
        let wanted = taught(*read);
        if self.saved.union(wanted) == self.saved { return; }
        let (reply, answer) = mpsc::channel();
        let live = Arc::new(AtomicBool::new(true));
        self.write = Write::Pending(Pending { taught: wanted, answer, live: live.clone() });
        emit(PreferenceCmd::HoldHint { taught: wanted, live, reply });
    }
}
fn taught(read: HoldHintRead) -> HoldHintTaught {
    HoldHintTaught { card: read.card, pill: read.pill }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_machine::machine::HoldHintEvent;

    #[test]
    fn independent_owners_retry_refusal_and_retire_late_receipts_without_cross_learning() {
        let mut a = HoldHintRead::default();
        let mut b = HoldHintRead::default();
        let mut pa = HintTaught::new(a);
        let mut pb = HintTaught::new(b);
        let mut commands = Vec::new();
        pa.poll(1, &mut a, false, 0, |c| commands.push(c));
        pb.poll(1, &mut b, false, 0, |_| panic!("B has no lesson"));
        a.apply(HoldHintEvent::Learned);
        pa.poll(1, &mut a, false, 1, |c| commands.push(c));
        assert_eq!(commands.len(), 1);
        let PreferenceCmd::HoldHint { live, reply, taught } = commands.pop().unwrap() else { panic!() };
        assert_eq!(taught, HoldHintTaught { card: true, pill: true });
        assert_eq!(b, HoldHintRead { context: 1, ..Default::default() });
        reply.send(false).unwrap();
        pa.poll(1, &mut a, false, 2, |_| panic!("refusal backs off"));
        assert!(!live.load(Ordering::Acquire));
        pa.poll(1, &mut a, false, 1001, |_| panic!("not due"));
        pa.poll(1, &mut a, false, 1002, |c| commands.push(c));
        let PreferenceCmd::HoldHint { live, reply, .. } = commands.pop().unwrap() else { panic!() };
        pa.poll(2, &mut a, false, 1003, |_| panic!("retired lesson is not new profile's"));
        assert!(!live.load(Ordering::Acquire));
        assert!(reply.send(true).is_err(), "late receipt has no live owner");
        assert_eq!(a, HoldHintRead { context: 2, ..Default::default() });
        pb.poll(1, &mut b, false, 1004, |_| panic!("A never taught B"));
    }

    #[test]
    fn saved_install_fact_crosses_profiles_once_and_controlled_never_writes() {
        let mut read = HoldHintRead { card: true, ..Default::default() };
        let mut owner = HintTaught::new(read);
        owner.poll(1, &mut read, false, 0, |_| panic!("seed is not news"));
        read.apply(HoldHintEvent::Learned);
        let mut receipt = None;
        owner.poll(1, &mut read, false, 1, |c| receipt = Some(c));
        let Some(PreferenceCmd::HoldHint { reply, .. }) = receipt else { panic!() };
        reply.send(true).unwrap();
        owner.poll(1, &mut read, false, 2, |_| panic!("once only"));
        owner.poll(2, &mut read, false, 3, |_| panic!("saved install fact"));
        assert_eq!(read, HoldHintRead { learned: false, card: true, pill: true, context: 2 });
        read.apply(HoldHintEvent::Learned);
        owner.poll(2, &mut read, true, 4, |_| panic!("controlled IO"));
    }

    #[test]
    fn logical_hint_publishers_cannot_return_but_render_layout_cache_remains() {
        let source = include_str!("../../ui/src/hold_hint.rs");
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for forbidden in ["static LEARNED", "static SHOWN", "static CHANGED", "pub fn mark_learned(",
            "pub fn reset_learned_for_test(", "pub fn seed_taught(", "pub fn take_newly_taught("] {
            assert!(!production.contains(forbidden), "logical publisher resurrected: {forbidden}");
        }
        assert!(production.contains("static LAYOUT:"), "render-only cache remains allowed");
        assert_eq!(production.matches("thread_local!").count(), 1, "only the layout cache may be TLS");
        for logical_cell in ["Cell<bool>", "Cell<u8>", "static mut", "Mutex<HoldHint", "RwLock<HoldHint"] {
            assert!(!production.contains(logical_cell), "logical authority must stay owned: {logical_cell}");
        }
        assert!(production.contains("HoldHintRead"));
        assert!(production.contains("Option<HoldHintEvent>"));
    }

    #[test]
    fn dropping_owner_cancels_queued_request() {
        let mut read = HoldHintRead::default();
        let mut owner = HintTaught::new(read);
        read.apply(HoldHintEvent::ReadCard);
        let mut command = None;
        owner.poll(1, &mut read, false, 0, |c| command = Some(c));
        let Some(PreferenceCmd::HoldHint { live, reply, .. }) = command else { panic!() };
        drop(owner);
        assert!(!live.load(Ordering::Acquire));
        assert!(reply.send(true).is_err());
    }
}
