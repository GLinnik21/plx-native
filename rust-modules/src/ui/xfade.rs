//! `Xfade` — the ONE content cross-fade choreography: fade the outgoing content OUT, swap the
//! data at the floor, fade the incoming content IN. Sibling of
//! [`Popover`](crate::ui::popover::Popover), which owns the modal *appear*; this owns the case
//! where a screen's CONTENT is replaced under it.
//!
//! **Why it SCHEDULES the swap instead of reacting to one.** The stores this rides on wipe
//! themselves synchronously on the key press — `browse::requery` empties `st.items` in the same
//! call as the input — so by the next frame there is nothing left to fade OUT. A fader that only
//! *watched* the data could therefore only ever be a fade IN, which is the half that does not fix
//! the complaint. So the fader owns the clock: [`Xfade::tick`] returns `true` on **exactly one**
//! frame and the screen applies its own typed pending action there. The animation and the swap
//! cannot disagree, because the animation IS the swap's schedule. `ui::press` defers a click's
//! activation the same way (its `COMMIT_MS`, so the spring-back bounce is on screen first).
//!
//! **Why milliseconds and not a `Spring`.** Everything else that moves in this UI is a
//! critically-damped spring, and that is right for a value the user steers (focus pop, scroll, a
//! panel's appear). This one gates a STATE COMMIT, so it needs a hard deadline: a spring is
//! asymptotic, "arrived" is a threshold guess, and the frame that threshold is crossed on depends
//! on the entry VELOCITY — which is non-zero and pointing the wrong way whenever a second reload
//! interrupts a fade-in. From `gfx::spring`'s closed form `x(t) = (1 + ωt)·e^(−ωt)` (ω = √k), the
//! shared `popover::K_APPEAR` = 300 takes ~337 ms to fall from 1 to 0.02; gating a 70 ms commit off
//! it would need k ≈ 2800 — not a spring any more, an exponential with a threshold bolted on. So
//! the RAMP is linear (exact deadline) and the alpha handed out is smoothstepped, so the dissolve
//! eases at both ends without moving the commit frame.
//!
//! It owns MOTION only — no pending action, no data. Each screen keeps its own typed pending
//! value, exactly as each screen keeps its own focus; a generic boxed-closure queue in here would
//! put a `Box<dyn FnOnce>` behind a `static mut` for no gain.

/// Outgoing ramp (ms). Short on purpose: the data swap is deferred by exactly this long, and a
/// control that acknowledges a press later than ~100 ms reads as dropped input. ≈4–5 frames at
/// 60 Hz; bounded at 2 frames by `app.rs`'s 0.05 s `dt` clamp.
const OUT_MS: f32 = 70.0;
/// Incoming ramp (ms) — deliberately longer than [`OUT_MS`]: leave fast, arrive gently, the
/// standard dissolve asymmetry. ≈9 frames at 60 Hz.
const IN_MS: f32 = 140.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Nothing in flight; alpha is 1.
    Idle,
    /// Ramping the OUTGOING content to 0. Ends by committing the swap.
    Out,
    /// Parked at 0 with the swap applied, waiting for the incoming content to exist.
    Hold,
    /// Ramping the INCOMING content to 1.
    In,
}

/// A screen's content cross-fade: one phase + one linear ramp, stepped once a frame.
///
/// Hold one per faded band (Library's grid is the first user). The screen calls [`reload`] on the
/// press, [`tick`] beside its springs, and pushes [`alpha`] as ONE `Painter::alpha` around the
/// content that is a function of the swapped data — never around the chrome that persists across
/// the swap, and never around a loading spinner, which must stay legible while the band is dark.
///
/// [`reload`]: Xfade::reload
/// [`tick`]: Xfade::tick
/// [`alpha`]: Xfade::alpha
pub(crate) struct Xfade {
    phase: Phase,
    /// Linear 0..1 — 0 = fully faded out, 1 = fully present. The COMMIT deadline is measured on
    /// this, not on [`Xfade::alpha`], so the easing can be retuned without moving the swap.
    t: f32,
}

impl Xfade {
    /// A fader at rest with its content fully present — the state a screen that has never swapped
    /// anything sits in, and what a `static mut` initializer needs (hence `const`).
    pub(crate) const fn new() -> Self {
        Xfade {
            phase: Phase::Idle,
            t: 1.0,
        }
    }

    /// A content swap has been requested. Fades out **from wherever the alpha already is** — a
    /// second request mid-fade must not restart the ramp, or holding a tab key down would keep the
    /// content dark for as long as the user keeps pressing. A request while already parked at 0
    /// commits on the very next frame with no visible fade, which is correct: there is nothing on
    /// screen left to dissolve.
    pub(crate) fn reload(&mut self) {
        self.phase = Phase::Out;
    }

