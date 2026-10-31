//! **What a scrolled card page does with the chrome around it** — ONE rule for the pages that
//! scroll a long grid under the tucking top chrome (the Library, its Collections page, the
//! Watchlist): the chrome tucks away once the first row has gone, a fade of the page's ground takes
//! the top edge, and a capsule restating the library's name and size (and the sort it is read in, "Films 1,284 · Date viewed") appears only where the page's
//! header names libraries ([`capsule_shown`]).
//!
//! **Home keeps its chrome and uses only the spatial half.** Its tab bar and profile chip are the
//! landing page's navigation, so Home owns no [`Handoff`] and never tucks (`Screen::chrome_tuck`
//! stays 0); its shelf headings ride the page under the shown chrome and fade by where they stand,
//! through [`clearance_under_shown_chrome`], the very rule a Handoff's [`clearance`](Handoff::clearance)
//! is at tuck 0. A page whose chrome DOES tuck owns the state below.
//!
//! **A clock, not the scroll's position.** [`Handoff`] is a small state a page owns and steps once
//! a frame. It used to be two pure functions of the scroll position, which made the handoff exactly
//! as fast as the exponential scroll spring happens to be: the capsule took 40 frames to arrive
//! going down and 3 to leave coming up, the bar 5 to leave and 2 to return, 2 each under key
//! repeat, and one frame of the way back had no header at all. Now:
//!
//! - **The state is decided on the key frame, from the scroll TARGET** ([`Handoff::step`]): tucked
//!   once the target reaches [`TUCK_AT`] short of the first row's pitch, shown again once it falls
//!   to [`UNTUCK_AT`] short of it. The gap between the two is the hysteresis, so a target hovering
//!   at the threshold cannot flap the chrome. The scroll spring is no longer in the loop: it moves
//!   the page, the clock moves the chrome.
//! - **The two glasses run in sequence on one fixed clock, the same in both directions**: the
//!   departing glass fades out over [`OUT_MS`](crate::xfade) and, in the very frame its DRAWN alpha
//!   is exactly 0, the arriving one starts to fade in over [`IN_MS`](crate::xfade) — the swap clock
//!   every content cross-fade in this UI already runs on, leave fast and arrive gently. Down is the
//!   chrome out, then the capsule in; Up is the capsule out, then the chrome in. At 60 Hz that is 5
//!   frames out and 9 in, 13 in all, either way.
//! - **A reversal reverses from where the alphas are**: they are values, not phases, so a flip
//!   mid-handoff takes the glass that is partway out back in (or the one partway in back out)
//!   without restarting a ramp, and the gate below holds across it.
//! - **A jump, a restore or a first arrival does not play it.** When the decision changes on a
//!   frame the scroll is already AT its target (`scroll.jump`: a saved viewport, a page arriving
//!   deep), the alphas snap to their destination. Only a glide plays the clock.
//!
//! **The chrome and the capsule hand the place over in sequence, never together.** The chrome band
//! and the capsule are two live glasses whose blur footprints meet, and two such glasses never share
//! a frame: the upper one cannot run its direct source job (a lower live glass lies inside its
//! footprint), so it captures INLINE in the visible pass, a mid-frame framebuffer copy and the
//! six-pass chain, about 1.4M GPU cycles per frame more than its direct job (measured on the
//! television: 25.6% of the old ramp's frames late, 1.6% once the two were sequential). The gate is
//! on DRAWN alphas, never on scroll: the arriving glass advances only once the departing one is
//! exactly 0, so the chrome band is never on screen (the dispatcher skips a chrome at alpha 0) in a
//! frame the capsule is. A page without a capsule keeps the chrome's own times; there is simply
//! nothing to wait for on the way back.
//!
//! **Why the shared swap clock's constants and not [`crate::xfade::Xfade`] itself.** That fader
//! commits a content swap, so it parks one frame at the floor (`Hold`) and hands out a
//! smoothstepped alpha. Run as the handoff it leaves FOUR frames with neither glass at 10% (the
//! last of the out ramp at 0.007, the commit frame, the hold frame, the first of the in ramp at
//! 0.04) where the header must not be bare for more than the one frame the sequence itself needs.
//! So the handoff shares the two durations and the millisecond ramp, and the alphas stay linear:
//! steepest at the floor, with no hold.
//!
//! **A row that rides the page keeps out of the band the chrome is drawn in** ([`Handoff::clearance`]).
//! The chrome returns on the clock above, decided on the key frame, but a page's header row rides the
//! scroll spring, which is still gliding home for a dozen frames after the chrome is whole: the
//! profile chip was painted over the Watchlist's title, and over the Library's first pill, while the
//! row passed through y 62..122 (design review F3). The row cannot wait for the chrome (it rides the
//! page) and the chrome must not wait for the row (the clock above), so the row is faded by where it
//! stands: whole once its top is [`CLEARANCE_RAMP`] below the bottom edge of the chrome AS DRAWN
//! (the band's resting bottom, [`TOP_BAR_BOTTOM`](crate::widgets::TOP_BAR_BOTTOM), less the lift
//! the tuck has put on it, `screen::chrome_lift`), gone where it would touch it. The edge is a value
//! of the drawn chrome, not a state, so a page whose row is not scaled by the chrome's own alpha (the
//! Library's) meets no pop when the chrome finishes leaving: the edge has lifted off the row by then.
//! This is a spatial rule and not a second timeline: it has no clock, no state and no say in when
//! the chrome is shown.
//!
//! A resting page asks for no further frames ([`Handoff::step`] reports motion only while an alpha
//! is on its way). The pixels are [`crate::scroll_capsule`] (the nav pill's live glass); this file
//! owns WHEN and WHAT.
//!
//! The module lives here, not in `plx_screens`, because a screen family may not name a sibling
//! module of the crate (`ci/check-deps.sh`, "sibling").
use std::ffi::{CStr, CString};

