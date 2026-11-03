//! Common subtitle resources composed beside the physical feed transport.
//! The existing process-lifetime owner stays singular; a feed executor receives only `feed`.
use std::ops::Deref;
use std::sync::{atomic::Ordering, Mutex};

pub struct Shared {
    pub feed: super::shared::Shared,
    pub ass_sources: Mutex<super::ass_source::Store>,
    pub ass_renderer: super::ass::Runtime,
}

impl Shared {
    pub const fn new() -> Self {
        Self {
            feed: super::shared::Shared::new(),
            ass_sources: Mutex::new(super::ass_source::Store::new()),
            ass_renderer: super::ass::Runtime::new(),
        }
    }

    pub fn reset_session(&self) {
        self.reset(false);
    }

    pub fn reset_session_for_reload(&self) {
        self.reset(true);
    }

    fn reset(&self, for_reload: bool) {
        let duration = self.feed.duration_ns.load(Ordering::Relaxed);
        self.feed.reset_session_with(for_reload, || {
            // File facts survive a reload; completed rasters do not survive source retirement.
            let mut sources = self.ass_sources.lock().unwrap_or_else(|e| e.into_inner());
            if for_reload { sources.retain_for_reload(); } else { sources.reset(); }
        });
        if for_reload {
            self.feed.duration_ns.store(duration, Ordering::Relaxed);
        }
    }
}

impl Deref for Shared {
    type Target = super::shared::Shared;
    fn deref(&self) -> &Self::Target { &self.feed }
}
