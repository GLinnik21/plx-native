//! The fence in front of the composition's raw sink.
//!
//! While an off-frame `Unload` owns the native object ([`PhysicalShared::native_releasing`]) the
//! main thread must not reach the object the worker is inside (PLX-NATIVE-1Q): every verb of the
//! sink the Feed's hooks hand out is refused. The release itself, which has to reach the real seam,
//! takes the raw sink instead.
//!
//! A [`SinkFence`] is a `const` the composition builds from the raw sink it chose and a refusing
//! sink in front of that same raw sink (`plx_platform::tv::sink::RefusingSink`, whose three memoized
//! queries keep the raw answer). It holds no state: the one thing it reads is the process's
//! [`FEED_SHARED`], which is why it lives here and not in the composition.
//!
//! [`PhysicalShared::native_releasing`]: crate::shared::PhysicalShared::native_releasing

use crate::shared::FEED_SHARED;
use plx_platform::tv::sink::VideoSink;

/// The raw sink and the refusing one in front of it. See the module doc.
pub struct SinkFence {
    raw: fn() -> &'static dyn VideoSink,
    fence: &'static dyn VideoSink,
}

impl SinkFence {
    /// `fence` is the composition's `&RefusingSink(raw)`, promoted to `'static` by the `const`
    /// that builds this value.
    pub const fn new(raw: fn() -> &'static dyn VideoSink, fence: &'static dyn VideoSink) -> Self {
        Self { raw, fence }
    }

    /// The sink a main-thread verb goes through right now: the fence while the native object is
    /// being released, the raw sink otherwise. Asked afresh at every call.
    pub fn current(&self) -> &'static dyn VideoSink {
        if FEED_SHARED.native_releasing() {
            self.fence
        } else {
            (self.raw)()
        }
    }
}