use plx_machine::machine::{Canon, LogicalState};

use crate::xfade::{IN_MS, OUT_MS};

/// How far short of the first row's pitch (`gone_at`) the scroll TARGET must have reached for the
/// page to tuck: past this the first row is on its way off the top, and the chrome starts to leave.
pub const TUCK_AT: f32 = 60.0;
/// How far short of the first row's pitch the target must have fallen to untuck. Further from the
/// pitch than [`TUCK_AT`], so the 60 px between them is the hysteresis.
pub const UNTUCK_AT: f32 = 120.0;

/// How far below the drawn chrome band a page-riding row has to stand to be whole: the air a header
/// row rests with under [`TOP_BAR_BOTTOM`](crate::widgets::TOP_BAR_BOTTOM) (the row's top, `page_header::TOP`,
/// less the band's bottom), so the row is exactly whole at its rest and the ramp is the row's own
/// last 18 px of travel.
pub const CLEARANCE_RAMP: f32 = crate::page_header::TOP - crate::widgets::TOP_BAR_BOTTOM;

/// The page's chrome and capsule, as drawn, and the state that decides them.
///
/// `tucked` is the DECIDED state (what the chrome and the capsule are heading for); `chrome` and
/// `capsule` are what is drawn, each 0..1 and linear in time. At rest exactly one of the pairs
/// `(chrome 1, capsule 0)` or `(chrome 0, capsule 1 | 0)` holds, and a chrome above 0 means a
/// capsule of exactly 0: the invariant the cost model rests on, kept by construction in
/// [`step`](Handoff::step) and not by two ranges happening to abut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handoff {
    tucked: bool,
    chrome: f32,
    capsule: f32,
}

impl Default for Handoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Handoff {
    /// A page at rest at its top: the chrome shown, no capsule.
    pub const fn new() -> Self {
        Handoff { tucked: false, chrome: 1.0, capsule: 0.0 }
    }

    /// One frame, after the page has stepped its scroll spring. `dt` is the frame's, `pos` and
    /// `target` the scroll's position and where it is heading, `gone_at` the scroll at which
    /// whatever stands under the chrome at rest has scrolled off (the first row's pitch, since a
    /// scrolled grid rests a whole row down at the first rest), and `capsule_worn` whether the page
    /// wears a capsule ([`capsule_shown`]).
    pub fn step(&mut self, dt: f32, pos: f32, target: f32, gone_at: f32, capsule_worn: bool) {
        let before = *self;
        let want = if self.tucked { target > untuck_below(gone_at) } else { target >= tuck_from(gone_at) };
        if want != self.tucked {
            self.tucked = want;
            // The scroll is already where it is going: a jump, a restore, a page arriving deep.
            // Nothing is gliding, so there is nothing to hand over.
            if (pos - target).abs() < 0.5 {
                self.chrome = if want { 0.0 } else { 1.0 };
                self.capsule = if want && capsule_worn { 1.0 } else { 0.0 };
                if *self != before {
                    plx_machine::idle::invalidate();
                }
                return;
            }
        }
        let out = dt * 1000.0 / OUT_MS;
        let into = dt * 1000.0 / IN_MS;
        if self.tucked {
            self.chrome = (self.chrome - out).max(0.0);
            if !capsule_worn {
                self.capsule = (self.capsule - out).max(0.0);
            } else if self.chrome == 0.0 {
                self.capsule = (self.capsule + into).min(1.0);
            }
        } else {
            self.capsule = (self.capsule - out).max(0.0);
            if self.capsule == 0.0 {
                self.chrome = (self.chrome + into).min(1.0);
            }
        }
        // A ms ramp is invisible to the spring instrumentation (`xfade`'s reasoning): say so while
        // an alpha is on its way, the frame it arrives included, and never once it has finished.
        if *self != before || !self.at_rest(capsule_worn) {
            plx_machine::idle::invalidate();
        }
    }

    fn at_rest(&self, capsule_worn: bool) -> bool {
        if self.tucked {
            self.chrome == 0.0 && self.capsule == if capsule_worn { 1.0 } else { 0.0 }
        } else {
            self.capsule == 0.0 && self.chrome == 1.0
        }
    }

    /// How much of the top chrome is shown (0 gone .. 1 shown).
    pub fn chrome(&self) -> f32 {
        self.chrome
    }

    /// How far the top chrome is tucked (0 shown .. 1 gone): what the dispatcher reads
    /// (`Screen::chrome_tuck`) and the top fade follows.
    pub fn tuck(&self) -> f32 {
        1.0 - self.chrome
    }

