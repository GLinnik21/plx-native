//! `plxnative-focusx` for every card section: what the FOCUSED card of a [`Shelf`](crate::cards::Shelf)
//! or [`Grid`](crate::cards::Grid) looked like on the frame just drawn, read by the focus
//! fingerprint (`src/app/bridge.rs`) and graded by `tests/paging_link.py`.
//!
//! A section draws through two functions (`Shelf::draw_card`, `Grid::draw_card`), so one hook
//! there covers Home's rows, the Library's hub shelves and grid, Search's rows, Detail's related
//! row and season strip, and a Collection's members, instead of one read-out per screen. The
//! record is the card's DRAWN screen rect (so a window slide that re-bases a still focus shows as
//! the rect jumping), its index inside the window the section holds and that window's length, the
//! artwork path it asked for (the stable key: `/library/metadata/<rk>/thumb/..`) and whether the
//! texture was resident. The caption's rect rides along, so a caption drawn off the canvas is
//! visible too.
//!
//! [`art`] runs for EVERY card, not only the focused one, and keeps, per artwork request (server,
//! path and the box it was asked at: the arguments a draw resolves with), the frame it last drew
//! in and whether that draw had its texture. A draw with none then falls into one of three cases,
//! told apart in drawn frames, so no clock is read here:
//!
//! - the request's last draw had its texture and was the frame before: the poster was on screen
//!   and went back to its placeholder IN PLACE. That is the defect, and [`regressions`] counts it
//!   once per occurrence (`cdr`), not once per frame the card then stays bare;
//! - its last draw had its texture but was earlier than that: the card left the screen and came
//!   back without its picture ([`returned_bare`], `cdb`). Legitimate when memory could not hold
//!   it, so it is reported and not graded;
//! - anything else is a picture that has not arrived yet, or a later frame of a case already
//!   counted. `phcount` counts those draws, however they came about.
//!
//! The older reading counted every bare DRAW within 90 frames of a textured one under one name:
//! one blink was up to 90 counts, and a card that scrolled off and back inside 1.5 s was the same
//! number as a blink in place.
//!
//! Observer only: nothing here decides a draw, and with the trigger absent every function returns
//! at its first line. The whole module is `devtriggers`, so a release build has none of it.
use std::cell::RefCell;
use std::collections::HashMap;

/// The most requests remembered; past it, the ones not drawn for [`FORGET`] frames are dropped
/// (a card that returns after that is a first sight again).
const SEEN_MAX: usize = 4096;
const FORGET: u64 = 600;

/// One artwork request's history, in drawn frames.
#[derive(Clone, Copy)]
struct Seen {
    /// The frame it last drew in.
    drawn: u64,
    /// Whether that draw had its texture.
    textured: bool,
}

/// What one bare or textured draw of a request amounts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Draw {
    /// Nothing to count: a texture, a first sight, or one more frame of a state already counted.
    Plain,
    /// Had its texture on the frame before; has none now.
    Regressed,
    /// Had a texture once, was off screen, and is back without it.
    ReturnedBare,
}

/// Record a draw of `id` in frame `now` and class it.
fn note(seen: &mut HashMap<u64, Seen>, id: u64, now: u64, ready: bool) -> Draw {
    let prev = seen.insert(id, Seen { drawn: now, textured: ready });
    if seen.len() > SEEN_MAX {
        seen.retain(|_, s| now - s.drawn < FORGET);
    }
    match prev {
        // Its last draw had the picture and this one has not: in place if that was the frame
        // before (or this one, a second card of the same request), a return otherwise.
        Some(Seen { textured: true, drawn }) if !ready => {
            if now - drawn <= 1 { Draw::Regressed } else { Draw::ReturnedBare }
        }
        _ => Draw::Plain,
    }
}

plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-focusx`, resolved once: a per-card `stat` is not affordable.
    pub fn armed = "focusx";
);

/// The focused card of one frame, in screen pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Focused {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    /// The caption's left edge and width, when it was drawn this frame.
    pub caption: Option<(f32, f32)>,
    /// Its slot in the run of cards the section holds (a shelf's whole source, a grid's painted
    /// window) and that run's length.
    pub index: usize,
    pub len: usize,
    /// Its index in the section's source, whatever the window: what a test orders the list by.
    pub global: usize,
    /// The artwork path the card asked for; empty when it asked for none.
    pub key: String,
    /// `Some(false)`: drawn with no texture. `None`: no art request was observed.
    pub ready: Option<bool>,
}

#[derive(Default)]
struct State {
    /// Request hash -> when it last drew.
    seen: HashMap<u64, Seen>,
    /// Frames finished since start ([`clear`] ends one).
    frame: u64,
    regressions: u64,
    returned_bare: u64,
    art: Option<(String, bool)>,
    caption: Option<(f32, f32)>,
    last: Option<Focused>,
}

thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

fn hash(srv: u16, path: &str, w: i32, h: i32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut s = std::collections::hash_map::DefaultHasher::new();
    (srv, path, w, h).hash(&mut s);
    s.finish()
}

/// A card is about to draw: forget the previous card's art and caption.
pub fn begin_card() {
    if !armed() { return; }
    STATE.with(|s| { let mut s = s.borrow_mut(); s.art = None; s.caption = None; });
}

/// `resolve_card_art` answered for `path` at `w`x`h` (`ready`: a texture came back).
pub fn art(srv: u16, path: &str, w: i32, h: i32, ready: bool) {
    if !armed() { return; }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let now = s.frame;
        match note(&mut s.seen, hash(srv, path, w, h), now, ready) {
            Draw::Regressed => s.regressions += 1,
            Draw::ReturnedBare => s.returned_bare += 1,
            Draw::Plain => {}
        }
        s.art = Some((path.to_owned(), ready));
    });
}

/// The focused card's caption was placed at screen `x`, `w`.
pub fn caption(x: f32, w: f32) {
    if !armed() { return; }
    STATE.with(|s| s.borrow_mut().caption = Some((x, w)));
}

/// The focused card finished drawing at screen rect `(x, y, w)`.
pub fn focused(x: f32, y: f32, w: f32, index: usize, len: usize, global: usize) {
    if !armed() { return; }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let (key, ready) = match s.art.take() {
            Some((k, r)) => (k, Some(r)),
            None => (String::new(), None),
        };
        let caption = s.caption.take();
        s.last = Some(Focused { x, y, w, caption, index, len, global, key, ready });
    });
}

/// The last focused card drawn since [`clear`].
pub fn peek() -> Option<Focused> {
    STATE.with(|s| s.borrow().last.clone())
}

/// End of frame: a frame that draws no focused card must not report the previous frame's.
pub fn clear() {
    if !armed() { return; }
    STATE.with(|s| { let mut s = s.borrow_mut(); s.last = None; s.frame += 1; });
}

/// Posters that were on screen with their texture and went back to the placeholder in place.
pub fn regressions() -> u64 {
    STATE.with(|s| s.borrow().regressions)
}

/// Cards that came back on screen without a texture they once had.
pub fn returned_bare() -> u64 {
    STATE.with(|s| s.borrow().returned_bare)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_keyed_by_its_server_text_and_box() {
        assert_eq!(hash(1, "x", 250, 375), hash(1, "x", 250, 375));
        assert_ne!(hash(1, "x", 250, 375), hash(2, "x", 250, 375));
        assert_ne!(hash(1, "x", 250, 375), hash(1, "y", 250, 375));
        // The same picture at another size is another texture: a hero and a card do not alias.
        assert_ne!(hash(1, "x", 250, 375), hash(1, "x", 480, 270));
    }

    #[test]
    fn a_blink_in_place_is_one_regression_however_long_the_card_stays_bare() {
        let mut seen = HashMap::new();
        assert_eq!(note(&mut seen, 7, 1, false), Draw::Plain, "not arrived yet");
        assert_eq!(note(&mut seen, 7, 2, true), Draw::Plain);
        assert_eq!(note(&mut seen, 7, 3, true), Draw::Plain);
        assert_eq!(note(&mut seen, 7, 4, false), Draw::Regressed);
        for f in 5..200 {
            assert_eq!(note(&mut seen, 7, f, false), Draw::Plain, "frame {f}: the same blink, still bare");
        }
        assert_eq!(note(&mut seen, 7, 200, true), Draw::Plain);
        assert_eq!(note(&mut seen, 7, 201, false), Draw::Regressed, "a second blink is a second count");
    }

    #[test]
    fn a_card_that_left_and_came_back_bare_is_not_a_regression() {
        let mut seen = HashMap::new();
        note(&mut seen, 7, 1, true);
        // Off screen for two frames, or for a thousand: back without its picture either way.
        assert_eq!(note(&mut seen, 7, 4, false), Draw::ReturnedBare);
        assert_eq!(note(&mut seen, 7, 5, false), Draw::Plain, "counted once");
        note(&mut seen, 7, 6, true);
        assert_eq!(note(&mut seen, 7, 1006, false), Draw::ReturnedBare);
        // Back WITH its picture is nothing at all.
        note(&mut seen, 8, 1, true);
        assert_eq!(note(&mut seen, 8, 50, true), Draw::Plain);
        // Never had one: a first sight, however often it leaves and returns.
        note(&mut seen, 9, 1, false);
        assert_eq!(note(&mut seen, 9, 50, false), Draw::Plain);
    }

    #[test]
    fn two_cards_of_one_request_in_a_frame_are_one_history() {
        let mut seen = HashMap::new();
        note(&mut seen, 7, 1, true);
        note(&mut seen, 7, 1, true);
        assert_eq!(note(&mut seen, 7, 2, false), Draw::Regressed);
        assert_eq!(note(&mut seen, 7, 2, false), Draw::Plain, "the second card of the frame is the same loss");
    }

    #[test]
    fn the_memory_is_bounded() {
        let mut seen = HashMap::new();
        for id in 0..(SEEN_MAX as u64 + 10) {
            note(&mut seen, id, id * 2, true);
        }
        assert!(seen.len() <= SEEN_MAX, "{} entries", seen.len());
    }

    #[test]
    fn a_disarmed_probe_records_nothing() {
        // The trigger file is absent under `cargo test`, so every hook returns at its first line.
        art(1, "/library/metadata/5/thumb/1", 250, 375, true);
        focused(10.0, 20.0, 190.0, 3, 24, 3);
        assert_eq!(peek(), None);
        assert_eq!((regressions(), returned_bare()), (0, 0));
    }
}
