//! The job lane: how a machine asks the world for IO without doing the IO itself.
//!
//! A store step that needs a fetch does not start a thread. It emits a [`Job`], a plain value
//! naming where the ONE answer goes ([`Job::addr`]) and what is asked ([`Job::req`]), and the
//! owner's runner (in the application layer, which owns the threads, the clock and the network)
//! starts it. The answer comes back through the job's [`Port`], and from there the result path is
//! the one results already take: the landing, then the pump.
//!
//! This is the `machine` layer, so it names nothing above `base` and nothing store-specific. A
//! store's own job enum, its request types and its port live with the store (`plx_data::stores`),
//! and the runner lives in the application.
//!
//! Tests of a step read the jobs it emitted with [`assert_fx!`] instead of waiting on a thread.

use crate::machine::Addr;

/// One request a machine asks the world to run. `addr` is where its answer is addressed; `req`
/// is the store's own description of the work, with everything the worker needs already captured
/// (a worker never reads the machine's state).
#[derive(Debug, Clone, PartialEq)]
pub struct Job<R> {
    pub addr: Addr,
    pub req: R,
}

/// Where a job's ONE answer goes: **exactly one call per job**, `done` when the work produced an
/// answer (a failed fetch answers with its failure value) and `refused` when it was never started
/// (an admission refused, no worker to run it). Silence leaves the request's reservation held
/// until its owner is reset, and a second call is a protocol error the landing refuses.
///
/// A port is captured when the job is emitted, so an answer lands where the request was made even
/// if the owner has since replaced the thing it lands into.
///
/// **The type does not enforce "exactly one".** The methods take `&self` and a port is usually
/// `Clone`, so a second call compiles. What checks the contract, and what does not:
///
/// - **A second answer** reaches the landing, which refuses it (`PublishError::AlreadyTerminal`, or
///   `Unknown` once the pump has taken the terminal). The store's publish step swallows that error
///   unless its replay tracker is armed (a controlled run), so in production a double answer is
///   silent.
/// - **Silence** is checked by nothing. A runner that takes a job and drops it holds the
///   reservation until the owner is reset, and four of them refuse every later request. The
///   application's leftover-job check sees only jobs nobody took, not jobs taken and never
///   answered. A drop guard on the port would not do: replay legitimately drops the launch
///   closure unanswered, because the tape supplies its answers.
/// - **The count per branch** is pinned by the runner's own tests (`app/adapters/stores.rs`): each
///   outcome of a job reaches `done` or `refused` once. A runner written on this lane carries the
///   same test for each of its branches; nothing else will notice a branch that answers twice or
///   not at all.
pub trait Port<T>: Send + Sync + 'static {
    /// The work's answer.
    fn done(&self, addr: Addr, out: T);
    /// The work was refused and will never answer on its own.
    fn refused(&self, addr: Addr);
}

/// Assert that a step emitted exactly these jobs, in this order. `$got` is the `Vec` the step's
/// owner hands back (`take_jobs()`), the list holds the expected values.
///
/// ```ignore
/// assert_fx!(store.take_jobs(), [StoreJob::Detail(Job { addr, req }, store.detail_port())]);
/// assert_fx!(store.take_jobs(), []); // and nothing else
/// ```
#[macro_export]
macro_rules! assert_fx {
    ($got:expr, [$($want:expr),* $(,)?]) => {{
        let got: Vec<_> = $got;
        let want: Vec<_> = vec![$($want),*];
        assert_eq!(got, want, "the step emitted exactly these jobs, in order");
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{MachineId, RequestId, StoreOrd};
    use std::sync::Mutex;

    fn job(req: u32, what: &'static str) -> Job<&'static str> {
        Job { addr: Addr { to: MachineId::Store(StoreOrd(0)), req: RequestId(req) }, req: what }
    }

    #[test]
    fn assert_fx_passes_on_exactly_the_jobs_in_order() {
        assert_fx!(vec![job(1, "a"), job(2, "b")], [job(1, "a"), job(2, "b")]);
        assert_fx!(Vec::<Job<&str>>::new(), []);
    }

    #[test]
    #[should_panic(expected = "the step emitted exactly these jobs, in order")]
    fn assert_fx_fails_on_a_different_order() {
        assert_fx!(vec![job(2, "b"), job(1, "a")], [job(1, "a"), job(2, "b")]);
    }

    #[test]
    #[should_panic(expected = "the step emitted exactly these jobs, in order")]
    fn assert_fx_fails_on_a_missing_job() {
        assert_fx!(vec![job(1, "a")], [job(1, "a"), job(2, "b")]);
    }

    #[test]
    #[should_panic(expected = "the step emitted exactly these jobs, in order")]
    fn assert_fx_fails_on_an_extra_job() {
        assert_fx!(vec![job(1, "a"), job(2, "b")], [job(1, "a")]);
    }

    #[test]
    #[should_panic(expected = "the step emitted exactly these jobs, in order")]
    fn assert_fx_fails_on_a_different_request() {
        assert_fx!(vec![job(1, "a")], [job(1, "b")]);
    }

    /// A port answers through `&self`, so one handle can serve a job from another thread.
    struct Log(Mutex<Vec<(u32, Option<&'static str>)>>);

    impl Port<&'static str> for Log {
        fn done(&self, addr: Addr, out: &'static str) {
            self.0.lock().unwrap().push((addr.req.0, Some(out)));
        }
        fn refused(&self, addr: Addr) {
            self.0.lock().unwrap().push((addr.req.0, None));
        }
    }

    #[test]
    fn a_port_is_shareable_and_answers_by_address() {
        let log = std::sync::Arc::new(Log(Mutex::new(Vec::new())));
        let port: std::sync::Arc<dyn Port<&'static str>> = log.clone();
        let (a, b) = (job(1, "a"), job(2, "b"));
        let worker = std::thread::spawn({
            let port = port.clone();
            move || port.done(a.addr, a.req)
        });
        worker.join().unwrap();
        port.refused(b.addr);
        assert_eq!(*log.0.lock().unwrap(), vec![(1, Some("a")), (2, None)]);
    }
}