    /// **How much of a page-riding row whose top stands at `top` (screen y) the drawn chrome leaves
    /// clear**, 0..1: 0 while the row's top is at or above the bottom edge of the chrome as it is
    /// drawn now, 1 from [`CLEARANCE_RAMP`] below it, a smoothstep between. The edge is the band's
    /// resting bottom ([`TOP_BAR_BOTTOM`](crate::widgets::TOP_BAR_BOTTOM)) less the lift the tuck has put on it
    /// ([`chrome_lift`](crate::screen::chrome_lift)): a shown chrome keeps a row out of y 0..130, a
    /// chrome that has left keeps it out of nothing. Pure in the chrome's drawn value and the row's
    /// place, so it is continuous under a reversal and under a jump. A page multiplies its header
    /// row's alpha by it (the Library's `draw_header`, the Watchlist's head).
    pub fn clearance(&self, top: f32) -> f32 {
        let edge = crate::widgets::TOP_BAR_BOTTOM - crate::screen::chrome_lift(self.tuck());
        let t = ((top - edge) / CLEARANCE_RAMP).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// The alpha a header row wears when the page has put it `dy` px from its resting place (the
    /// section's own top, scroll included: `-scroll` for a row at the document's head): the chrome's
    /// own shown value, so a row that leaves with the chrome leaves on its clock, times what the
    /// drawn chrome leaves [`clear`](Self::clearance) at the row's top, `page_header::TOP + dy`.
    pub fn head_alpha(&self, dy: f32) -> f32 {
        self.chrome * self.clearance(crate::page_header::TOP + dy)
    }

    /// How far the capsule has arrived (0 absent .. 1 shown). It is above 0 only while
    /// [`chrome`](Self::chrome) is exactly 0.
    pub fn capsule(&self) -> f32 {
        self.capsule
    }
}

/// **[`Handoff::clearance`] for a page whose chrome never tucks** (Home): how much of a page-riding
/// row whose top stands at `top` (screen y) the SHOWN chrome leaves clear, 0..1 — 0 while the top
/// is at or above the band's resting bottom ([`TOP_BAR_BOTTOM`](crate::widgets::TOP_BAR_BOTTOM),
/// 130), 1 from [`CLEARANCE_RAMP`] below it, a smoothstep between. Pure in the row's place, so it
/// is continuous under a reversal and a jump, and it has no clock and no state: a heading that
/// rides up the page fades by where it stands and is gone before it reaches the chip.
pub fn clearance_under_shown_chrome(top: f32) -> f32 {
    Handoff::new().clearance(top)
}

/// The scroll target from which the chrome tucks. `gone_at` is floored at the hysteresis span so a
/// page whose layout has not measured a pitch yet still rests untucked at scroll 0.
fn tuck_from(gone_at: f32) -> f32 {
    gone_at.max(UNTUCK_AT) - TUCK_AT
}

/// The scroll target at or below which a tucked chrome returns.
fn untuck_below(gone_at: f32) -> f32 {
    gone_at.max(UNTUCK_AT) - UNTUCK_AT
}

impl LogicalState for Handoff {
    fn write(&self, w: &mut Canon) {
        w.bool(self.tucked).f32(self.chrome).f32(self.capsule);
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!("handoff tucked={} chrome={} capsule={}", self.tucked, self.chrome, self.capsule));
    }
}

/// Does the page show the capsule? It restates the library the header row names, so only a header
/// that names libraries to choose between (`selectable`: the names in its row, the `+N` overflow
/// stop counted) has anything to restate. A one-library page and a page whose header names none
/// (the Watchlist) draw no capsule, though their chrome still tucks and their top fade stays.
pub fn capsule_shown(selectable: usize) -> bool {
    selectable >= 2
}

/// The capsule's runs: the library's whole name (never shortened: the capsule grows to hold it),
/// the count, and the sort `criterion` the page carries ("Date viewed"; `None` for a page that
/// carries none, which draws the plain capsule).
pub fn capsule_parts(name: &str, total: i64, criterion: Option<&str>) -> (CString, CString, Option<CString>) {
    let count = plx_platform::i18n::current().number(total);
    (
        CString::new(name).unwrap_or_default(),
        CString::new(count).unwrap_or_default(),
        criterion.map(|criterion| CString::new(criterion).unwrap_or_default()),
    )
}

/// **The capsule's strings, kept while their text stays the same.** A page asks for the three C
/// strings ([`capsule_parts`]) on every frame the capsule is up, but the library's name, the
/// listing's size and the sort change only when the reader does something; this holds the last
/// answer and rebuilds it when the name, the total or the criterion differs, or the interface
/// language does (the count is spelled in it). The page owns one beside its handoff, so a library
/// switch builds the next library's strings once.
#[derive(Default)]
pub struct CapsuleText {
    /// What `parts` was built for: the pieces the name is made of, the total, the criterion and
    /// the language.
    key: Option<(Vec<String>, i64, Option<String>, usize)>,
    parts: (CString, CString, Option<CString>),
    /// How many times the strings were built: the probe that says "built when the text changes,
    /// not every frame" without reading addresses.
    builds: u32,
}

impl CapsuleText {
    /// The strings for `name`, `total` and the sort `criterion`: the held ones when nothing
    /// changed, else [`capsule_parts`] of them.
    pub fn get(&mut self, name: &str, total: i64, criterion: Option<&str>) -> (&CStr, &CStr, Option<&CStr>) {
        self.get_keyed(&[name], || name.to_owned(), total, criterion)
    }

    /// [`Self::get`] for a name that costs a heap string to put together: `key` is the borrowed
    /// pieces the name is made of (a library's title and its owner's handle), compared as they
    /// are, and `build` makes the name from them only when they, the total, the criterion or the
    /// language differ. A `build` must read nothing the key does not name, or the held strings go
    /// stale: it takes the pieces by capture, never the page.
    pub fn get_keyed(&mut self, key: &[&str], build: impl FnOnce() -> String, total: i64, criterion: Option<&str>) -> (&CStr, &CStr, Option<&CStr>) {
        let language = std::ptr::from_ref(plx_platform::i18n::current()) as usize;
        let held = self.key.as_ref().is_some_and(|(pieces, t, c, l)| {
            pieces.len() == key.len() && pieces.iter().zip(key).all(|(piece, now)| piece == now)
                && *t == total && c.as_deref() == criterion && *l == language
        });
        if !held {
            self.parts = capsule_parts(&build(), total, criterion);
            self.key = Some((key.iter().map(|piece| (*piece).to_owned()).collect(), total, criterion.map(str::to_owned), language));
            self.builds += 1;
        }
        (&self.parts.0, &self.parts.1, self.parts.2.as_deref())
    }

