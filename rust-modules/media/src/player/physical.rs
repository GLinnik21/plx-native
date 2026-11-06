//! The physical drivers the player adapter owns, as one [`PhysicalSet`].
//!
//! The runner is the single admission owner; a driver only executes addressed commands and reports
//! evidence. This set holds the persistent buffer-feed backend (with the `play-feed` feature) and
//! the optional URI driver the composition installed, answers the runner's [`DriverSet`] across
//! both, and lends the feed half to the pump as a [`FeedControl`]. It replaces the temporary
//! per-call borrow the adapter used to build from its fields.
//!
//! The adapter holds the set as a `Box<dyn PhysicalSet>` and reaches the feed only through
//! [`PhysicalSet::feed`]. The test fixtures that install, take and inspect an Engine hold a
//! `probe::Probe` on the boxed value instead (`PlayerAdapter::new_probed`).

use plx_base::task::MainThread;
use plx_playback_contract::feed::{FeedControl, PhysicalSet};
use plx_playback_contract::{
    AttemptId, BackendEvent, BackendKind, Capabilities, CommandId, DispatchResult, DriverSet,
    ExitDisposition, Failure, Operation, SourceKey, UriDriver,
};
#[cfg(feature = "play-feed")]
use plx_play_feed::control::FeedBackend;

#[cfg(all(test, feature = "play-feed"))]
pub(super) mod probe;

/// The adapter's persistent physical executors.
pub(super) struct Physical {
    #[cfg(feature = "play-feed")]
    feed: FeedBackend,
    uri: Option<Box<dyn UriDriver<Context = MainThread>>>,
}

impl Physical {
    /// The process's physical executors: the buffer-feed backend over the shared feed state and
    /// the engine's native hooks (with `play-feed`), and no URI driver yet.
    pub(super) fn new() -> Self {
        Self {
            #[cfg(feature = "play-feed")]
            feed: FeedBackend::new(plx_play_feed::feed_resources::FeedResources::new(
                &plx_play_feed::shared::FEED_SHARED, &super::engine::NATIVE_HOOKS)),
            uri: None,
        }
    }
}

impl DriverSet for Physical {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        match backend {
            BackendKind::Feed => {
                #[cfg(feature = "play-feed")]
                { self.feed.capabilities(BackendKind::Feed) }
                #[cfg(not(feature = "play-feed"))]
                { None }
            }
            BackendKind::Uri => {
                self.uri.as_ref().and_then(|driver| driver.capabilities(BackendKind::Uri))
            }
        }
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        match backend {
            BackendKind::Feed => {
                #[cfg(feature = "play-feed")]
                { self.feed.dispatch(BackendKind::Feed, mt, attempt, command, operation) }
                #[cfg(not(feature = "play-feed"))]
                { let _ = (mt, attempt, command, operation);
                    DispatchResult::Refused(Failure::Unavailable) }
            }
            BackendKind::Uri => {
                self.uri.as_mut().map_or(DispatchResult::Refused(Failure::Unavailable),
                    |driver| driver.dispatch(BackendKind::Uri, mt, attempt, command, operation))
            }
        }
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<BackendEvent> {
        #[cfg(feature = "play-feed")]
        let mut events = self.feed.poll(mt);
        #[cfg(not(feature = "play-feed"))]
        let mut events = Vec::new();
        if let Some(uri) = self.uri.as_mut() { events.extend(uri.poll(mt)); }
        events
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        match backend {
            BackendKind::Feed => {
                #[cfg(feature = "play-feed")]
                self.feed.discard_staged(BackendKind::Feed, attempt, key);
                #[cfg(not(feature = "play-feed"))]
                let _ = (attempt, key);
            }
            BackendKind::Uri => {
                if let Some(uri) = self.uri.as_mut() {
                    uri.discard_staged(BackendKind::Uri, attempt, key);
                }
            }
        }
    }

    fn exit_disposition(&self) -> ExitDisposition {
        #[cfg(feature = "play-feed")]
        if self.feed.exit_disposition() == ExitDisposition::PreserveNativeOwnership {
            return ExitDisposition::PreserveNativeOwnership;
        }
        if self.uri.as_ref().is_some_and(|driver|
            driver.exit_disposition() == ExitDisposition::PreserveNativeOwnership) {
            return ExitDisposition::PreserveNativeOwnership;
        }
        ExitDisposition::Clean
    }
}

