//! The Library grid's AT-REST poster lookahead: while the grid rests, make the rows just beyond the
//! painted window resident, so a short scroll reveals loaded posters instead of dark tiles.
//!
//! Holding a scroll key used to show dark tiles: a row that was never drawn is never warmed
//! (`GridPart::draw`: repeated warms of hidden rows recycled each other's cold slots and kept an
//! idle page uploading, `art_admission_tests`), so every row that scrolls into view starts its
//! fetch only when it is first drawn. This module is the quiet alternative, ON by default at
//! [`DEFAULT_ROWS`] rows (measured on the TV against the 1000-movie mock: `library-burst` 28,969
//! placeholder tile-frames with no lookahead, 6,668 with it; with the moving-card decline also off
//! (`ui::card_motion`) 1,349).
//!
//! The dev trigger `plxnative-lookahead=<0-4>` overrides the depth, to measure against another
//! one: the number of rows to keep warm beyond the painted window in the last direction of travel
//! (one row the other way is always kept as well). `0` turns the lookahead off — [`rows`] answers
//! `0`, `GridPart::prepare` returns at once and nothing here is read again — which is how the
//! baseline stays measurable. A trigger that does not parse is ignored and the default stands. In
//! a build without `devtriggers` (the shipping one) the trigger cannot exist and the depth is
//! always [`DEFAULT_ROWS`].
//!
//! The rules, each of which keeps the page able to go idle:
//! - only a grid at rest asks ([`Rest`]): the spring is inside the rest band of its target AND the
//!   target has not moved for [`SETTLE_FRAMES`] frames, which also lets the visible cards claim
//!   their own art first;
//! - only a quiet source is asked (`tex::source_idle`): nothing queued, in flight or waiting for
//!   upload, so a lookahead never competes with a visible card;
//! - at most one card per frame, the nearest not yet held, and the walk ends at the first card the
//!   source accepted or refused. A fetch completing wakes the page for the next; when every
//!   candidate is held, or the source has no slot it may give, nothing wakes it again;
//! - the set is at most [`plx_ui::tex::AHEAD_SET_MAX`] cards, which is what the source's bounded
//!   lookahead store (`poster.rs`, `ahead_victim`) is sized against.
use std::ops::Range;
use std::sync::OnceLock;

use plx_ui::tex::AHEAD_SET_MAX;

/// The rows kept warm ahead of the last direction of travel unless the dev trigger says otherwise.
const DEFAULT_ROWS: usize = 3;
/// The most rows a trigger may ask for: `ROWS_MAX + 1` rows of the widest grid (six columns) is
/// exactly [`AHEAD_SET_MAX`].
const ROWS_MAX: usize = 4;
/// Frames the scroll target must hold still before the grid counts as at rest. The first lets the
/// cards on screen judge their placement (`card_motion`, one sample to know a speed), the second
/// lets them claim their art, which closes `source_idle` to a lookahead until they have it.
const SETTLE_FRAMES: u8 = 2;

/// How many rows to keep warm ahead: [`DEFAULT_ROWS`], or the dev trigger's count; `0` is off.
/// Read ONCE. This crate's own unit tests never read the host's trigger file (a leftover
/// `/tmp/plxnative-lookahead` on a dev machine would otherwise change what they assert): they see
/// the default, and [`rows_for`] is the pure rule they grade.
pub(super) fn rows() -> usize {
    static SEEN: OnceLock<usize> = OnceLock::new();
    *SEEN.get_or_init(|| {
        #[cfg(test)]
        let trigger: Option<String> = None;
        #[cfg(not(test))]
        let trigger = plx_base::devtrig::read("lookahead");
        let (rows, how) = match trigger {
            None => (DEFAULT_ROWS, "default"),
            Some(text) => match parse_rows(&text) {
                Some(rows) => (rows, "plxnative-lookahead"),
                None => {
                    plx_base::eventlog::log(&format!(
                        "library: plxnative-lookahead {:?} ignored (want a row count 0-{ROWS_MAX}); lookahead stays at {DEFAULT_ROWS}",
                        text.trim()));
                    (DEFAULT_ROWS, "default")
                }
            },
        };
        if rows == 0 {
            plx_base::eventlog::log(&format!("library: poster lookahead off ({how})"));
        } else {
            plx_base::eventlog::log(&format!(
                "library: poster lookahead on, {rows} row(s) ahead of the last direction of travel, 1 behind ({how})"));
        }
        rows
    })
}

