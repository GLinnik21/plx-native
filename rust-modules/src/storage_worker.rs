//! A bounded FIFO for blocking persistence work, never a lock around application state.
//!
//! Domains assign revisions and submit under their own short coordinator lock. The callback
//! owns disk I/O; a UI caller only polls its ticket. Dropping the writer drains accepted work
//! but does not join: process termination must never be the mechanism that makes a save durable.

use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::OnceLock;

/// Shared persistence queue bound. Domain adapters must submit immutable snapshots and never
/// perform disk I/O inline on the caller.
pub(crate) const CAPACITY: usize = 8;

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubmitError {
    Full,
    Stopped,
    StartFailed,
}

pub(crate) struct TypedTicket<R> {
    result: Receiver<R>,
    worker: Receiver<()>,
}

impl<R> TypedTicket<R> {
    pub(crate) fn try_recv(&self) -> Result<R, TryRecvError> {
        match self.result.try_recv() {
            Err(TryRecvError::Disconnected)
                if matches!(self.worker.try_recv(), Err(TryRecvError::Empty)) =>
            {
                // The operation has started unwinding, but the worker has not yet closed its
                // command receiver. Keep the failure private until a following submit is
                // guaranteed to observe Stopped rather than enqueue behind a dead worker.
                Err(TryRecvError::Empty)
            }
            result => result,
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_blocking(self) -> Result<R, mpsc::RecvError> {
        match self.result.recv() {
            Ok(result) => Ok(result),
            Err(error) => {
                let _ = self.worker.recv();
                Err(error)
            }
        }
    }
}

pub(crate) struct Executor {
    writer: Writer<Job, ()>,
}

static SHARED: OnceLock<Result<Executor, ()>> = OnceLock::new();

pub(crate) fn submit<R: Send + 'static>(
    operation: impl FnOnce() -> R + Send + 'static,
) -> Result<TypedTicket<R>, SubmitError> {
    let executor = match SHARED.get_or_init(|| Executor::start(CAPACITY).map_err(|_| ())) {
        Ok(executor) => executor,
        Err(()) => return Err(SubmitError::StartFailed),
    };
    executor.submit(operation)
}

/// Wait until every job accepted before this call has finished. Test redirects are process-wide;
/// draining before moving one prevents a detached persistence callback from following the next
/// test's root. Production has no exit-time flush promise and never calls this.
#[cfg(test)]
pub(crate) fn drain_for_test() {
    loop {
        match submit(|| ()) {
            Ok(ticket) => {
                let _ = ticket.wait_blocking();
                return;
            }
            Err(SubmitError::Full) => std::thread::yield_now(),
            Err(SubmitError::Stopped | SubmitError::StartFailed) => return,
        }
    }
}

impl Executor {
    pub(crate) fn start(capacity: usize) -> Result<Self, std::io::Error> {
        Self::from_writer(Writer::start("persistence", capacity, |job: Job| job()))
    }

    fn from_writer(
        result: Result<Writer<Job, ()>, std::io::Error>,
    ) -> Result<Self, std::io::Error> {
        result.map(|writer| Self { writer })
    }

    pub(crate) fn submit<R: Send + 'static>(
        &self,
        operation: impl FnOnce() -> R + Send + 'static,
    ) -> Result<TypedTicket<R>, SubmitError> {
        let (reply, result) = mpsc::channel();
        let job: Job = Box::new(move || {
            let _ = reply.send(operation());
        });
        match self.writer.submit_ticket(job) {
            Ok(worker) => Ok(TypedTicket {
                result,
                worker: worker.0,
            }),
            Err(SubmitErrorGeneric::Full(_)) => Err(SubmitError::Full),
            Err(SubmitErrorGeneric::Stopped(_)) => Err(SubmitError::Stopped),
        }
    }

    #[cfg(test)]
    fn start_refused() -> Result<Self, std::io::Error> {
        Self::from_writer(Err(std::io::Error::other("injected worker start refusal")))
    }
}

// Keep the generic Writer's command error distinct from the typed executor's public result.
pub(crate) struct Writer<C, R> {
    sender: SyncSender<(C, mpsc::Sender<R>)>,
}

pub(crate) enum SubmitErrorGeneric<C> {
    Full(C),
    Stopped(C),
}

pub(crate) struct Ticket<R>(Receiver<R>);

impl<R> Ticket<R> {
    #[cfg(test)]
    pub(crate) fn try_recv(&self) -> Result<R, TryRecvError> {
        self.0.try_recv()
    }

    /// Only background callers and tests may wait; the SDL thread polls `try_recv` instead.
    #[cfg(test)]
    pub(crate) fn wait_blocking(self) -> Result<R, mpsc::RecvError> {
        self.0.recv()
    }
}