    /// How many times the strings have been built (see the field).
    pub fn builds(&self) -> u32 { self.builds }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PITCH: f32 = 480.0;

    /// 60 Hz, the frame the pages tick at.
    const DT: f32 = 1.0 / 60.0;

    /// One leg of a trip: `(leaving, arriving)` drawn alphas per frame, the glass on its way out
    /// first. The key frame is frame 0.
    type Leg = Vec<(f32, f32)>;

    /// The drawn alphas on each frame of a Down past the threshold and an Up back, with the REAL
    /// scroll spring a page owns (`consts::K_SCROLL`) stepped toward the target once a frame, then
    /// `model(pos, target)` asked for `(chrome alpha, capsule alpha)`.
    fn trip(mut model: impl FnMut(f32, f32) -> (f32, f32)) -> (Leg, Leg) {
        let mut spring = crate::Spring::at(0.0);
        let mut leg = |target: f32| -> Vec<(f32, f32)> {
            (0..150).map(|_| { spring.step(target, crate::consts::K_SCROLL, DT); model(spring.pos, target) }).collect()
        };
        let down = leg(PITCH);
        let up = leg(0.0).into_iter().map(|(chrome, capsule)| (capsule, chrome)).collect::<Vec<_>>();
        (down, up)
    }

    /// Frames on which a glass is partly transparent (0 < alpha < 1): its half of the handoff.
    fn frames_between(leg: &[(f32, f32)], pick: fn(&(f32, f32)) -> f32) -> Vec<f32> {
        leg.iter().map(pick).filter(|a| *a > 0.0 && *a < 1.0).collect()
    }

    /// Frames from the key frame until neither alpha is still moving.
    fn frames_to_rest(leg: &[(f32, f32)]) -> usize {
        let last = *leg.last().unwrap();
        leg.iter().rposition(|(a, b)| (a - last.0).abs() > 1e-3 || (b - last.1).abs() > 1e-3).map_or(1, |i| i + 2)
    }

    /// What is wrong with one leg of a trip, as sentences (empty when it is right): the handoff
    /// runs on a fixed clock in sequence, the same in both directions.
    fn faults(name: &str, leg: &[(f32, f32)]) -> Vec<String> {
        let mut out = Vec::new();
        for (i, (leaving, arriving)) in leg.iter().enumerate() {
            if *leaving > 0.0 && *arriving > 0.0 {
                out.push(format!("{name} frame {i}: both glasses on screen (leaving {leaving:.3}, arriving {arriving:.3})"));
            }
        }
        // Neither glass past 10% for more than the single frame the sequence itself needs is a bare header.
        let bare = leg.iter().filter(|(a, b)| *a < 0.1 && *b < 0.1).count();
        if bare > 1 {
            out.push(format!("{name}: {bare} frames with neither glass at 10%"));
        }
        for (which, half) in [("leaving", frames_between(leg, |f| f.0)), ("arriving", frames_between(leg, |f| f.1))] {
            if half.len() < 4 {
                out.push(format!("{name}: the {which} glass is partly transparent on only {} frames", half.len()));
            }
            let monotone = if which == "leaving" { half.windows(2).all(|w| w[1] <= w[0]) } else { half.windows(2).all(|w| w[1] >= w[0]) };
            if !monotone {
                out.push(format!("{name}: the {which} glass's alpha is not monotone"));
            }
        }
        out
    }

    /// **The handoff is a clock, not the scroll's position**: a Down past the threshold and an Up
    /// back, with the real scroll spring, take the same number of frames each way, each glass's
    /// half runs at least four frames with a monotone alpha, the two glasses are never on screen
    /// together (two live glasses sharing a frame cost +1.4M GPU cycles on the television), and the
    /// header is never bare for more than the one frame between the halves. Driven by the page's
    /// own tick order: the spring steps, then the chrome follows it.
    #[test]
    fn the_handoff_runs_on_one_fixed_clock_the_same_each_way() {
        let _g = plx_base::testlock::serial();
        let mut handoff = Handoff::new();
        let (down, up) = trip(|pos, target| {
            handoff.step(DT, pos, target, PITCH, true);
            (handoff.chrome(), handoff.capsule())
        });
        let mut faults = faults("Down", &down);
        faults.extend(self::faults("Up", &up));
        let (frames_down, frames_up) = (frames_to_rest(&down), frames_to_rest(&up));
        if frames_down.abs_diff(frames_up) > 1 {
            faults.push(format!("Down takes {frames_down} frames and Up {frames_up}"));
        }
        assert!(faults.is_empty(), "{}", faults.join("; "));
    }

    /// Did driving the handoff ask the frame gate for a repaint? Consumes the flag, so each call
    /// answers for the frames since the last one (`xfade`'s tests, same reason).
    fn asked_to_repaint() -> bool {
        plx_machine::idle::frame_begin(DT);
        plx_machine::idle::note_present(0);
        plx_machine::idle::should_present(0)
    }

