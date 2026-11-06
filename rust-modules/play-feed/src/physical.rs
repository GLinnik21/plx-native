//! The physical set the application composes: the buffer-feed backend and the URI driver's seat,
//! as one [`PhysicalSet`].
//!
//! The runner is the single admission owner; a driver only executes addressed commands and reports
//! evidence. This set answers the runner's [`DriverSet`] across the feed and the URI driver, and
//! lends the feed half to the pump as a [`FeedControl`]. The application builds the [`FeedBackend`]
//! (its native hooks and the sink it Loads through are the application's) and hands it to
//! [`FeedSet::new`]; the player's adapter holds the result as a `Box<dyn PhysicalSet>` and reaches
//! the feed only through [`PhysicalSet::feed`].
//!
//! It lives here, not in the application, because the player's own test fixtures compose the same
//! set over their own hooks and the application is not a crate they can name.

use crate::control::FeedBackend;
use plx_base::task::MainThread;
use plx_platform::tv::sink::VideoSink;
use plx_playback_contract::feed::{FeedControl, PhysicalSet, UriOnlySet};
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Operation, SourceKey, UriDriver,
};

/// Test support: a handle on the [`FeedSet`] an adapter boxes as its `dyn PhysicalSet`.
#[cfg(any(test, feature = "test-support"))]
pub mod probe;

/// The buffer-feed backend plus the URI driver's seat. The seat is the contract's
/// [`UriOnlySet`]: the URI half of every answer is the seat's own.
pub struct FeedSet {
    feed: FeedBackend,
    seat: UriOnlySet,
}

impl FeedSet {
    /// A set over `feed` with no URI driver yet.
    pub fn new(feed: FeedBackend) -> Self {
        Self { feed, seat: UriOnlySet::new() }
    }

    /// The sink the Feed's own verbs go through: the composition's hook, which answers the fence
    /// in place of the real sink while an off-frame `Unload` owns the native object. The
    /// composition hands it to the player's adapter beside this set, so the media-side verbs
    /// that are not hooks (the preroll volume, the ACB mirror, the diagnostics' window facts)
    /// reach the seam through the same one.
    pub fn display_sink(&self) -> fn() -> &'static dyn VideoSink {
        self.feed.resources().hooks().feed.sink
    }
}

impl DriverSet for FeedSet {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        match backend {
            BackendKind::Feed => self.feed.capabilities(BackendKind::Feed),
            BackendKind::Uri => self.seat.capabilities(BackendKind::Uri),
        }
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        match backend {
            BackendKind::Feed => self.feed.dispatch(BackendKind::Feed, mt, attempt, command, operation),
            BackendKind::Uri => self.seat.dispatch(BackendKind::Uri, mt, attempt, command, operation),
        }
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<BackendEvent> {
        let mut events = self.feed.poll(mt);
        events.extend(self.seat.poll(mt));
        events
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        match backend {
            BackendKind::Feed => self.feed.discard_staged(BackendKind::Feed, attempt, key),
            BackendKind::Uri => self.seat.discard_staged(BackendKind::Uri, attempt, key),
        }
    }

    fn exit_disposition(&self) -> ExitDisposition {
        if self.feed.exit_disposition() == ExitDisposition::PreserveNativeOwnership {
            return ExitDisposition::PreserveNativeOwnership;
        }
        self.seat.exit_disposition()
    }
}

impl PhysicalSet for FeedSet {
    fn feed(&mut self) -> Option<&mut dyn FeedControl> { Some(&mut self.feed) }

    fn uri(&self) -> Option<&(dyn UriDriver<Context = MainThread> + 'static)> { self.seat.uri() }

    fn uri_mut(&mut self) -> Option<&mut (dyn UriDriver<Context = MainThread> + 'static)> {
        self.seat.uri_mut()
    }

    fn install_uri(&mut self, driver: Box<dyn UriDriver<Context = MainThread>>)
        -> Result<(), Box<dyn UriDriver<Context = MainThread>>> {
        self.seat.install_uri(driver)
    }

    fn take_uri(&mut self) -> Option<Box<dyn UriDriver<Context = MainThread>>> {
        self.seat.take_uri()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::BackendSet;

    fn mt() -> MainThread {
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        unsafe { MainThread::assume() }
    }

    /// A fresh set holds the feed and no URI driver. (A dispatch needs a runner-minted command
    /// id, which only the player's own suites hold; the URI half is the contract's seat.)
    #[test]
    fn a_fresh_set_holds_the_feed_alone() {
        let mut set = FeedSet::new(crate::control::tests::backend());
        assert_eq!(BackendSet::from_capabilities(&set), BackendSet::FEED);
        assert!(set.feed().is_some());
        assert_eq!(set.exit_disposition(), ExitDisposition::Clean);
        assert!(set.uri().is_none() && set.poll(&mt()).is_empty());
    }

    /// The feed the set lends the pump is the set's own backend: one native session slot, not two.
    #[test]
    fn the_lent_feed_is_the_concrete_backend() {
        let mut set = FeedSet::new(crate::control::tests::backend());
        let concrete = &set.feed as *const FeedBackend as *const ();
        let lent = set.feed().expect("the set installs the feed") as *mut dyn FeedControl
            as *const ();
        assert_eq!(lent, concrete);
        assert!(!set.feed.resources().is_live());
        assert!(set.feed().unwrap().sample().is_none());
    }
}
