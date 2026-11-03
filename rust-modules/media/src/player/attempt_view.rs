//! Main-thread readout for one physical playback attempt. A backend publishes sparse facts
//! addressed to the attempt that produced them; an absent fact remains unknown across a reset.
//! This is a value owned by `PlaybackSession`, never a second process-wide mailbox.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Backend { Feed, Uri }

/// Which runtime boundary ended playback after route resolution succeeded.
///
/// This is a small product vocabulary, not a copy of FFmpeg or Starfish return codes. Every
/// variant is backed by a distinct signal the worker already publishes, so the HUD never parses
/// log strings or guesses from elapsed time.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RuntimeFailure {
    /// No worker supplied a cause. Say that honestly rather than leaving the reason slot blank.
    Unknown,
    /// The producer never opened a usable media stream or produced a video access unit.
    MediaSource,
    /// The media producer stopped after it had supplied at least one access unit. The worker's
    /// flag deliberately says nothing narrower about why it stopped.
    PlaybackInterrupted,
    /// Starfish refused the Load declaration, so no decoder session could start.
    TvPipeline,
    /// Issue #74 D.1.4's `NATIVE_LOAD_BUDGET` fired — a hang, not a firmware refusal. See
    /// [`FailureKind::LoadTimeout`].
    LoadTimeout,
    /// The previous object's `Unload` is overdue. See [`FailureKind::ReleaseTimeout`].
    ReleaseTimeout,
    /// The resolve landed no plan and no verdict ([`FailureKind::ResolveFailed`]).
    ResolveFailed,
}

/// How the pipeline refused a Load, for the `playback.failed` event: the Load call itself returned
/// 0 (`Sync`, `threads::load_thread` via `publish_native_load_failure`), or the call returned and
/// a `type=18` error callback arrived before any picture (`Async`, `sf_on_event`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RefusalPath {
    None = 0,
    Sync = 1,
    Async = 2,
}

impl RefusalPath {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Sync,
            2 => Self::Async,
            _ => Self::None,
        }
    }

    /// The stable wire code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sync => "sync",
            Self::Async => "async",
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct AttemptFacts {
    pub requested_kbps: Option<i64>,
    pub refusal_path: Option<RefusalPath>,
    pub runtime_failure: Option<RuntimeFailure>,
    pub no_video: Option<bool>,
    pub position_ns: Option<i64>,
    pub duration_ns: Option<i64>,
    pub playable_buffer_ms: Option<i64>,
    /// A callback reported a decoded frame. This does not independently prove panel presentation.
    pub reported_frame: Option<bool>,
    pub frames: Option<i32>,
    pub raster: Option<(i32, i32)>,
    pub pipeline_stage: Option<u8>,
    pub pipeline_error: Option<i32>,
    pub http_status: Option<i32>,
    pub demux_exit: Option<u8>,
    pub declared_kbps: Option<i64>,
    pub media_kbps: Option<i64>,
    pub abr_buffer_ms: Option<i64>,
    pub transport_rc: Option<i32>,
    pub transfer_age_ms: Option<i64>,
    pub transport_reopen_failed: Option<bool>,
}

#[derive(Clone, Copy)]
pub struct AttemptView<A> {
    attempt: Option<A>,
    backend: Option<Backend>,
    facts: AttemptFacts,
}

impl<A: Copy + Eq> AttemptView<A> {
    pub const EMPTY: Self = Self { attempt: None, backend: None, facts: AttemptFacts {
        runtime_failure: None, no_video: None, requested_kbps: None, refusal_path: None,
        position_ns: None, duration_ns: None, playable_buffer_ms: None,
        reported_frame: None, frames: None, raster: None, pipeline_stage: None,
        pipeline_error: None, http_status: None, demux_exit: None,
        declared_kbps: None, media_kbps: None, abr_buffer_ms: None,
        transport_rc: None, transfer_age_ms: None, transport_reopen_failed: None,
    } };

    pub fn begin(&mut self, attempt: A, backend: Backend) {
        *self = Self { attempt: Some(attempt), backend: Some(backend), ..Self::EMPTY };
    }

    pub fn attempt(&self) -> Option<A> { self.attempt }
    pub fn backend(&self) -> Option<Backend> { self.backend }

    pub fn facts(&self) -> &AttemptFacts { &self.facts }

    /// An old worker or an unrelated backend cannot change the current readout.
    pub fn ingest(&mut self, attempt: A, incoming: AttemptFacts) -> bool {
        if self.attempt != Some(attempt) { return false; }
        self.facts = incoming;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_attempt_resets_every_fact_and_refuses_late_publishers() {
        let (a, b) = (1_u64, 2_u64);
        let mut view = AttemptView::EMPTY;
        view.begin(a, Backend::Feed);
        assert!(view.ingest(a, AttemptFacts { position_ns: Some(42), reported_frame: Some(true), ..Default::default() }));
        view.begin(b, Backend::Uri);
        assert_eq!(view.facts().position_ns, None);
        assert_eq!(view.facts().reported_frame, None);
        assert!(view.backend() == Some(Backend::Uri));
        assert!(!view.ingest(a, AttemptFacts { position_ns: Some(99), ..Default::default() }));
        assert_eq!(view.facts().position_ns, None);
        assert!(view.ingest(b, AttemptFacts { duration_ns: Some(100), ..Default::default() }));
        assert_eq!(view.facts().duration_ns, Some(100));
        assert!(view.ingest(b, AttemptFacts::default()));
        assert_eq!(view.facts().duration_ns, None, "a sparse replacement cannot carry stale evidence");
    }
}
