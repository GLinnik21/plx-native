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
//! [`art`] runs for EVERY card, not only the focused one, and remembers when each artwork path
//! last drew with a texture: a draw with none within [`RECENT`] of that is a poster that was
//! showing and went back to its placeholder ([`regressions`]). `phcount` counts placeholder draws
//! however they came about, so it cannot tell that from a poster that had not arrived yet; and a
//! path last resident long ago (the cache evicted it while it was off screen) is not counted.
//!
//! Observer only: nothing here decides a draw, and with the trigger absent every function returns
//! at its first line. The whole module is `devtriggers`, so a release build has none of it.
use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long after a texture last drew a bare draw of the same path still counts as a regression.
pub const RECENT: Duration = Duration::from_millis(1500);

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
    /// `(server, path)` hash -> when its texture last drew.
    seen: HashMap<u64, Instant>,
    regressions: u64,
    art: Option<(String, bool)>,
    caption: Option<(f32, f32)>,
    last: Option<Focused>,
}

thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }

fn hash(srv: u16, path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (srv, path).hash(&mut h);
    h.finish()
}

/// A card is about to draw: forget the previous card's art and caption.
pub fn begin_card() {
    if !armed() { return; }
    STATE.with(|s| { let mut s = s.borrow_mut(); s.art = None; s.caption = None; });
}

/// `resolve_card_art` answered for `path` (`ready`: a texture came back).
pub fn art(srv: u16, path: &str, ready: bool) {
    if !armed() { return; }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let id = hash(srv, path);
        let now = Instant::now();
        if ready {
            s.seen.insert(id, now);
            if s.seen.len() > 4096 {
                s.seen.retain(|_, t| now.duration_since(*t) < RECENT);
            }
        } else if s.seen.get(&id).is_some_and(|t| now.duration_since(*t) < RECENT) {
            s.regressions += 1;
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
    STATE.with(|s| s.borrow_mut().last = None);
}

/// Draws of a path that had been resident, with no texture.
pub fn regressions() -> u64 {
    STATE.with(|s| s.borrow().regressions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_keyed_by_its_server_and_text() {
        assert_eq!(hash(1, "x"), hash(1, "x"));
        assert_ne!(hash(1, "x"), hash(2, "x"));
        assert_ne!(hash(1, "x"), hash(1, "y"));
    }

    #[test]
    fn a_disarmed_probe_records_nothing() {
        // The trigger file is absent under `cargo test`, so every hook returns at its first line.
        art(1, "/library/metadata/5/thumb/1", true);
        focused(10.0, 20.0, 190.0, 3, 24, 3);
        assert_eq!(peek(), None);
        assert_eq!(regressions(), 0);
    }
}