impl PhysicalSet for Physical {
    fn feed(&mut self) -> Option<&mut dyn FeedControl> {
        #[cfg(feature = "play-feed")]
        { Some(&mut self.feed) }
        #[cfg(not(feature = "play-feed"))]
        { None }
    }

    fn uri(&self) -> Option<&(dyn UriDriver<Context = MainThread> + 'static)> {
        self.uri.as_deref()
    }

    fn uri_mut(&mut self) -> Option<&mut (dyn UriDriver<Context = MainThread> + 'static)> {
        self.uri.as_deref_mut()
    }

    fn install_uri(&mut self, driver: Box<dyn UriDriver<Context = MainThread>>)
        -> Result<(), Box<dyn UriDriver<Context = MainThread>>> {
        self.uri = Some(driver);
        Ok(())
    }

    fn take_uri(&mut self) -> Option<Box<dyn UriDriver<Context = MainThread>>> {
        self.uri.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::test_support::FakeUriDriver;
    use plx_playback_contract::{BackendSet, DriverEvent, ReleaseStatus};

    const URI_CAPS: Capabilities = Capabilities {
        progressive_uri: true, hls_uri: false, packet_feed: false, pause: true, seek: true,
        audio_track: false, subtitle_track: false, audio_off: false, subtitle_off: false,
    };

    fn mt() -> MainThread {
        // SAFETY: a host test; nothing here touches the thread-affine state the token guards.
        unsafe { MainThread::assume() }
    }

    fn attempt() -> AttemptId { AttemptId::new(1, 1) }

    /// The set a fresh adapter holds installs the feed (when compiled in) and no URI driver. (A
    /// dispatch needs a runner-minted command id, which only the adapter's own suites hold.)
    #[test]
    fn a_fresh_set_holds_the_feed_alone() {
        let mut set = Physical::new();
        let expected = if cfg!(feature = "play-feed") { BackendSet::FEED } else { BackendSet::NONE };
        assert_eq!(BackendSet::from_capabilities(&set), expected);
        assert_eq!(set.feed().is_some(), cfg!(feature = "play-feed"));
        assert_eq!(set.exit_disposition(), ExitDisposition::Clean);
        assert!(set.uri().is_none() && set.poll(&mt()).is_empty());
    }

    /// The URI driver answers beside the feed: its capabilities join the set, its polled events
    /// come back, a staged source is discarded on its half only, and its exit disposition counts.
    #[test]
    fn an_installed_uri_driver_answers_beside_the_feed() {
        let (driver, handle) = FakeUriDriver::new(URI_CAPS);
        let mut set = Physical::new();
        assert!(set.install_uri(Box::new(driver)).is_ok());
        let expected = if cfg!(feature = "play-feed") { BackendSet::BOTH } else { BackendSet::URI };
        assert_eq!(BackendSet::from_capabilities(&set), expected);
        handle.queue([DriverEvent::Release { attempt: attempt(), status: ReleaseStatus::Released }]);
        let events = set.poll(&mt());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].backend, BackendKind::Uri);
        let key = SourceKey::new(3);
        set.discard_staged(BackendKind::Feed, attempt(), key);
        assert!(handle.discarded().is_empty(), "a feed discard never reaches the URI driver");
        set.discard_staged(BackendKind::Uri, attempt(), key);
        assert_eq!(handle.discarded(), [(attempt(), key)]);
        assert_eq!(set.exit_disposition(), ExitDisposition::Clean);
        handle.set_exit_preserved(true);
        assert_eq!(set.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        assert!(set.take_uri().is_some());
        assert_eq!(BackendSet::from_capabilities(&set),
            if cfg!(feature = "play-feed") { BackendSet::FEED } else { BackendSet::NONE });
    }

    /// The feed the set lends the pump is the set's own backend: one native session slot, not two.
    #[cfg(feature = "play-feed")]
    #[test]
    fn the_lent_feed_is_the_concrete_backend() {
        let mut set = Physical::new();
        let concrete = &set.feed as *const FeedBackend as *const ();
        let lent = set.feed().expect("a feed build installs the feed") as *mut dyn FeedControl
            as *const ();
        assert_eq!(lent, concrete);
        assert!(!set.feed.resources().is_live());
        assert!(set.feed().unwrap().sample().is_none());
    }
}
