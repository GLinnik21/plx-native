//! **The scrub's way back**: the knob's spring to the playhead when a scrub is cancelled, and the
//! undo window that follows a committed one.
//!
//! Both are PURE — a value of time in, a number or a rect out — so the choreography is tested
//! without a GL context, like [`crate::scrub_preview::Motion`] (whose [`Ramp`] and [`Ease`] they
//! reuse rather than growing a second easing). `TransportRow` owns one of each; the player screen
//! decides WHEN (`open`, `close`) and the HUD draws what they answer.
//!
//! **The undo window.** A scrub that started while the film was playing and then committed leaves
//! the position it started from behind: for [`UNDO_MS`] a BACK returns there (one more seek). The
//! bar shows where "there" is as a ghost tick at the origin, and a chip, `BACK · m:ss`, says what
//! the key will do; both fade out over [`UNDO_FADE_MS`] once the window closes. The window is a
//! deadline on the wall clock and nothing else — it is not held open by anything on screen.
//!
//! **The spring.** A cancelled scrub ends with the published preview position gone and the playhead
//! where it always was, so the knob would jump. [`KnobSpring`] remembers where the preview last was
//! and eases from there to wherever the bar then shows. A COMMITTED scrub ends the same way and
//! needs no case of its own: the bar then shows the seek target, which is where the preview was, so
//! the ease has nothing to travel.

use crate::scrub_preview::{Ease, Ramp};
use plx_ui::Rect;

/// How long BACK undoes a committed scrub.
pub const UNDO_MS: u32 = 5_000;
/// The ghost tick and the chip fade out over this once the window has closed.
pub const UNDO_FADE_MS: u32 = 300;
/// The knob's return to the playhead after a cancelled scrub.
pub const SPRING_MS: u32 = 280;
/// The ghost tick: 2 px wide, 14 px tall, white at 40 %.
pub const TICK_W: f32 = 2.0;
pub const TICK_H: f32 = 14.0;
pub const TICK_A: f32 = 0.40;
/// The chip's ink: white at 60 %.
pub const CHIP_A: f32 = 0.60;
/// Clearance between the chip and the knob clock, the transport mark or the remaining-time label.
const CHIP_GAP: f32 = 14.0;

/// An open undo window: where BACK returns to, the instant it closes, and the chip it is drawn
/// with.
#[derive(Clone, PartialEq, Debug)]
pub struct Undo {
    origin_ns: i64,
    until: u32,
    chip: Option<Chip>,
}

/// The chip's text, formatted and made a C string ONCE when the window opens, and its width as
/// the first live-font draw measured it. A window is drawn for [`UNDO_MS`] + [`UNDO_FADE_MS`] —
/// over three hundred frames — and the text cannot change in that time, so the per-frame draw
/// only reads it back.
#[derive(Clone, PartialEq, Debug)]
pub struct Chip {
    c: std::ffi::CString,
    /// `(size, width)` as measured; kept only when the measure is the process's live font
    /// (`Measure::live_font`), so a recording or replay still sees every query it is graded on.
    width: Option<(i32, f32)>,
}

impl Chip {
    /// `None` when the text cannot be a C string (an embedded NUL, which no shipped word has).
    fn new(back_word: &str, origin_ns: i64) -> Option<Self> {
        let text = chip_text(back_word, origin_ns);
        let c = std::ffi::CString::new(text).ok()?;
        Some(Self { c, width: None })
    }

    /// The text, for `Painter::text`.
    pub fn c_str(&self) -> &std::ffi::CStr {
        &self.c
    }

    /// The chip's width at `sz`, measured once per window on the live font.
    pub fn width(&mut self, sz: i32, measure: &dyn plx_machine::machine::Measure) -> f32 {
        if let Some((s, w)) = self.width {
            if s == sz {
                return w;
            }
        }
        let w = measure.width(&self.c, sz, false);
        // `text_width` reads 0 until the font is up: never keep that
        if w > 0.0 && measure.live_font() {
            self.width = Some((sz, w));
        }
        w
    }
}