/// The depth a trigger's content (`None`: no trigger) asks for: its count, else [`DEFAULT_ROWS`].
/// [`rows`] is this rule applied to the latched `/tmp` read.
#[cfg(test)]
pub(super) fn rows_for(trigger: Option<&str>) -> usize {
    trigger.and_then(parse_rows).unwrap_or(DEFAULT_ROWS)
}

/// A trigger's content as a row count: a whole number, capped at [`ROWS_MAX`]; anything else is
/// `None` (the caller keeps the default).
pub(super) fn parse_rows(text: &str) -> Option<usize> {
    text.trim().parse::<usize>().ok().map(|rows| rows.min(ROWS_MAX))
}

/// Whether the grid is at rest, and which way it last travelled. Render history of the page, not
/// logical state: it is never canonised, restored or compared.
pub(super) struct Rest {
    target: Option<f32>,
    still: u8,
    forward: bool,
}

impl Default for Rest {
    fn default() -> Self { Self { target: None, still: 0, forward: true } }
}

impl Rest {
    /// One call per prepared frame. `scroll` is the spring's position, `target` where it is going;
    /// returns whether the grid is at rest. A moved target restarts the count and names the
    /// direction (down is forward); the first sight of a target is not a move.
    pub(super) fn observe(&mut self, scroll: f32, target: f32) -> bool {
        match self.target {
            Some(last) if last != target => {
                self.forward = target > last;
                self.still = 0;
            }
            Some(_) => self.still = self.still.saturating_add(1),
            None => {}
        }
        self.target = Some(target);
        self.still >= SETTLE_FRAMES && plx_machine::idle::settled(scroll, target, 0.0)
    }

    pub(super) fn forward(&self) -> bool { self.forward }
}

/// The cards to keep warm, nearest row first: `ahead` rows beyond the painted window in the
/// direction of travel and one the other way, as indexes into a grid of `len` cards in `cols`
/// columns and `rows` rows. `painted` is the run of cards the grid draws (`cards::Grid::painted`),
/// so a lookahead card is exactly one the grid does NOT draw, and never one a Draw would already
/// be asking for. Empty when nothing is painted. Never more than [`AHEAD_SET_MAX`] cards.
pub(super) fn candidates(painted: Range<usize>, cols: usize, rows: usize, len: usize, ahead: usize, forward: bool)
    -> impl Iterator<Item = usize> {
    // Nearest row first and, at equal distance, the direction of travel first.
    let mut order = [usize::MAX; ROWS_MAX + 1];
    let mut n = 0;
    if !painted.is_empty() {
        let (first, last) = (painted.start / cols, (painted.end - 1) / cols);
        // The row `d` beyond the painted window on the side of travel, and the one just behind it.
        let along = |d: usize| if forward { last.checked_add(d) } else { first.checked_sub(d) };
        let behind = if forward { first.checked_sub(1) } else { last.checked_add(1) };
        for d in 1..=ahead.min(ROWS_MAX) {
            for row in [along(d), if d == 1 { behind } else { None }] {
                if let Some(row) = row.filter(|&row| row < rows) { order[n] = row; n += 1; }
            }
        }
    }
    (0..n).flat_map(move |at| (0..cols).map(move |col| order[at] * cols + col))
        .filter(move |&index| index < len)
        .take(AHEAD_SET_MAX)
}
