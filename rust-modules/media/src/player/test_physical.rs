//! The tests' composition of the player's physical set: what the application's boot composes
//! (`app::adapters::backends`) from the same hooks, for the fixtures that drive a Feed.
//!
//! [`PlayerAdapter::new`] starts with the contract's `UriOnlySet`, so a test that starts, feeds
//! or stops a Feed builds its adapter with [`PlayerAdapter::new_fed`] (or `new_probed`, which
//! keeps a handle on the set), and a test that never touches one keeps `new`.

use super::adapter::PlayerAdapter;
use plx_base::task::MainThread;
#[cfg(feature = "play-feed")]
use plx_play_feed::control::FeedBackend;
#[cfg(feature = "play-feed")]
use plx_play_feed::feed_resources::FeedResources;
#[cfg(feature = "play-feed")]
use plx_play_feed::physical::FeedSet;
#[cfg(feature = "play-feed")]
use plx_play_feed::shared::FEED_SHARED;

/// A Feed set over the process's shared feed state and the player's native hooks.
#[cfg(feature = "play-feed")]
pub(super) fn feed_set() -> FeedSet {
    FeedSet::new(FeedBackend::new(FeedResources::new(&FEED_SHARED, &super::engine::NATIVE_HOOKS)))
}

impl PlayerAdapter {
    /// [`Self::new`] with the Feed set installed (a build with no Feed has none to install).
    pub fn new_fed(mt: MainThread) -> Self {
        let mut adapter = Self::new(mt);
        #[cfg(feature = "play-feed")]
        assert!(adapter.install_physical(Box::new(feed_set())).is_ok(),
            "a fresh adapter takes its physical set");
        adapter
    }
}