impl Undo {
    /// A window opened by a scrub that committed at `now`, having started from `origin_ns`.
    /// It draws no chip until [`with_chip`](Self::with_chip) gives it one.
    pub fn open(origin_ns: i64, now: u32) -> Self {
        Self { origin_ns, until: now.wrapping_add(UNDO_MS), chip: None }
    }

    /// The window with its chip built, from the word for Back in the current language.
    pub fn with_chip(mut self, back_word: &str) -> Self {
        self.chip = Chip::new(back_word, self.origin_ns);
        self
    }

    /// The chip, to draw and measure; `None` when the window has none.
    pub fn chip_mut(&mut self) -> Option<&mut Chip> {
        self.chip.as_mut()
    }

    /// The position BACK returns to.
    pub fn origin_ns(&self) -> i64 {
        self.origin_ns
    }

    /// Is BACK still an undo at `now`? Wrapping, like every other deadline in the HUD.
    pub fn armed(&self, now: u32) -> bool {
        (self.until.wrapping_sub(now) as i32) > 0
    }

    /// Is anything of it still on screen — the open window or its fade?
    pub fn visible(&self, now: u32) -> bool {
        self.armed(now) || (now.wrapping_sub(self.until) as i32) < UNDO_FADE_MS as i32
    }

    /// The tick's and the chip's opacity: full while armed, then [`UNDO_FADE_MS`] out.
    pub fn alpha(&self, now: u32) -> f32 {
        if self.armed(now) {
            return 1.0;
        }
        Ramp::new(1.0, 0.0, self.until, UNDO_FADE_MS, Ease::In).at(now)
    }

    /// 0 = gone, 1 = open and steady, 2 = fading: the part of the window's state a frame depends
    /// on. A frame is owed when this changes, and for as long as it is 2 (the player's
    /// `clock_fingerprint` folds it in).
    pub fn phase(&self, now: u32) -> u8 {
        if self.armed(now) {
            1
        } else if self.visible(now) {
            2
        } else {
            0
        }
    }
}

/// The ghost tick at the origin: [`TICK_W`] × [`TICK_H`], centred on the bar's centre line `cy`.
pub fn tick_rect(origin_x: f32, cy: f32) -> Rect {
    Rect::new(origin_x - TICK_W * 0.5, cy - TICK_H * 0.5, TICK_W, TICK_H)
}

/// The chip's text: the Back word, in capitals as every clickable text mark in the app is, then the
/// origin's time written the way the app's other clocks are (`plx_ui::fmt::clock`).
pub fn chip_text(back_word: &str, origin_ns: i64) -> String {
    format!("{} · {}", back_word.to_uppercase(), plx_ui::fmt::clock(origin_ns / 1_000_000))
}

/// What the playbar drew on its clock line, as x extents — everything the chip must stay clear of.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LineExtents {
    /// the knob clock's label box
    pub clock: (f32, f32),
    /// the transport mark's ink, when one is drawn
    pub mark: Option<(f32, f32)>,
    /// the remaining-time label's left edge, when it is drawn
    pub rem_l: Option<f32>,
    /// the bar's own extent: the chip never leaves it
    pub lo: f32,
    pub hi: f32,
}

/// Where the chip of width `w` starts (its left edge), or `None` when it fits nowhere.
///
/// It sits on the clock's line, on the side the origin is on — so it points at the tick — beyond the
/// mark if one is drawn; the other side is the fallback. It never reaches the remaining-time label
/// nor leaves the bar, and when neither side has room it is not drawn: a chip that cannot be read is
/// worse than none.
pub fn chip_x(ext: &LineExtents, origin_x: f32, w: f32) -> Option<f32> {
    let (mut l, mut r) = ext.clock;
    if let Some((ml, mr)) = ext.mark {
        l = l.min(ml);
        r = r.max(mr);
    }
    let left = l - CHIP_GAP - w;
    let right = r + CHIP_GAP;
    let right_limit = ext.rem_l.map_or(ext.hi, |rl| rl - CHIP_GAP);
    let fits_left = left >= ext.lo;
    let fits_right = right + w <= right_limit;
    let prefer_left = origin_x < (ext.clock.0 + ext.clock.1) * 0.5;
    let pick_left = if prefer_left { fits_left || !fits_right } else { !fits_right && fits_left };
    if pick_left {
        fits_left.then_some(left)
    } else {
        fits_right.then_some(right)
    }
}

