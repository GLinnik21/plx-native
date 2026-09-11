//! **The Player machine** — the playback state `App` owns (restructure spec §2.2, phase 9).
//!
//! Everything about a playback that is a DECISION rather than a resource lives here, in one value
//! with one owner. Before phase 9 the same state was two `static mut`s and a scattering of loop
//! locals: `route::decision::SESSION` (the resolved route, the stream URL, the transcode session,
//! the HUD strings and the Up Next queue) and `App.foreground` (the app-switch lifecycle). Neither
//! could be handed to a function, so "who may write this" was a comment — `MAIN THREAD ONLY` —
//! rather than a signature, and the compiler had nothing to check.
//!
//! It is a MACHINE, not an adapter: no OS handle, no FFI, no thread. The Starfish/ACB session
//! object and its `MainThread` token are the ADAPTER's
//! ([`crate::player::adapter::PlayerAdapter`]), and the two are deliberately separate fields of
//! `App` so a screen or a pump can be handed the decisions without being handed the pipeline.
//!
//! # The tick
//!
//! [`Player::now_ms`] is this frame's millisecond stamp, set ONCE per iteration by the loop from
//! the same `fr.now` every other phase of the frame reads (spec §4.1: "Every integrator takes the
//! tick from its event"). It is the only clock this machine has, and it exists so that
//! `PlaybackSession::auto_last_switch` — the adaptive controller's "how long since the last
//! visible rung change" — is stamped from the frame tick rather than from a clock read taken at
//! whatever depth of the call stack happened to need it. `crate::player::vclock_ms()` was the
//! interim source; a second reader of a second clock is exactly how two stamps of "now" in one
//! frame come to disagree.

use crate::route::PlaybackSession;

/// The playback state `App` owns (§2.2).
pub(crate) struct Player {
    /// The resolved route, the stream URL, the transcode session, the HUD strings and the Up Next
    /// queue — what `route::decision::SESSION` was.
    pub(crate) session: PlaybackSession,
    /// The webOS app-switch lifecycle (`app::lifecycle`): suspend on background, the exact-attempt
    /// Load on foreground. Already a machine before phase 9; this moves the `App` field in beside
    /// the session it drives, because every one of its transitions is about this playback.
    pub(crate) lifecycle: crate::app::lifecycle::ForegroundLifecycle,
    /// This frame's millisecond stamp — see the module doc. Zero before the first frame.
    pub(crate) now_ms: u32,
    /// **Is the hardware video plane bound to our sink?** (spec §9, §4.4, §16 risk 10.)
    ///
    /// A DECISION of this machine, with defined transitions, not an expression the frame
    /// re-derives: it is set when the ACB bind transaction has actually reached the sink and
    /// cleared only when the sink is confirmed unbound. Read it; never recompute it. Through
    /// phase 8 the same question was asked as `matches!(app.route, Route::Player)` at three
    /// separate places in the loop, which is a claim about which SCREEN is up — true a second
    /// before the plane binds and a second after it unbinds, when the compositor has an ordinary
    /// UI surface and the gate should be treating it like one.
    ///
    /// Its EDGES are the only source of [`crate::ui::present::PresentEvent::VideoPlane`]; see
    /// [`Player::set_video_plane_bound`].
    pub(crate) video_plane_bound: bool,
    /// Last frame's [`crate::screens::player::PlayerScreen::clock_fingerprint`] — the machine's
    /// memory of what the player page's clock-driven picture WAS, so a change can be a frame.
    clock_fp: u64,
}

impl Player {
    pub(crate) fn new() -> Self {
        Self {
            session: PlaybackSession::IDLE,
            lifecycle: crate::app::lifecycle::ForegroundLifecycle::IDLE,
            now_ms: 0,
            video_plane_bound: false,
            clock_fp: 0,
        }
    }

    /// **The player page's clock-driven motion report** (spec §8.3). `true` = this frame is owed a
    /// present. See `PlayerScreen::clock_fingerprint` for what the value is and why the comparison
    /// lives here rather than as an `invalidate` inside each animator.
    pub(crate) fn note_clock(&mut self, fp: u64) -> bool {
        let changed = self.clock_fp != fp;
        self.clock_fp = fp;
        changed
    }

    /// **The one writer of [`Player::video_plane_bound`], and the one source of
    /// `PresentEvent::VideoPlane`** (spec §16 risk 10).
    ///
    /// Publishes on the EDGES only. That is not an optimisation: the present gate's video-plane
    /// input is a piece of STATE with one owner, and a level written every frame would be a second
    /// formula term feeding it — exactly the risk the spec names, where the gate and the machine
    /// come to disagree about which frame the plane went away on and the false edge lands on a
    /// frame that was never presented.
    /// `Some(bound)` when this was an EDGE — the loop forwards that, and only that, to the two
    /// `ui::present::Present` machines it owns beside the live gate.
    pub(crate) fn set_video_plane_bound(&mut self, bound: bool) -> Option<bool> {
        if self.video_plane_bound == bound {
            return None;
        }
        self.video_plane_bound = bound;
        // ONE line per edge, in the event log. The bit decides the present gate, the opaque
        // region, the capture skip and whether a frame may sample the framebuffer at all — and
        // every one of those is invisible from a log that does not say when the plane arrived.
        crate::log(if bound {
            "videoplane: BOUND — the gate presents unconditionally and no frame may snapshot"
        } else {
            "videoplane: unbound — ordinary idle rules from here"
        });
        crate::ui::idle::note(crate::ui::present::PresentEvent::VideoPlane(bound));
        // The FALSE edge has to reach the panel, and the frame it lands on is very often one the
        // gate would otherwise skip — the picture is gone and nothing is animating (spec §3.3
        // step 9). `invalidate` is what makes that frame present, so the opaque region is really
        // cleared and the UI surface really goes back to being blended.
        if !bound {
            crate::ui::idle::invalidate();
        }
        Some(bound)
    }

    /// The loop's one write of the frame tick, at the top of the iteration (§4.1).
    pub(crate) fn set_now(&mut self, now_ms: u32) {
        self.now_ms = now_ms;
        // …and the session's mirror of it, which is what `route::decision`'s two stamp readers
        // see. `PlaybackSession::now_ms`'s own doc says why the tick travels with the value
        // rather than as a parameter beside it.
        self.session.set_now(now_ms);
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}
