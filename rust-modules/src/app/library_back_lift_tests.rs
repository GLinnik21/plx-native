//! **BACK from a Detail onto the Library grid leaves its rows where they stood.** Owner, on the TV:
//! "when entering a movie details and going back the row below the selected card bounces". The
//! frames are the product's: the route dip's own image decision (`PageImage::plan`, `DipImage`)
//! after every World frame, and the grid's settled geometry read back (`World::poster_cell`, the
//! caption bands folded in).
//!
//! The rule under test is `Grid::dormant`'s: a page that RETURNS (`Enter(Restored)`) is not put to
//! sleep under the dip's image, a page ENTERED under one is. `Enter(Restored)` also reaches a live
//! page, a menu's dismissal being one, so nothing here leaves a mark on a page that no image
//! followed.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_screens::registry::{LibraryCmd, LibraryMenuKind};
use plx_ui::containers::transition::{PageDip, PagePaint};

/// One frame of the run: the dip's phase, what the image decision made of the page, whether the
/// page reported layout motion, and the focused poster's drawn pop (`None` off the Library).
#[derive(Clone, Copy, Debug)]
struct Frame {
    phase: &'static str,
    paint: PagePaint,
    moving: bool,
    scale: Option<f32>,
}

/// The Library on the route dip, 120 posters in the store, settled on its first poster.
struct Rig {
    w: World,
    dip: DipImage,
    frames: Vec<Frame>,
    _held: ImageHeld,
}

impl Rig {
    fn new(name: &str) -> Rig {
        let mut w = World::movies_library(name, false);
        w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
        w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
        let mut rig = Rig { w, dip: DipImage::default(), frames: Vec::new(), _held: ImageHeld };
        rig.run(90);
        assert_eq!(rig.w.grid_cell(), Some((0, 0)), "premise: the library opens on its first poster");
        rig
    }

    fn step(&mut self, inputs: Vec<InputEvent<u32>>) {
        let paint = self.w.step_dip(&mut self.dip, inputs).expect("a page is on top");
        let scale = self.w.on(&AppArg::Library).then(|| self.scale());
        self.frames.push(Frame {
            phase: self.w.d.nav.tabs.stack.transition.phase_word(),
            paint,
            moving: plx_machine::idle::page_layout_moving(),
            scale,
        });
    }

    fn run(&mut self, n: u32) { for _ in 0..n { self.step(vec![]); } }

    fn key(&mut self, key: Key) {
        let t = tick(self.w.frame + 1);
        self.step(script_key(key, t));
    }

    /// The focused poster's drawn scale now (1.0 is rest).
    fn scale(&self) -> f32 { self.w.library().probe_arrival(self.w.d.focus()).1 }

    /// OK on the focused poster and its Detail settled: the frames before the BACK.
    fn open_detail(&mut self) {
        self.key(Key::Ok);
        self.run(90);
        assert!(!self.w.on(&AppArg::Library), "premise: OK on the poster opened its Detail");
    }

    /// BACK from the Detail and `frames` frames of the dip after it; the index of the BACK's own
    /// frame is returned.
    fn back(&mut self, frames: u32) -> usize {
        let from = self.frames.len();
        self.key(Key::Back);
        self.run(frames);
        from
    }

    /// The first frame at or after `from` on which `is` holds.
    fn first(&self, from: usize, is: impl Fn(&Frame) -> bool) -> usize {
        from + self.frames[from..].iter().position(is).expect("the run reached that frame")
    }

    /// The floor of the dip that begins at or after `from`.
    fn floor(&self, from: usize) -> usize { self.first(from, |f| f.phase == "hold") }

    /// The first frame of that dip on which the dissolve is over.
    fn in_end(&self, from: usize) -> usize {
        let floor = self.floor(from);
        self.first(floor, |f| f.phase == "live")
    }

    /// The first frame of that dip on which the image is replaced by the live page.
    fn release(&self, from: usize) -> usize {
        let floor = self.floor(from);
        self.first(floor, |f| matches!(f.paint, PagePaint::ReplacementCapture | PagePaint::Live))
    }
}

/// The first cell of the row under the focused one.
fn below(w: &World) -> plx_ui::Rect {
    let first = w.poster_cell(0);
    (1..64).map(|i| w.poster_cell(i)).find(|cell| cell.y > first.y + 1.0).expect("premise: a second row")
}