    /// Run `frames` frames of a page whose scroll is `pos` and heads for `target`.
    fn run(h: &mut Handoff, frames: usize, pos: f32, target: f32, worn: bool) {
        for _ in 0..frames {
            h.step(DT, pos, target, PITCH, worn);
        }
    }

    /// The frame-by-frame table at 60 Hz, pinned: the chrome leaves in four partial frames and is
    /// exactly 0 on the fifth, and on THAT frame the capsule's first step is already drawn (so the
    /// sequence leaves the one frame it needs and no more); the capsule then arrives over nine.
    #[test]
    fn the_sixty_hertz_table_is_five_frames_out_then_nine_in_with_no_hold() {
        let _g = plx_base::testlock::serial();
        let mut h = Handoff::new();
        let mut table = Vec::new();
        for _ in 0..14 {
            h.step(DT, 100.0, PITCH, PITCH, true);
            table.push((h.chrome(), h.capsule()));
        }
        let chrome_zero = table.iter().position(|(c, _)| *c == 0.0).unwrap();
        assert_eq!(chrome_zero, 4, "the chrome is exactly 0 on the fifth frame: {table:?}");
        assert!(table[..4].iter().all(|(c, k)| *c > 0.0 && *k == 0.0), "no capsule while any chrome is drawn: {table:?}");
        assert!(table[4].1 > 0.1, "the capsule's first step is drawn on the frame the chrome reaches 0: {table:?}");
        assert_eq!(table[12], (0.0, 1.0), "and it has arrived by the thirteenth: {table:?}");
        assert!(table[11].1 < 1.0, "not before: {table:?}");
    }

    /// **The invariant the cost rests on, at every frame rate and through every reversal**: a
    /// chrome above 0 means a capsule of exactly 0, so the chrome band is never on screen in a
    /// frame the capsule is. The target is flipped at every frame offset of a handoff in both
    /// directions (a reversal mid-ramp), at several frame times, and no alpha moves by more than
    /// one step of its own ramp in a frame (the clock reverses from where the alphas are; it never
    /// restarts or jumps).
    #[test]
    fn the_two_glasses_are_never_on_screen_together_through_any_reversal_at_any_frame_rate() {
        let _g = plx_base::testlock::serial();
        for dt in [1.0 / 30.0, 1.0 / 50.0, DT, 1.0 / 120.0, 0.05, 0.003] {
            let step_cap = dt * 1000.0 / OUT_MS + 1e-5;
            for flip_at in 0..40 {
                for start_tucked in [false, true] {
                    let mut h = Handoff::new();
                    let (mut pos, mut target) = (0.0, 0.0);
                    if start_tucked {
                        pos = PITCH;
                        target = PITCH;
                        h.step(dt, pos, target, PITCH, true); // a jump: lands tucked
                    }
                    let mut last = (h.chrome(), h.capsule());
                    for frame in 0..160 {
                        if frame == 0 {
                            target = if start_tucked { 0.0 } else { PITCH };
                            pos = PITCH * 0.5; // gliding: the scroll is not at its target
                        } else if frame == flip_at {
                            target = if target > 0.0 { 0.0 } else { PITCH };
                        }
                        h.step(dt, pos, target, PITCH, true);
                        let now = (h.chrome(), h.capsule());
                        assert!(now.0 == 0.0 || now.1 == 0.0, "dt {dt}, flip {flip_at}, frame {frame}: chrome {} with capsule {}", now.0, now.1);
                        assert!((0.0..=1.0).contains(&now.0) && (0.0..=1.0).contains(&now.1));
                        assert!((now.0 - last.0).abs() <= step_cap && (now.1 - last.1).abs() <= step_cap, "dt {dt}, flip {flip_at}, frame {frame}: {last:?} -> {now:?}");
                        last = now;
                    }
                    let rest = if target > 0.0 { (0.0, 1.0) } else { (1.0, 0.0) };
                    assert_eq!(last, rest, "dt {dt}, flip {flip_at}: settles at the decided state");
                }
            }
        }
    }

    /// **A target hovering at the threshold cannot flap the chrome**: it tucks at `gone_at - 60`
    /// and returns only at `gone_at - 120`, so a target wobbling inside the 60 px between never
    /// reverses the handoff.
    #[test]
    fn a_target_hovering_at_the_threshold_cannot_flap_the_chrome() {
        let _g = plx_base::testlock::serial();
        let (tuck_from, untuck_below) = (PITCH - TUCK_AT, PITCH - UNTUCK_AT);
        let mut h = Handoff::new();
        run(&mut h, 40, 1.0, tuck_from - 1.0, true);
        assert_eq!((h.chrome(), h.capsule()), (1.0, 0.0), "just short of the threshold: shown");
        run(&mut h, 40, 1.0, tuck_from, true);
        assert_eq!((h.chrome(), h.capsule()), (0.0, 1.0), "at the threshold: tucked");
        for wobble in [tuck_from - 1.0, tuck_from - 30.0, untuck_below + 1.0, tuck_from, tuck_from + 5.0, untuck_below + 0.5] {
            run(&mut h, 3, 1.0, wobble, true);
            assert_eq!((h.chrome(), h.capsule()), (0.0, 1.0), "target {wobble} is inside the hysteresis: still tucked");
        }
        run(&mut h, 40, 1.0, untuck_below, true);
        assert_eq!((h.chrome(), h.capsule()), (1.0, 0.0), "at the lower threshold: shown again");
        run(&mut h, 3, 1.0, tuck_from - 1.0, true);
        assert_eq!((h.chrome(), h.capsule()), (1.0, 0.0), "and the same wobble from below does not tuck");
    }