    /// Mount a screen whose content is not on screen yet (route entry, or a store wiped from under
    /// us): park at 0 with NO commit pending, and fade in once the content is ready. This is also
    /// the one recovery for a fader left mid-phase by leaving the screen, since only the screen's
    /// own route steps it.
    pub(crate) fn mount(&mut self) {
        self.phase = Phase::Hold;
        self.t = 0.0;
    }

    /// The content changed under us and there is nothing to fade out — an async landing the screen
    /// did not schedule. Ramp the new content straight in, with no commit.
    pub(crate) fn arrive(&mut self) {
        self.phase = Phase::In;
        self.t = 0.0;
    }

    /// Abandon a fade-OUT and bring the content back, with **no commit** — the request that started
    /// it is being WITHDRAWN (BACK pressed inside the [`OUT_MS`] window; a destination re-pressed
    /// that turns out to be the one already showing). The ramp REVERSES from wherever `t` is rather
    /// than restarting, so a withdrawal 20 ms in costs 20 ms of fade-in, not a whole [`IN_MS`].
    ///
    /// Deliberately a no-op once the swap has happened (`Hold`/`In`) and while `Idle`: by then the
    /// caller's pending action has been applied and there is nothing left to withdraw — undoing it
    /// is a fresh request, not a rewound animation.
    ///
    /// Returns whether it cancelled anything, so a caller holding a typed pending value beside the
    /// fader drops it on exactly the frames the fader agrees it still can — and so an input that
    /// did NOT cancel anything can fall through to its normal handling instead of being swallowed.
    pub(crate) fn cancel(&mut self) -> bool {
        if self.phase == Phase::Out {
            self.phase = Phase::In; // `t` is kept: the ramp reverses, it does not restart
            true
        } else {
            false
        }
    }

    /// Advance one frame with the SAME `dt` the screen's springs get. `ready` = the incoming
    /// content is drawable (Library: `!browse::loading_initial()`).
    ///
    /// Returns `true` on **exactly one** frame — the frame the caller must apply its pending swap.
    ///
    /// A `dt` of 0 on a sub-millisecond frame simply leaves `Out` unadvanced for that frame; it
    /// cannot latch, because `SDL_GetTicks` is ms-resolution and the loop is ≥ 1 ms.
    pub(crate) fn tick(&mut self, dt: f32, ready: bool) -> bool {
        // This ramp integrates MILLISECONDS, not a spring — so `ui::idle`'s spring instrumentation
        // is structurally blind to it, and before this line a route dip largely did not play: the
        // present gate froze the panel on the last presented frame (the OUTGOING page at alpha≈1)
        // until the 2 s keepalive hard-cut to the destination. BACK is the worst case, because it
        // arms no press spring to accidentally cover the fade.
        //
        // Reported from the ADVANCE, never from `alpha()`: `nav::page_alpha()` is read at the root
        // of all four gated screens every frame and returns 1.0 at rest, so a read-reports ramp
        // would pin the loop forever. `Hold` is excluded because it is a genuine wait on data
        // (Library's deferred reload) where the screen is legitimately static.
        if matches!(self.phase, Phase::Out | Phase::In) {
            crate::ui::idle::invalidate();
        }
        match self.phase {
            Phase::Idle => {
                self.t = 1.0;
                false
            }
            Phase::Out => {
                self.t -= dt * 1000.0 / OUT_MS;
                if self.t <= 0.0 {
                    self.t = 0.0;
                    self.phase = Phase::Hold;
                    true // <- THE commit frame, the only `true` this ever returns
                } else {
                    false
                }
            }
            Phase::Hold => {
                self.t = 0.0;
                if ready {
                    self.phase = Phase::In;
                }
                false
            }
            Phase::In => {
                self.t += dt * 1000.0 / IN_MS;
                if self.t >= 1.0 {
                    self.t = 1.0;
                    self.phase = Phase::Idle;
                }
                false
            }
        }
    }