/// **The row under the focused poster stays where it stood on the way back.** The caption band the
/// poster opens pushes that row down; the page that returned put the card to rest and shut the
/// band under the dip's image, then grew both again, so the row moved by the band and eased back.
#[test]
fn back_from_a_detail_leaves_the_library_grid_rows_where_they_stood() {
    let mut rig = Rig::new("library-back-lift");
    let settled = below(&rig.w).y;
    rig.open_detail();
    rig.key(Key::Back);
    let mut ys = Vec::new();
    for _ in 0..90 {
        rig.run(1);
        if rig.w.on(&AppArg::Library) { ys.push(below(&rig.w).y); }
    }
    assert!(!ys.is_empty(), "premise: BACK returned to the Library");
    assert!(ys.iter().all(|y| (y - settled).abs() < 0.5),
        "the row under the focused poster moved on the way back (settled at {settled}): {ys:?}");
}

/// **The focused poster is lifted on every frame of the way back**, the image's included: the
/// card is never put to rest and grown again.
#[test]
fn back_from_a_detail_keeps_the_focused_poster_lifted_every_frame() {
    let mut rig = Rig::new("library-back-selected");
    let lifted = rig.scale();
    assert!(lifted > 1.0, "premise: the focused poster is lifted at rest on the grid ({lifted})");
    rig.open_detail();
    let from = rig.back(90);
    let scales: Vec<f32> = rig.frames[from..].iter().filter_map(|f| f.scale).collect();
    assert!(!scales.is_empty(), "premise: BACK returned to the Library");
    assert!(scales.iter().all(|s| (s - lifted).abs() < 0.001),
        "the focused poster left its lift on the way back (lifted at {lifted}): {scales:?}");
}

/// **The dip's image is let go when its dissolve ends**, as it is for the Recommended page: a card
/// that comes back whole starts no layout motion for the image to wait for (two frames of slack for
/// the quiescence tick).
#[test]
fn back_from_a_detail_lets_the_dip_image_go_when_its_dissolve_ends() {
    let mut rig = Rig::new("library-back-release");
    rig.open_detail();
    let from = rig.back(90);
    let (in_end, release) = (rig.in_end(from), rig.release(from));
    assert!(release - in_end <= 2,
        "the BACK dip's dissolve ended on frame {in_end} and the image stood until frame {release} ({} frames)",
        release - in_end);
}

/// **A key pressed under the image of the way back waits for nothing.** The page is awake for its
/// return only until the reader moves: the move is slept through like an entry's, so the image is
/// let go when the dissolve ends and the new focus grows from rest after it, where it can be seen
/// (a move played under the image stretched its hold to the bound).
#[test]
fn a_key_pressed_under_the_back_dips_image_is_slept_through_and_grows_after_it() {
    let mut rig = Rig::new("library-back-key");
    rig.open_detail();
    let from = rig.back(0);
    rig.run(8);
    let floor = rig.floor(from);
    assert!(matches!(rig.frames.last().unwrap().paint, PagePaint::Held(_)) && rig.frames.len() > floor + 2,
        "premise: the dip's image is up (floor on frame {floor})");
    let pressed = rig.frames.len();
    rig.key(Key::Right);
    rig.run(120);
    assert_eq!(rig.w.grid_cell(), Some((0, 1)), "premise: RIGHT moved focus to the next poster");
    let (in_end, release) = (rig.in_end(from), rig.release(from));
    let moving: Vec<usize> = (pressed..release).filter(|&i| rig.frames[i].moving).collect();
    assert!(moving.is_empty(), "the move played under the image, held on frames {moving:?} (released on {release})");
    assert!(release - in_end <= 2,
        "the BACK dip's dissolve ended on frame {in_end} and the image stood until frame {release}");
    let held: Vec<Option<f32>> = rig.frames[pressed + 1..release].iter().map(|f| f.scale).collect();
    assert!(held.iter().all(|scale| scale.is_some_and(|s| (s - 1.0).abs() < 1e-5)),
        "the new focus is not at rest under the held image: {held:.4?}");
    let after: Vec<f32> = rig.frames[release..].iter().filter_map(|f| f.scale).collect();
    assert!(after[0] < 1.05, "the new focus is drawn at {:.3} the frame the image is released", after[0]);
    assert!(after.iter().copied().fold(0.0f32, f32::max) > 1.08, "the new focus grows to its lift: {after:.3?}");
}