/// The knob's return to the playhead after a scrub ends (see the module doc).
#[derive(Clone, Copy, Debug)]
pub struct KnobSpring {
    /// the preview position as of the last frame a scrub was live; `-1` = no scrub is live
    live_ns: i64,
    /// where the spring started from
    from_ns: i64,
    ramp: Ramp,
}

impl Default for KnobSpring {
    fn default() -> Self {
        Self::new()
    }
}

impl KnobSpring {
    pub const fn new() -> Self {
        Self { live_ns: -1, from_ns: 0, ramp: Ramp::rest(1.0) }
    }

    /// Once per frame, with the PUBLISHED scrub position (`-1` = none). The frame the scrub ends
    /// starts the spring from where the preview last was.
    pub fn step(&mut self, scrub_ns: i64, now: u32) {
        if scrub_ns >= 0 {
            self.live_ns = scrub_ns;
            self.ramp = Ramp::rest(1.0);
        } else if self.live_ns >= 0 {
            self.from_ns = self.live_ns;
            self.live_ns = -1;
            self.ramp = Ramp::new(0.0, 1.0, now, SPRING_MS, Ease::Out);
        }
    }

    /// The position the bar draws: `shown_ns` (what it would show with no spring) once the spring is
    /// done, and a point between where the preview was and `shown_ns` until then. It eases toward
    /// the CURRENT `shown_ns`, so a playhead that keeps moving is followed, not passed.
    pub fn apply(&self, shown_ns: i64, now: u32) -> i64 {
        let t = self.ramp.at(now);
        if t >= 1.0 {
            return shown_ns;
        }
        self.from_ns + ((shown_ns - self.from_ns) as f64 * f64::from(t)) as i64
    }