    /// The cascade alpha to push around the faded content
    /// ([`Painter::alpha`](crate::ui::Painter::alpha)). Smoothstepped so the dissolve eases at both
    /// ends while the commit deadline stays exact.
    pub(crate) fn alpha(&self) -> f32 {
        let t = self.t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// A swap is in flight — for a screen that must not treat a FOREIGN store change as a new
    /// reload while it is already running one.
    pub(crate) fn is_swapping(&self) -> bool {
        self.phase != Phase::Idle
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! Pure value semantics — with ONE exception that costs these tests their parallelism:
    //! `tick` reports to `ui::idle`'s process-global dirty flag (a ms ramp is invisible to the
    //! spring instrumentation, so it must say so itself). Driving an `Xfade` therefore mutates a
    //! crate global that `ui::idle`'s own "a settled screen does not repaint" assertions read, so
    //! every test here holds `crate::testlock::serial()` — the rule in `lib.rs::testlock`, not a
    //! precaution. Without it these would intermittently fail *other modules'* tests, which is the
    //! worst shape a flake can take.
    //!
    //! Nothing here draws, so what these CANNOT say is whether 70/140 ms and a smoothstep read
    //! right on the panel; that is a device capture.
    use super::*;

    /// One 60 Hz frame.
    const DT: f32 = 1.0 / 60.0;

    /// Did driving this fader ask the frame gate for a repaint? Consumes the flag, so each call
    /// answers for exactly the frames since the last one.
    ///
    /// `frame_begin` + `note_present` first, so neither leftover spring motion from another test on
    /// this thread nor the 2 s keepalive can answer in the fader's place — this must isolate the
    /// fader's own report and nothing else.
    fn asked_to_repaint() -> bool {
        crate::ui::idle::frame_begin(DT);
        crate::ui::idle::note_present(0);
        crate::ui::idle::should_present(0)
    }

    /// A running ramp MUST report, in both directions. This is the regression that shipped: the
    /// gate is spring-instrumented, `Xfade` integrates milliseconds, so a route dip froze on the
    /// outgoing page until the 2 s keepalive hard-cut to the destination.
    #[test]
    fn a_running_ramp_asks_for_every_frame_of_itself() {
        let _g = crate::testlock::serial();
        asked_to_repaint(); // drain whatever the previous test left on the shared flag
        let mut x = Xfade::new();
        x.reload(); // -> Out
        for f in 0..4 {
            x.tick(DT, true);
            assert!(asked_to_repaint(), "frame {f} of a fade-OUT must repaint");
        }
        let mut y = Xfade::new();
        y.arrive(); // -> In
        for f in 0..4 {
            y.tick(DT, true);
            assert!(asked_to_repaint(), "frame {f} of a fade-IN must repaint");
        }
    }

    /// ...and a fader at rest must NOT, or every screen holding one pins the loop at 60fps
    /// forever — the failure mode that costs the whole feature and that no floor-style gate sees.
    #[test]
    fn a_fader_at_rest_never_asks() {
        let _g = crate::testlock::serial();
        asked_to_repaint(); // drain whatever the previous test left on the shared flag
        let mut x = Xfade::new(); // Idle
        for _ in 0..8 {
            x.tick(DT, true);
        }
        assert!(
            !asked_to_repaint(),
            "an idle fader must not hold the panel awake"
        );
    }

    /// `Hold` is a genuine WAIT on data (Library's deferred reload), not motion: the screen is
    /// legitimately static at alpha 0, and reporting there would burn 60fps for as long as the
    /// server takes to answer.
    #[test]
    fn waiting_on_data_is_not_moving() {
        let _g = crate::testlock::serial();
        asked_to_repaint(); // drain whatever the previous test left on the shared flag
        let mut x = Xfade::new();
        x.mount(); // -> Hold
        for _ in 0..8 {
            x.tick(DT, false); // not ready — parked
        }
        assert!(
            !asked_to_repaint(),
            "a fader parked waiting for content must not repaint"
        );
        x.tick(DT, true); // Hold -> In
        x.tick(DT, true);
        assert!(
            asked_to_repaint(),
            "the fade-in must repaint once the content arrives"
        );
    }

    /// Drive `n` frames, returning (commit count, the alpha after each frame).
    fn run(x: &mut Xfade, n: usize, ready: bool) -> (usize, Vec<f32>) {
        let mut commits = 0;
        let mut a = Vec::with_capacity(n);
        for _ in 0..n {
            if x.tick(DT, ready) {
                commits += 1;
            }
            a.push(x.alpha());
        }
        (commits, a)
    }

    /// The whole contract in one pass: out, exactly one commit at the floor, back to full. The
    /// monotonicity assertions are what stop a "fix" that re-ramps `t` from re-introducing a
    /// flicker no static read of the code would catch.
    #[test]
    fn a_reload_fades_out_commits_exactly_once_then_comes_back_to_full() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.reload();
        let mut commits = 0;
        let mut commit_frame = usize::MAX;
        let mut prev = x.alpha();
        for f in 0..60 {
            if x.tick(DT, true) {
                commits += 1;
                commit_frame = f;
                assert_eq!(
                    x.alpha(),
                    0.0,
                    "the commit must happen AT the floor, not near it"
                );
            }
            let a = x.alpha();
            if f <= commit_frame {
                assert!(
                    a <= prev + 1e-6,
                    "frame {f}: alpha rose during the fade-out ({prev} → {a})"
                );
            } else {
                assert!(
                    a >= prev - 1e-6,
                    "frame {f}: alpha fell during the fade-in ({prev} → {a})"
                );
            }
            prev = a;
        }
        assert_eq!(commits, 1, "exactly one commit per reload");
        assert_eq!(x.alpha(), 1.0);
        assert!(!x.is_swapping());
    }

    /// The "hold the tab key" / fast double tab-switch regression: restarting `t` at 1.0 on every
    /// request would keep the grid dark for as long as the user keeps pressing, and would fire a
    /// commit per press. The ramp must continue from where it is, and the total time to the commit
    /// must be unchanged from a single reload.
    #[test]
    fn a_second_reload_mid_fade_out_does_not_restart_the_ramp() {
        let _g = crate::testlock::serial();
        // baseline: frames to the commit for one uninterrupted reload
        let mut base = Xfade::new();
        base.reload();
        let mut want = 0usize;
        for f in 0..60 {
            if base.tick(DT, true) {
                want = f;
                break;
            }
        }

        let mut x = Xfade::new();
        x.reload();
        let mut commits = 0;
        let mut got = usize::MAX;
        let mut prev = x.alpha();
        for f in 0..60 {
            if f == 2 {
                x.reload(); // the second press, mid fade-out
            }
            if x.tick(DT, true) {
                commits += 1;
                got = f;
            }
            let a = x.alpha();
            if got == usize::MAX {
                assert!(
                    a <= prev + 1e-6,
                    "frame {f}: the ramp restarted ({prev} → {a})"
                );
            }
            prev = a;
        }
        assert_eq!(
            commits, 1,
            "two presses inside one fade must commit ONCE, to the last one"
        );
        assert_eq!(got, want, "the second request must not extend the fade-out");
    }

    /// `Out` entered from `t == 0` must resolve in a single frame rather than stall: this is the
    /// "a second reload arrives before the first content does" branch, and the proof that a
    /// request while parked is not a lost press.
    #[test]
    fn a_reload_while_parked_at_zero_commits_on_the_very_next_frame() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.reload();
        while !x.tick(DT, false) {} // drive to the floor; content never arrives
        let (commits, alphas) = run(&mut x, 5, false);
        assert_eq!(commits, 0, "Hold must not commit on its own");
        assert!(alphas.iter().all(|a| *a == 0.0));

        x.reload();
        assert!(
            x.tick(DT, false),
            "a reload from the floor commits on the next frame"
        );
        assert_eq!(x.alpha(), 0.0, "and nothing flickers on the way");
    }