    /// A jump, a restore or a page that arrives deep (the scroll is already AT its target) lands
    /// on the decided state with no handoff played; a glide plays it.
    #[test]
    fn a_jump_lands_on_the_decided_state_and_only_a_glide_plays_the_handoff() {
        let _g = plx_base::testlock::serial();
        let mut jumped = Handoff::new();
        jumped.step(DT, PITCH * 3.0, PITCH * 3.0, PITCH, true);
        assert_eq!((jumped.chrome(), jumped.capsule()), (0.0, 1.0), "arrived deep: tucked on the first frame");
        jumped.step(DT, 0.0, 0.0, PITCH, true);
        assert_eq!((jumped.chrome(), jumped.capsule()), (1.0, 0.0), "jumped home: shown on the first frame");
        let mut glided = Handoff::new();
        glided.step(DT, 1.0, PITCH, PITCH, true);
        assert!(glided.chrome() > 0.0 && glided.capsule() == 0.0, "a glide starts the chrome out, not at its destination");
        let mut bare = Handoff::new();
        bare.step(DT, PITCH, PITCH, PITCH, false);
        assert_eq!((bare.chrome(), bare.capsule()), (0.0, 0.0), "a jump to a page with no capsule lands on no capsule");
    }

    /// A page without a capsule (the Watchlist, a one-library page) tucks the chrome at the
    /// chrome's own pace and never draws a capsule; coming back there is nothing to wait for, so
    /// the chrome starts to return on the key frame.
    #[test]
    fn a_page_without_a_capsule_hands_nothing_over_and_waits_for_nothing() {
        let _g = plx_base::testlock::serial();
        let mut h = Handoff::new();
        for _ in 0..30 {
            h.step(DT, 1.0, PITCH, PITCH, false);
            assert_eq!(h.capsule(), 0.0, "no capsule is ever drawn");
        }
        assert_eq!(h.chrome(), 0.0);
        h.step(DT, PITCH, 0.0, PITCH, false);
        assert!(h.chrome() > 0.0, "the chrome returns on the key frame: {}", h.chrome());
    }

    /// A page that LOSES its capsule while tucked lets it go at the out pace rather than cutting it.
    #[test]
    fn a_capsule_the_page_stops_wearing_fades_out_where_it_stands() {
        let _g = plx_base::testlock::serial();
        let mut h = Handoff::new();
        run(&mut h, 40, 1.0, PITCH, true);
        assert_eq!((h.chrome(), h.capsule()), (0.0, 1.0));
        h.step(DT, PITCH, PITCH, PITCH, false);
        assert!(h.capsule() > 0.0 && h.capsule() < 1.0 && h.chrome() == 0.0, "{} {}", h.chrome(), h.capsule());
        run(&mut h, 10, PITCH, PITCH, false);
        assert_eq!((h.chrome(), h.capsule()), (0.0, 0.0));
    }

    /// A running handoff asks for every frame of itself, in both directions; a page at rest asks
    /// for none (the clock integrates milliseconds, which the spring instrumentation cannot see).
    #[test]
    fn a_running_handoff_asks_for_every_frame_and_a_resting_page_asks_for_none() {
        let _g = plx_base::testlock::serial();
        asked_to_repaint(); // drain whatever the previous test left on the shared flag
        let mut h = Handoff::new();
        for f in 0..13 {
            h.step(DT, 1.0, PITCH, PITCH, true);
            assert!(asked_to_repaint(), "frame {f} of the Down handoff must repaint");
        }
        for f in 0..13 {
            h.step(DT, PITCH, 0.0, PITCH, true);
            assert!(asked_to_repaint(), "frame {f} of the Up handoff must repaint");
        }
        assert_eq!((h.chrome(), h.capsule()), (1.0, 0.0));
        for _ in 0..8 {
            h.step(DT, 0.0, 0.0, PITCH, true);
        }
        assert!(!asked_to_repaint(), "a resting page must not hold the panel awake");
        let mut tucked = Handoff::new();
        run(&mut tucked, 40, 1.0, PITCH, true);
        asked_to_repaint();
        run(&mut tucked, 8, PITCH, PITCH, true);
        assert!(!asked_to_repaint(), "nor a resting tucked page");
    }