/// **Leaving the grid under the image of the way back waits for nothing either.** Up onto the
/// header, or the BACK that walks there: the caption band the grid kept open shuts at once and
/// nothing is left closing on a spring, so the image is let go when the dissolve ends.
#[test]
fn focus_leaving_the_grid_under_the_back_dips_image_is_slept_through() {
    for key in [Key::Up, Key::Back] {
        let mut rig = Rig::new("library-back-leave");
        rig.open_detail();
        let from = rig.back(0);
        rig.run(8);
        assert!(matches!(rig.frames.last().unwrap().paint, PagePaint::Held(_)), "premise: the dip's image is up");
        let pressed = rig.frames.len();
        rig.key(key);
        rig.run(120);
        assert_eq!(rig.w.grid_cell(), None, "premise: {key:?} took focus off the grid");
        let (in_end, release) = (rig.in_end(from), rig.release(from));
        let moving: Vec<usize> = (pressed..release).filter(|&i| rig.frames[i].moving).collect();
        assert!(moving.is_empty(), "{key:?}: the move played under the image, held on frames {moving:?} (released on {release})");
        assert!(release - in_end <= 2,
            "{key:?}: the BACK dip's dissolve ended on frame {in_end} and the image stood until frame {release}");
    }
}

/// The page's fade alpha (1.0 once it is in) and the focused poster's scale on each frame of the
/// dissolve a library command starts.
fn dissolve(rig: &mut Rig) -> Vec<(f32, f32)> {
    Bridge::library_command(&mut rig.w.d, LibraryCmd::Enter(plx_data::stores::browse::SecKind::Movie));
    (0..30).map(|_| {
        rig.run(1);
        rig.w.library().probe_arrival(rig.w.d.focus())
    }).collect()
}

/// The dissolve of `trace` seats nothing: the poster stands at rest until the page is in, and then
/// grows.
fn assert_dissolves_unpopped(trace: &[(f32, f32)], what: &str) {
    let fading: Vec<_> = trace.iter().filter(|(alpha, _)| *alpha < 1.0).collect();
    assert!(fading.len() >= 5, "premise ({what}): the page dissolves in ({} frames of fade)", fading.len());
    assert!(fading.iter().all(|(_, scale)| (scale - 1.0).abs() < 1e-5),
        "{what}: the seat was lifted under the dissolve: {trace:.3?}");
    assert!(trace.last().unwrap().1 > 1.08, "{what}: the seat never grew once the page was in: {trace:.3?}");
}

/// **A return leaves no mark on the page**: the next arrival is slept through as ever. A modal
/// dismissed over the Library delivers `Enter(Restored)` to the live page with no image after it,
/// and BACK delivers it with one; the library switched afterwards dissolves in with its seat at
/// rest and lifts on the dissolve's last frame, as it does on a page that never returned.
#[test]
fn a_page_that_returned_dissolves_the_next_library_in_unpopped() {
    // the control: a page that never returned
    {
        let mut rig = Rig::new("library-back-fresh");
        let trace = dissolve(&mut rig);
        assert_dissolves_unpopped(&trace, "a page that never returned");
    }
    // a Sort & Filter menu opened and dismissed: no image follows its Restored
    {
        let mut rig = Rig::new("library-back-menu");
        Bridge::library_command(&mut rig.w.d, LibraryCmd::OpenMenu(LibraryMenuKind::SortFilter));
        rig.run(40);
        rig.key(Key::Back);
        rig.run(40);
        assert_eq!(rig.w.grid_cell(), Some((0, 0)), "premise: the menu is gone and focus is back on the poster");
        assert!(rig.scale() > 1.08, "premise: the poster came back lifted");
        let trace = dissolve(&mut rig);
        assert_dissolves_unpopped(&trace, "a page a menu was dismissed over");
    }
    // BACK from a Detail: the image follows, and is let go
    {
        let mut rig = Rig::new("library-back-then-switch");
        rig.open_detail();
        rig.back(90);
        let trace = dissolve(&mut rig);
        assert_dissolves_unpopped(&trace, "a page BACK returned to");
    }
}