    /// The nit's second trap. `Hold` waits on `ready` forever if it must — the screen's own
    /// spinner is outside the faded band and the store retries by itself — but the moment content
    /// exists the fader has to come back to full on its own, with no further input.
    #[test]
    fn the_fade_never_wedges_at_zero_when_the_content_never_arrives() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.reload();
        while !x.tick(DT, false) {}
        let (commits, alphas) = run(&mut x, 600, false);
        assert_eq!(commits, 0);
        assert!(alphas.iter().all(|a| *a == 0.0), "parked, not drifting");
        assert!(
            x.is_swapping(),
            "still mid-swap: the screen knows it is waiting"
        );

        let (commits, _) = run(&mut x, 12, true);
        assert_eq!(commits, 0, "arrival is not a second commit");
        assert_eq!(
            x.alpha(),
            1.0,
            "IN_MS = 140 ms is ≈9 frames; 12 is the whole ramp plus slack"
        );
        assert!(!x.is_swapping());
    }

    /// Route entry must never fire a swap it did not queue — `mount` exists precisely for the case
    /// where there IS no outgoing content, so a stray commit there would apply whatever the screen
    /// happened to have left in its pending slot.
    #[test]
    fn mount_never_commits() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.mount();
        let (commits, alphas) = run(&mut x, 30, false);
        assert_eq!(commits, 0);
        assert!(alphas.iter().all(|a| *a == 0.0));
        let (commits, _) = run(&mut x, 12, true);
        assert_eq!(commits, 0);
        assert_eq!(x.alpha(), 1.0);
    }

    /// The half an async landing wants (content the screen did not schedule): ramp in, no commit.
    #[test]
    fn arrive_ramps_in_without_a_commit() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.arrive();
        assert_eq!(x.alpha(), 0.0);
        let (commits, alphas) = run(&mut x, 12, true);
        assert_eq!(commits, 0);
        assert!(
            alphas.windows(2).all(|w| w[1] >= w[0] - 1e-6),
            "monotone in"
        );
        assert_eq!(x.alpha(), 1.0);
    }

    /// The withdrawal contract: a cancelled fade-out must come back to full having committed
    /// NOTHING. The monotonicity assertion is what stops a "fix" that re-enters `Out` (or restarts
    /// `t`) from re-introducing a flicker no static read of the code would catch.
    #[test]
    fn cancel_withdraws_a_fade_out_without_ever_committing() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.reload();
        let (commits, _) = run(&mut x, 2, true);
        assert_eq!(
            commits, 0,
            "70 ms is ≥ 2 frames at 60 Hz — nothing has committed yet"
        );
        assert!(x.cancel(), "a fade-out in flight IS withdrawable");

        let (commits, alphas) = run(&mut x, 20, true);
        assert_eq!(
            commits, 0,
            "a withdrawn transition must never apply its pending action"
        );
        assert!(
            alphas.windows(2).all(|w| w[1] >= w[0] - 1e-6),
            "monotone back to full: {alphas:?}"
        );
        assert_eq!(x.alpha(), 1.0);
        assert!(!x.is_swapping());
    }

    /// The reverse of `a_second_reload_mid_fade_out_does_not_restart_the_ramp`, and the reason
    /// `cancel` keeps `t`: a press the user takes back after two frames should look like two frames
    /// of motion undone, not like a whole fresh dissolve.
    #[test]
    fn a_withdrawal_costs_only_what_the_fade_had_already_spent() {
        let _g = crate::testlock::serial();
        let mut full = Xfade::new();
        full.arrive(); // a whole IN_MS ramp from the floor
        let mut want = usize::MAX;
        for f in 0..60 {
            full.tick(DT, true);
            if full.alpha() >= 1.0 {
                want = f;
                break;
            }
        }

        let mut x = Xfade::new();
        x.reload();
        run(&mut x, 2, true);
        assert!(x.cancel());
        let mut got = usize::MAX;
        for f in 0..60 {
            x.tick(DT, true);
            if x.alpha() >= 1.0 {
                got = f;
                break;
            }
        }
        assert!(
            got < want,
            "the ramp restarted ({got} frames back to full vs {want} for a full IN_MS)"
        );
    }

    /// "BACK after the commit is an ordinary BACK, not a rewind." Once the swap has happened the
    /// caller's pending action is spent, so the fader must refuse to un-run it — and must still
    /// finish its own fade-in whichever phase the refusal landed in.
    #[test]
    fn cancel_is_refused_once_the_swap_has_happened() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        assert!(!x.cancel(), "nothing in flight");

        x.reload();
        while !x.tick(DT, false) {} // drive to the floor; content never arrives, so we park in Hold
        assert!(
            !x.cancel(),
            "the swap is applied — there is nothing left to withdraw"
        );
        let (commits, _) = run(&mut x, 12, true); // content lands: Hold -> In
        assert_eq!(commits, 0);
        assert_eq!(
            x.alpha(),
            1.0,
            "it still finishes on its own after the refusal"
        );

        x.reload();
        while !x.tick(DT, true) {}
        x.tick(DT, true); // Hold -> In
        assert!(
            !x.cancel(),
            "mid fade-IN is past the point of no return too"
        );
        let (commits, _) = run(&mut x, 12, true);
        assert_eq!(commits, 0);
        assert_eq!(x.alpha(), 1.0);
        assert!(!x.is_swapping());
    }

    /// `app.rs` clamps `dt` at 0.05 s, so a stalled frame hands the fader 3.5 whole `OUT_MS`
    /// worth of time at once. The latches must absorb it: one commit, alpha never outside 0..1,
    /// and the transition still finishes. Frame budget: 2 out (0.714 of the ramp each) + 1 to
    /// leave `Hold` + 3 in (0.357 each) = 6.
    #[test]
    fn a_dt_spike_completes_without_overshooting() {
        let _g = crate::testlock::serial();
        let mut x = Xfade::new();
        x.reload();
        let mut commits = 0;
        for f in 0..8 {
            if x.tick(0.05, true) {
                commits += 1;
            }
            let a = x.alpha();
            assert!(
                (0.0..=1.0).contains(&a),
                "frame {f}: alpha {a} escaped 0..1"
            );
        }
        assert_eq!(commits, 1);
        assert_eq!(x.alpha(), 1.0);
        assert!(!x.is_swapping());
    }
}