impl<C, R> Clone for Writer<C, R> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<C: Send + 'static, R: Send + 'static> Writer<C, R> {
    /// Capacity bounds queued commands, plus at most one command executing in the callback.
    pub(crate) fn start(
        name: &str,
        capacity: usize,
        mut handle: impl FnMut(C) -> R + Send + 'static,
    ) -> Result<Self, std::io::Error> {
        if capacity == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "zero queue capacity",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<(C, mpsc::Sender<R>)>(capacity);
        crate::task::spawn(name, move || loop {
            let Ok((command, reply)) = receiver.recv() else {
                break;
            };
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(command))) {
                Ok(result) => {
                    // Dropping a ticket cancels interest, not an already accepted durable write.
                    let _ = reply.send(result);
                }
                Err(panic) => {
                    // Close the command side before the current ticket. Without this explicit
                    // order, unwinding drops `reply` first and leaves a small interval where a
                    // producer can enqueue work behind a worker that has already failed.
                    drop(receiver);
                    drop(reply);
                    std::panic::resume_unwind(panic);
                }
            }
        })
        .ok_or_else(|| std::io::Error::other("storage worker unavailable"))?;
        Ok(Self { sender })
    }

    #[cfg(test)]
    pub(crate) fn submit(&self, command: C) -> Result<(), SubmitErrorGeneric<C>> {
        let (tx, _rx) = mpsc::channel();
        self.submit_with_reply(command, tx)
    }

    pub(crate) fn submit_ticket(&self, command: C) -> Result<Ticket<R>, SubmitErrorGeneric<C>> {
        let (tx, rx) = mpsc::channel();
        self.submit_with_reply(command, tx).map(|()| Ticket(rx))
    }

    fn submit_with_reply(
        &self,
        command: C,
        tx: mpsc::Sender<R>,
    ) -> Result<(), SubmitErrorGeneric<C>> {
        match self.sender.try_send((command, tx)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full((command, _))) => Err(SubmitErrorGeneric::Full(command)),
            Err(TrySendError::Disconnected((command, _))) => {
                Err(SubmitErrorGeneric::Stopped(command))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_executor_serializes_domain_jobs_and_returns_typed_results() {
        let executor = Executor::start(CAPACITY).unwrap();
        let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let first_order = order.clone();
        let first = executor
            .submit(move || {
                first_order.lock().unwrap().push("session");
                7u32
            })
            .unwrap();
        let second_order = order.clone();
        let second = executor
            .submit(move || {
                second_order.lock().unwrap().push("consent");
                String::from("ok")
            })
            .unwrap();
        assert_eq!(first.wait_blocking().unwrap(), 7);
        assert_eq!(second.wait_blocking().unwrap(), "ok");
        assert_eq!(&*order.lock().unwrap(), &["session", "consent"]);
    }

    #[test]
    fn typed_executor_start_refusal_is_explicit() {
        assert!(Executor::start_refused().is_err());
    }

    #[test]
    fn typed_executor_full_rejects_without_running_the_job() {
        let entered = std::sync::mpsc::channel();
        let release = std::sync::mpsc::channel();
        let executor = Executor::start(1).unwrap();
        let first = executor
            .submit({
                let entered = entered.0.clone();
                let release_rx = release.1;
                move || {
                    entered.send(()).unwrap();
                    release_rx.recv().unwrap();
                    1u8
                }
            })
            .unwrap();
        entered.1.recv().unwrap();
        let queued = executor.submit(|| 9u8).unwrap();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_job = ran.clone();
        assert!(matches!(
            executor.submit(move || {
                ran_job.store(true, std::sync::atomic::Ordering::Release);
                2u8
            }),
            Err(SubmitError::Full)
        ));
        assert!(!ran.load(std::sync::atomic::Ordering::Acquire));
        release.0.send(()).unwrap();
        assert_eq!(first.wait_blocking().unwrap(), 1);
        assert_eq!(queued.wait_blocking().unwrap(), 9);
    }

    #[test]
    fn typed_singleton_submit_runs_off_caller_thread() {
        let caller = std::thread::current().id();
        let ticket = submit(move || std::thread::current().id()).unwrap();
        match ticket.try_recv() {
            Ok(id) => assert_ne!(id, caller),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert_ne!(ticket.wait_blocking().unwrap(), caller)
            }
            Err(error) => panic!("unexpected ticket state: {error:?}"),
        }
    }

    fn accepted<C: Send + 'static, R: Send + 'static>(writer: &Writer<C, R>, c: C) -> Ticket<R> {
        writer
            .submit_ticket(c)
            .unwrap_or_else(|_| panic!("command refused"))
    }

    #[test]
    fn writes_are_fifo_off_thread_and_drain_without_a_blocking_drop() {
        let caller = std::thread::current().id();
        let mut previous = 0;
        let writer = Writer::start("storage test", CAPACITY, move |n| {
            assert_ne!(std::thread::current().id(), caller);
            assert_eq!(n, previous + 1);
            previous = n;
            n
        })
        .unwrap();
        let tickets: Vec<_> = (1..=8).map(|n| accepted(&writer, n)).collect();
        drop(writer);
        for (i, ticket) in tickets.into_iter().enumerate() {
            assert_eq!(ticket.wait_blocking().unwrap(), i + 1);
        }
    }

    #[test]
    fn full_queue_returns_the_command_without_waiting_for_disk() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = Writer::start("blocked storage test", 1, move |n| {
            if n == 1 {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            n
        })
        .unwrap();
        let first = accepted(&writer, 1);
        entered_rx.recv().unwrap();
        assert!(matches!(first.try_recv(), Err(TryRecvError::Empty)));
        let second = accepted(&writer, 2);
        assert!(matches!(writer.submit(3), Err(SubmitErrorGeneric::Full(3))));
        drop(writer); // must return even while the disk callback is blocked
        release_tx.send(()).unwrap();
        assert_eq!(first.wait_blocking().unwrap(), 1);
        assert_eq!(second.wait_blocking().unwrap(), 2);
    }

    #[test]
    fn a_failed_worker_disconnects_tickets_instead_of_claiming_success() {
        let writer = Writer::start("panicking storage test", 1, |_: ()| -> () {
            panic!("injected worker failure");
        })
        .unwrap();
        assert!(accepted(&writer, ()).wait_blocking().is_err());
        assert!(matches!(
            writer.submit(()),
            Err(SubmitErrorGeneric::Stopped(()))
        ));
    }

    #[test]
    fn typed_failure_is_not_visible_until_the_executor_is_stopped() {
        let (unwound_tx, unwound_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = Writer::start("paused panicking executor", 1, move |job: Job| {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
                Ok(()) => (),
                Err(panic) => {
                    unwound_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    std::panic::resume_unwind(panic);
                }
            }
        })
        .unwrap();
        let executor = Executor::from_writer(Ok(writer)).unwrap();
        let ticket = executor
            .submit(|| -> () { panic!("injected typed worker failure") })
            .unwrap();

        // The real Job closure has unwound and dropped its typed sender, while the outer worker
        // is deliberately still alive. The public ticket must not expose Disconnected yet.
        unwound_rx.recv().unwrap();
        assert!(matches!(ticket.try_recv(), Err(TryRecvError::Empty)));

        release_tx.send(()).unwrap();
        assert!(ticket.wait_blocking().is_err());
        assert!(matches!(executor.submit(|| ()), Err(SubmitError::Stopped)));
    }

    #[test]
    fn zero_capacity_is_refused_instead_of_a_queue_that_never_accepts() {
        assert!(Writer::start("zero", 0, |()| ()).is_err());
    }

    #[test]
    fn dropping_a_ticket_does_not_cancel_an_accepted_write() {
        let (saved_tx, saved_rx) = mpsc::channel();
        let writer = Writer::start("discarded ticket", 1, move |value| {
            saved_tx.send(value).unwrap();
        })
        .unwrap();
        drop(accepted(&writer, 42));
        drop(writer);
        assert_eq!(saved_rx.recv().unwrap(), 42);
    }

    #[test]
    fn cloned_writers_preserve_each_producers_fifo_and_deliver_once() {
        const COUNT: usize = 32;
        let mut next = [0usize; 2];
        let writer = Writer::start("concurrent storage test", COUNT * 2, move |command| {
            let (producer, sequence) = command;
            assert_eq!(sequence, next[producer]);
            next[producer] += 1;
            (producer, sequence)
        })
        .unwrap();
        let first = writer.clone();
        let second = writer.clone();
        let first_tickets = std::thread::spawn(move || {
            (0..COUNT)
                .map(|n| accepted(&first, (0usize, n)))
                .collect::<Vec<_>>()
        });
        let second_tickets = std::thread::spawn(move || {
            (0..COUNT)
                .map(|n| accepted(&second, (1usize, n)))
                .collect::<Vec<_>>()
        });
        let first_tickets = first_tickets.join().expect("first producer panicked");
        let second_tickets = second_tickets.join().expect("second producer panicked");
        drop(writer);

        let mut results = [Vec::new(), Vec::new()];
        for ticket in first_tickets.into_iter().chain(second_tickets) {
            let (producer, sequence) = ticket.wait_blocking().expect("worker disconnected");
            results[producer].push(sequence);
        }
        assert_eq!(results[0], (0..COUNT).collect::<Vec<_>>());
        assert_eq!(results[1], (0..COUNT).collect::<Vec<_>>());
        assert_eq!(results[0].len() + results[1].len(), COUNT * 2);
    }
}