    /// Does the spring still move at `now`?
    pub fn busy(&self, now: u32) -> bool {
        self.ramp.busy(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_armed_for_five_seconds_and_no_longer() {
        let u = Undo::open(60_000_000_000, 1_000);
        assert!(u.armed(1_000) && u.armed(5_999));
        assert!(!u.armed(6_000), "the window closes AT five seconds");
        assert_eq!(u.origin_ns(), 60_000_000_000);
    }

    #[test]
    fn the_window_survives_the_tick_counter_wrapping() {
        let u = Undo::open(1, u32::MAX - 1_000);
        assert!(u.armed(u32::MAX - 500) && u.armed(2_000), "armed across the wrap");
        assert!(!u.armed(4_000), "…and closed after it");
    }

    #[test]
    fn the_tick_and_chip_fade_out_over_300_ms_after_the_window_closes() {
        let u = Undo::open(0, 0);
        assert_eq!(u.alpha(4_999), 1.0);
        assert_eq!(u.alpha(5_000), 1.0, "the fade starts at the close");
        let mid = u.alpha(5_150);
        assert!(mid > 0.0 && mid < 1.0, "half-way through the fade: {mid}");
        assert_eq!(u.alpha(5_300), 0.0);
        assert!(u.visible(5_299) && !u.visible(5_300));
        let a: Vec<f32> = (0..=300).step_by(25).map(|d| u.alpha(5_000 + d)).collect();
        assert!(a.windows(2).all(|w| w[0] >= w[1]), "never brightens: {a:?}");
    }

    #[test]
    fn the_phase_names_open_fading_and_gone() {
        let u = Undo::open(0, 0);
        assert_eq!([u.phase(10), u.phase(5_100), u.phase(5_400)], [1, 2, 0]);
    }

    #[test]
    fn the_tick_is_two_by_fourteen_centred_on_the_origin_and_the_bar() {
        let r = tick_rect(500.0, 100.0);
        assert_eq!((r.w, r.h), (2.0, 14.0));
        assert_eq!((r.x + r.w * 0.5, r.y + r.h * 0.5), (500.0, 100.0));
    }

    /// A counting measure: how many times the draw asked the font.
    struct Counting(std::cell::Cell<u32>, bool);
    impl plx_machine::machine::Measure for Counting {
        fn width(&self, s: &std::ffi::CStr, _sz: i32, _bold: bool) -> f32 {
            self.0.set(self.0.get() + 1);
            s.to_bytes().len() as f32
        }
        fn cap_h(&self, sz: i32) -> f32 {
            sz as f32
        }
        fn line_h(&self, sz: i32) -> f32 {
            sz as f32
        }
        fn live_font(&self) -> bool {
            self.1
        }
    }

    #[test]
    fn the_chip_is_built_once_and_measured_once_on_the_live_font() {
        let mut u = Undo::open(83_000_000_000, 0).with_chip("Back");
        let live = Counting(std::cell::Cell::new(0), true);
        let chip = u.chip_mut().expect("a window with a chip");
        assert_eq!(chip.c_str().to_str(), Ok("BACK · 1:23"));
        let first = chip.width(22, &live);
        assert!(first > 0.0);
        for _ in 0..300 {
            assert_eq!(chip.width(22, &live), first);
        }
        assert_eq!(live.0.get(), 1, "one measurement for the whole window");
        assert!(chip.width(24, &live) > 0.0);
        assert_eq!(live.0.get(), 2, "another size is another question");
    }

    #[test]
    fn a_recording_measure_still_sees_every_query() {
        let mut u = Undo::open(0, 0).with_chip("Back");
        let recorded = Counting(std::cell::Cell::new(0), false);
        let chip = u.chip_mut().unwrap();
        for _ in 0..5 {
            chip.width(22, &recorded);
        }
        assert_eq!(recorded.0.get(), 5, "a non-live font is asked every time");
    }

    #[test]
    fn the_chip_reads_back_then_the_origin_in_the_apps_clock_format() {
        assert_eq!(chip_text("Back", 83_000_000_000), "BACK · 1:23");
        assert_eq!(chip_text("Назад", 4_505_000_000_000), "НАЗАД · 1:15:05");
        assert_eq!(chip_text("Retour", 0), "RETOUR · 0:00");
    }

    fn ext(rem_l: Option<f32>, mark: Option<(f32, f32)>) -> LineExtents {
        LineExtents { clock: (900.0, 960.0), mark, rem_l, lo: 96.0, hi: 1824.0 }
    }

    #[test]
    fn the_chip_sits_on_the_side_of_the_origin() {
        let e = ext(Some(1700.0), None);
        // origin left of the knob clock: the chip is left of it, its right edge a gap short of it
        assert_eq!(chip_x(&e, 300.0, 200.0), Some(900.0 - 14.0 - 200.0));
        // origin right of it: the chip starts a gap after the clock
        assert_eq!(chip_x(&e, 1500.0, 200.0), Some(960.0 + 14.0));
    }

    #[test]
    fn the_chip_clears_the_transport_mark() {
        let e = ext(Some(1700.0), Some((972.0, 1010.0)));
        assert_eq!(chip_x(&e, 1500.0, 200.0), Some(1010.0 + 14.0), "beyond the mark on the right");
        let e = ext(Some(1700.0), Some((850.0, 888.0)));
        assert_eq!(chip_x(&e, 300.0, 200.0), Some(850.0 - 14.0 - 200.0), "beyond the mark on the left");
    }

    #[test]
    fn the_chip_never_reaches_the_remaining_time_label() {
        // the clock is close to the label: the right side cannot hold a 200 px chip, so it goes left
        let e = LineExtents { clock: (1400.0, 1460.0), mark: None, rem_l: Some(1650.0), lo: 96.0, hi: 1824.0 };
        let x = chip_x(&e, 1750.0, 200.0).expect("the left side has room");
        assert!(x + 200.0 <= 1400.0 - 14.0, "fell back to the left: {x}");
        // and with the label hidden, the bar's end is the limit instead
        let e = LineExtents { rem_l: None, ..e };
        assert_eq!(chip_x(&e, 1750.0, 200.0), Some(1460.0 + 14.0));
    }

    #[test]
    fn a_chip_that_fits_nowhere_is_not_drawn() {
        let e = LineExtents { clock: (120.0, 180.0), mark: None, rem_l: Some(260.0), lo: 96.0, hi: 1824.0 };
        assert_eq!(chip_x(&e, 1000.0, 400.0), None);
    }

    #[test]
    fn the_knob_springs_from_the_last_preview_to_the_playhead_in_about_280_ms() {
        let mut s = KnobSpring::new();
        s.step(60_000_000_000, 1_000);
        s.step(80_000_000_000, 1_016);
        assert_eq!(s.apply(80_000_000_000, 1_016), 80_000_000_000, "while live the bar shows the preview");
        // the scrub is cancelled: the published position is gone, the playhead is at 10 s
        s.step(-1, 1_032);
        let head = 10_000_000_000;
        assert_eq!(s.apply(head, 1_032), 80_000_000_000, "the first frame is still where the knob was");
        let mid = s.apply(head, 1_032 + 140);
        assert!(mid < 80_000_000_000 && mid > head, "half-way: {mid}");
        assert_eq!(s.apply(head, 1_032 + 280), head, "home after 280 ms");
        assert!(s.busy(1_032 + 100) && !s.busy(1_032 + 280));
        let path: Vec<i64> = (0..=280).step_by(20).map(|d| s.apply(head, 1_032 + d)).collect();
        assert!(path.windows(2).all(|w| w[0] >= w[1]), "monotone toward the playhead: {path:?}");
    }

    #[test]
    fn a_committed_scrub_has_nothing_to_spring() {
        let mut s = KnobSpring::new();
        s.step(80_000_000_000, 1_000);
        s.step(-1, 1_016);
        // the bar then shows the seek target, which is where the preview was
        for d in [0, 50, 140, 280, 400] {
            assert_eq!(s.apply(80_000_000_000, 1_016 + d), 80_000_000_000, "+{d} ms");
        }
    }

    #[test]
    fn a_new_scrub_cancels_a_spring_in_flight() {
        let mut s = KnobSpring::new();
        s.step(80_000_000_000, 1_000);
        s.step(-1, 1_016);
        s.step(30_000_000_000, 1_100);
        assert_eq!(s.apply(30_000_000_000, 1_100), 30_000_000_000);
        assert!(!s.busy(1_100));
    }

    #[test]
    fn an_idle_spring_never_moves_the_bar() {
        let s = KnobSpring::new();
        assert_eq!(s.apply(42, 0), 42);
        assert!(!s.busy(0));
    }

    /// The chip is drawn in every shipped language, at the longest clock a film can show, wherever
    /// the origin is: the Back word is the one thing that varies, and it is measured with the
    /// device's own advances rather than guessed at.
    #[test]
    fn the_chip_is_drawn_in_every_shipped_language() {
        use plx_machine::machine::Measure;
        use plx_platform::i18n::{language_on_this_thread_for_test, msg, SHIPPED};
        let measure = plx_base::fontcov::advances::ShippedMeasure;
        let line = LineExtents { clock: (900.0, 980.0), mark: None, rem_l: Some(1700.0), lo: 96.0, hi: 1824.0 };
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let text = chip_text(msg::settings_back(), 35_999 * 1_000_000_000);
            let w = measure.width_str(&text, plx_ui::theme::size::MICRO, false);
            for ox in [120.0, 500.0, 940.0, 1400.0, 1800.0] {
                assert!(
                    chip_x(&line, ox, w).is_some(),
                    "{}: {text:?} ({w:.0} px) fits nowhere with the origin at {ox}",
                    language.tag(),
                );
            }
        }
    }
}
