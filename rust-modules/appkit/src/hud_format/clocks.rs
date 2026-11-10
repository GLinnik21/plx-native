//! **The playbar's two clocks, as app text the compiler and the lint can see** (`docs/hud-plugins.md`,
//! "Invariants" I4 and I10, "Text fit: the app is the enforcement point").
//!
//! The playbar draws its clocks itself (`classic::draw_playbar_look`): the elapsed one in bold, the remaining one
//! in regular with `fmt_time`'s leading minus, both on a line [`CLOCK_TOP`] under the track's top. Neither is a
//! `text` op, so the layout engine gave them no room and no check saw them: a band under the bar landed on the
//! clock line, and the reference's gear disc was drawn over `-52:33`.
//!
//! This module is their geometry, derived and never guessed:
//! - **the string** is the widest the app can print, `fmt_time` over `H:MM:SS` with two hour digits and each
//!   position's widest digit (measured, so a face whose digits differ in width is covered, with no
//!   tabular-figure assumption), in the weight the clock is drawn in;
//! - **the rect** stands where `draw_clock` draws: the elapsed clock at the bar's start under `clocks: ends`
//!   (the clamp in `draw_clock` seats its left edge there) and anywhere along the track under `follow_head`
//!   (centred on the playhead, clamped to the track), which makes its rect the whole track; the remaining clock
//!   always ends at the track's end. A rect is as tall as the `Measure`'s line at the clock's rung.
//!
//! The compiler stores the rects on the playbar op ([`ClockRects`]); `lint_zorder` and `lint_safe` read them as
//! decor (I4, I5) and `lint_text` judges the strings against their room (I10). The room under the bar that keeps
//! a band from landing on them is `classic::clock_line_h`, taken by the compiler from the same [`CLOCK_TOP`] the
//! draw uses.
//!
//! Pure: no IO, no globals.

use std::ops::RangeInclusive;

use plx_machine::machine::Measure;
use plx_ui::Rect;

use super::compile::ClockMode;
use crate::player_hud::classic::{fmt_time, CLOCK_TOP};

/// Where the playbar's two clocks stand on the clock line (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockRects {
    /// the elapsed clock: its widest string at the bar's start (`ends`), or the whole track (`follow_head`)
    pub elapsed: Rect,
    /// the remaining clock: its widest string, ending at the track's end
    pub remaining: Rect,
    /// Optional right-aligned lane for the entire remaining-clock and end-time run.
    pub end_time: Option<EndTimeLane>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EndTimeLane {
    pub rect: Rect,
    /// The widest duration digit at the compiled clock size and weight.
    pub(crate) widest_digit: char,
}

/// Shared spacing, ink and weight for the combined clock in the renderer and text lint.
pub(crate) fn end_time_run(size: i32) -> plx_ui::widgets::DottedRun<'static> {
    use plx_ui::theme;
    plx_ui::widgets::DottedRun::new(size, theme::TEXT_SECONDARY, theme::space::XS)
}

/// The weight each clock is drawn in (`draw_playbar_look`: `draw_clock` is bold, the remaining label is not).
pub(crate) const ELAPSED_BOLD: bool = true;
/// See [`ELAPSED_BOLD`].
pub(crate) const REMAINING_BOLD: bool = false;

/// The widest clock string the app can print at `size` in `bold`, as the playbar prints it: `H:MM:SS` with two
/// hour digits, each position's widest digit, and `fmt_time`'s minus when `remaining`.
pub(crate) fn widest_clock(measure: &dyn Measure, size: i32, bold: bool, remaining: bool) -> String {
    let tens_of_hours = widest_digit(measure, size, bold, 1..=9);
    let tens_of_minutes = widest_digit(measure, size, bold, 0..=5);
    let ones = widest_digit(measure, size, bold, 0..=9);
    let hours = i64::from(tens_of_hours * 10 + ones);
    let minutes = i64::from(tens_of_minutes * 10 + ones);
    let seconds = minutes;
    fmt_time(((hours * 60 + minutes) * 60 + seconds) * 1_000_000_000, remaining)
}

/// The digit of `digits` that is widest at `size` and `bold`; the lowest of equals.
pub(crate) fn widest_digit(measure: &dyn Measure, size: i32, bold: bool, digits: RangeInclusive<u32>) -> u32 {
    let mut widest = (*digits.start(), f32::NEG_INFINITY);
    for digit in digits {
        let width = measure.width_str(&digit.to_string(), size, bold);
        if width > widest.1 {
            widest = (digit, width);
        }
    }
    widest.0
}

/// The clocks of a playbar whose track is `track`, with its clocks at rung `size` placed per `mode`, measured
/// with `measure`.
pub(crate) fn rects(
    track: Rect, size: i32, mode: ClockMode, end_time_width: Option<f32>, measure: &dyn Measure,
) -> ClockRects {
    let y = track.y + CLOCK_TOP;
    let h = measure.line_h(size);
    let width = |bold, remaining| measure.width_str(&widest_clock(measure, size, bold, remaining), size, bold);
    let (elapsed_w, remaining_w) = (width(ELAPSED_BOLD, false), width(REMAINING_BOLD, true));
    let elapsed = match mode {
        ClockMode::Ends => Rect::new(track.x, y, elapsed_w.min(track.w), h),
        ClockMode::FollowHead => Rect::new(track.x, y, track.w, h),
    };
    let remaining_w = remaining_w.min(track.w);
    let at_end = |w| Rect::new(track.x + track.w - w, y, w, h);
    let end_time = end_time_width.map(|w| EndTimeLane {
        rect: at_end(w.min(track.w)),
        widest_digit: char::from(b'0' + widest_digit(measure, size, REMAINING_BOLD, 0..=9) as u8),
    });
    ClockRects { elapsed, remaining: at_end(remaining_w), end_time }
}