    /// The same value is not the same state: the canonical encoding (hashed into the replay
    /// fixtures) sees the decided state, not just the two alphas.
    #[test]
    fn the_decided_state_is_part_of_the_canonical_encoding() {
        let mut shown = Handoff::new();
        let mut a = Canon::new();
        shown.write(&mut a);
        shown.tucked = true;
        let mut b = Canon::new();
        shown.write(&mut b);
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn a_short_pitch_still_leaves_scroll_zero_untucked() {
        let _g = plx_base::testlock::serial();
        for pitch in [40.0, 0.0, 119.0] {
            let mut h = Handoff::new();
            for _ in 0..30 {
                h.step(DT, 0.0, 0.0, pitch, true);
            }
            assert_eq!((h.chrome(), h.capsule()), (1.0, 0.0), "pitch {pitch}");
        }
    }

    #[test]
    fn the_capsule_runs_are_the_whole_name_then_the_count_then_the_criterion_it_carries() {
        let (name, count, criterion) = capsule_parts("Films", 1284, None);
        assert_eq!(name.to_str().unwrap(), "Films");
        assert!(count.to_str().unwrap().contains("284"), "{count:?}");
        assert!(criterion.is_none(), "a page that carries no sort has no third run");
        let long = "Documentaries and Concerts of the Century, Remastered In Full";
        let (name, count, _) = capsule_parts(long, 1284, None);
        assert_eq!(name.to_str().unwrap(), long, "never shortened, no ellipsis");
        assert!(count.to_str().unwrap().contains('1'));
        let (_, _, criterion) = capsule_parts("Films", 1284, Some("Date viewed"));
        assert_eq!(criterion.unwrap().to_str().unwrap(), "Date viewed", "the criterion as the header shows it");
    }

    /// **The capsule's strings are built when their text changes, not every frame**: asked again with
    /// the same name, total and criterion, `CapsuleText` hands back the very same C strings (same
    /// addresses); a different name, total or criterion (or none) builds them again, and what it
    /// builds is what [`capsule_parts`] says.
    #[test]
    fn the_capsule_strings_are_kept_while_the_name_the_total_and_the_criterion_stand() {
        let _g = plx_base::testlock::serial();
        let mut text = CapsuleText::default();
        let at = |parts: (&CStr, &CStr, Option<&CStr>)| (parts.0.as_ptr(), parts.1.as_ptr(), parts.2.map(CStr::as_ptr));
        let first = at(text.get("Films", 1284, Some("Date viewed")));
        for _ in 0..3 { assert_eq!(at(text.get("Films", 1284, Some("Date viewed"))), first, "nothing changed: nothing rebuilt"); }
        let (name, count, criterion) = text.get("Films", 1284, Some("Date viewed"));
        let want = capsule_parts("Films", 1284, Some("Date viewed"));
        assert_eq!((name, count, criterion), (want.0.as_c_str(), want.1.as_c_str(), want.2.as_deref()));
        for (label, name, total, criterion) in [("name", "Shows", 1284, Some("Date viewed")), ("total", "Films", 1285, Some("Date viewed")),
            ("criterion", "Films", 1284, Some("Year")), ("no criterion", "Films", 1284, None)] {
            let mut text = CapsuleText::default();
            let before = text.get("Films", 1284, Some("Date viewed")).0.to_owned();
            let parts = text.get(name, total, criterion);
            let want = capsule_parts(name, total, criterion);
            assert_eq!((parts.0, parts.1, parts.2), (want.0.as_c_str(), want.1.as_c_str(), want.2.as_deref()), "{label}: rebuilt for the new text");
            assert_eq!(before.to_str().unwrap(), "Films");
        }
    }

    /// **A name made of pieces is built only when a piece changes**: the pieces are compared as
    /// they stand, `build` does not run while they, the total and the criterion are the same, and it
    /// runs once for a changed title, a changed owner, a different split of the same text, a new
    /// total or a new criterion.
    #[test]
    fn a_name_made_of_pieces_is_built_when_a_piece_changes() {
        let _g = plx_base::testlock::serial();
        let mut text = CapsuleText::default();
        let built = std::cell::Cell::new(0);
        let ask = |text: &mut CapsuleText, key: &[&str], total: i64, criterion: Option<&str>| {
            let name = key.join(" ");
            let parts = text.get_keyed(key, || { built.set(built.get() + 1); name }, total, criterion);
            (parts.0.to_owned(), parts.1.to_owned(), parts.2.map(CStr::to_owned))
        };
        let first = ask(&mut text, &["Films", "friend"], 1284, Some("Year"));
        assert_eq!(first.0.to_str().unwrap(), "Films friend");
        for _ in 0..3 { assert_eq!(ask(&mut text, &["Films", "friend"], 1284, Some("Year")), first); }
        assert_eq!((built.get(), text.builds()), (1, 1), "the same pieces: nothing built again");
        for (label, key, total, criterion) in [
            ("title", &["Movies", "friend"][..], 1284, Some("Year")), ("owner", &["Movies", "pal"][..], 1284, Some("Year")),
            ("split", &["Movies pal"][..], 1284, Some("Year")), ("no owner", &["Movies", ""][..], 1284, Some("Year")),
            ("total", &["Movies", ""][..], 1285, Some("Year")), ("criterion", &["Movies", ""][..], 1285, Some("Plays")),
        ] {
            let before = built.get();
            let parts = ask(&mut text, key, total, criterion);
            assert_eq!(built.get(), before + 1, "{label}: built once for the change");
            assert_eq!(text.builds(), built.get(), "{label}: the probe counts the same builds");
            let want = capsule_parts(&key.join(" "), total, criterion);
            assert_eq!((parts.0.as_c_str(), parts.1.as_c_str(), parts.2.as_deref()), (want.0.as_c_str(), want.1.as_c_str(), want.2.as_deref()), "{label}");
            assert_eq!(ask(&mut text, key, total, criterion), parts, "{label}: and then held");
            assert_eq!(built.get(), before + 1);
        }
    }

    /// One predicate for every page: a header that names no library (the Watchlist, a one-library
    /// page) and a header that names exactly one has nothing to restate; two or more do.
    #[test]
    fn the_capsule_shows_only_where_the_header_names_two_or_more_libraries() {
        assert!(!capsule_shown(0), "a header that names none has nothing to restate");
        assert!(!capsule_shown(1), "one name is not a choice between libraries");
        assert!(capsule_shown(2));
        assert!(capsule_shown(8));
    }

    /// **A header row riding the page keeps out of the band the chrome is drawn in, and a chrome
    /// that is leaving takes the band with it.** The geometry (`clearance`): 0 where the row's top is
    /// at or above the drawn band's bottom edge, 1 [`CLEARANCE_RAMP`] below it, monotone between; the
    /// band is [`TOP_BAR_BOTTOM`](crate::widgets::TOP_BAR_BOTTOM) tall shown and nothing once gone; and a row at
    /// its rest (`page_header::TOP`) is whole under a shown chrome, so nothing at rest is dimmed.
    #[test]
    fn a_riding_row_is_clear_below_the_drawn_band_and_gone_inside_it() {
        let bottom = crate::widgets::TOP_BAR_BOTTOM;
        let shown = Handoff::new();
        assert_eq!(shown.clearance(bottom), 0.0, "touching the band is inside it");
        assert_eq!(shown.clearance(bottom - 40.0), 0.0);
        assert_eq!(shown.clearance(bottom + CLEARANCE_RAMP), 1.0);
        assert_eq!(shown.clearance(crate::page_header::TOP), 1.0, "the row at rest is whole under a shown chrome");
        let ramp: Vec<f32> = (0..=18).map(|i| shown.clearance(bottom + CLEARANCE_RAMP * i as f32 / 18.0)).collect();
        assert!(ramp.windows(2).all(|w| w[1] >= w[0]) && ramp.iter().any(|a| *a > 0.0 && *a < 1.0), "a ramp, not a cut: {ramp:?}");
        let mut gone = Handoff::new();
        gone.step(DT, 0.0, 2.0 * PITCH, PITCH, false);
        for _ in 0..30 { gone.step(DT, 2.0 * PITCH, 2.0 * PITCH, PITCH, false); }
        assert_eq!(gone.chrome(), 0.0);
        assert_eq!(gone.clearance(CLEARANCE_RAMP), 1.0, "a chrome that has gone keeps a row out of nothing");
        assert_eq!(gone.clearance(0.0), 0.0, "only the screen's own top edge");
    }

    /// **A page that keeps its chrome (Home) asks the same rule a shown Handoff answers.** A heading
    /// whose top is at or above the chrome's bottom edge is gone, one [`CLEARANCE_RAMP`] below it is
    /// whole, and the answer is exactly `Handoff::new().clearance` at every height, so there is one
    /// rule and Home holds no copy of it.
    #[test]
    fn a_page_that_keeps_its_chrome_asks_the_shown_handoffs_rule() {
        let bottom = crate::widgets::TOP_BAR_BOTTOM;
        assert_eq!(clearance_under_shown_chrome(bottom), 0.0);
        assert_eq!(clearance_under_shown_chrome(0.0), 0.0);
        assert_eq!(clearance_under_shown_chrome(bottom + CLEARANCE_RAMP), 1.0);
        assert_eq!(clearance_under_shown_chrome(crate::page_header::TOP), 1.0, "a row at its rest is whole");
        for i in 0..=40 {
            let top = bottom - 20.0 + i as f32 * 2.0;
            assert_eq!(clearance_under_shown_chrome(top), Handoff::new().clearance(top), "at {top}");
        }
    }

    /// **Up from a scrolled page, through the real scroll spring, never draws the row where the chrome
    /// is; Down never dims a row before the chrome that is leaving has lifted off it.** The Up leg is
    /// the F3 shape (the chrome whole by frame 8 while the row is still gliding home); the Down leg is
    /// what keeps the Library's header, which is not scaled by the chrome's own alpha, from popping when
    /// the chrome has finished leaving, and the Watchlist's from fading faster than its chrome.
    #[test]
    fn the_row_waits_for_its_place_on_the_way_up_and_is_never_dimmed_early_on_the_way_down() {
        let mut spring = crate::Spring::at(0.0);
        let mut h = Handoff::new();
        let mut down = Vec::new();
        for _ in 0..90 {
            spring.step(PITCH, crate::consts::K_SCROLL, DT);
            h.step(DT, spring.pos, PITCH, PITCH, false);
            down.push((h.chrome(), h.clearance(crate::page_header::TOP - spring.pos), crate::page_header::TOP - spring.pos));
        }
        assert!(down.iter().any(|f| f.0 > 0.0 && f.0 < 1.0), "premise: the chrome leaves over several frames");
        for (i, (chrome, clear, top)) in down.iter().enumerate().filter(|(_, f)| f.0 > 0.0) {
            assert_eq!(*clear, 1.0, "down frame {i}: the row is dimmed while the chrome (alpha {chrome:.2}) is still leaving, at y {top:.0}");
        }
        let mut up = Vec::new();
        for i in 0..90 {
            spring.step(0.0, crate::consts::K_SCROLL, DT);
            h.step(DT, spring.pos, 0.0, PITCH, false);
            let top = crate::page_header::TOP - spring.pos;
            up.push((i, h.chrome(), h.clearance(top), top));
        }
        let bottom = crate::widgets::TOP_BAR_BOTTOM;
        for (i, chrome, clear, top) in &up {
            assert!(*clear == 0.0 || *top >= bottom * chrome, "up frame {i}: the row is drawn ({clear:.2}) at y {top:.0} inside the chrome band (alpha {chrome:.2})");
        }
        let (shown_first, whole_first) = (up.iter().position(|f| f.2 > 0.0).unwrap(), up.iter().position(|f| f.2 == 1.0).unwrap());
        let chrome_whole = up.iter().position(|f| f.1 == 1.0).unwrap();
        assert!(shown_first > chrome_whole, "the row arrives after the chrome is whole ({shown_first} vs {chrome_whole}): the chrome did not wait");
        assert!(whole_first >= shown_first + 3, "and arrives over more than a pop ({shown_first}..{whole_first})");
        assert_eq!(up.last().unwrap().2, 1.0, "home, it is whole");
    }
}
