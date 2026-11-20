//! Test scaffolding for the code that polls or pushes SDL events: only SDL's event subsystem, with
//! no window, renderer, boot or device. Compiled for this crate's own tests and, through the
//! `test-support` feature, for the dependents' test builds (the application's loop tests poll SDL
//! when a by-id token drains it first). Never in a shipped build.
use crate::sys;

/// SDL's event subsystem alone (`SDL_INIT_EVENTS`), brought up for the duration of a test and
/// quit when dropped. A test that holds one serialises on `plx_base::testlock::serial()` first,
/// because SDL's queue is process-wide.
pub struct EventsOnly;

impl EventsOnly {
    pub fn init() -> Self {
        assert_eq!(unsafe { sys::SDL_Init(0x4000) }, 0);
        Self
    }
}

impl Drop for EventsOnly {
    fn drop(&mut self) {
        unsafe { sys::SDL_Quit() };
    }
}
