//! **How the Recommended page ARRIVES**, frame by frame, the way the product plans it: the route
//! dip's own image decision (`PageImage::plan`, the same call `Dispatcher::draw_with` makes) run on
//! the World rig after every frame, and what the page DRAWS read back (the focused card's placement
//! `At::Drawn`, the text a draw puts on screen with its cascaded alpha), never a helper's answer.
//!
//! Owner, on the television, pressing Recommended on the Library header: "No animation on
//! recommended entering". The Library's grid arrives in its first poster (the page is held dormant
//! under the dip's image, then the poster grows from rest on the standard lift spring); the
//! Recommended page seated its first card with the pop already whole while the image was up, which
//! opened the first row's caption band and lifted its heading as layout motion, so the dip's image
//! was kept for half a second after the dissolve and then cut to the focused page.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_ui::containers::transition::{PageDip, PagePaint};
use plx_ui::screen::At;

/// The pop is a multiple of the card's resting size: the Recommended page's shelves are `HOME`
/// shelves (`hub_style`), so the style's own focus scale is the card whole in its lift.
const FULL_LIFT: f32 = plx_ui::cards::RowStyle::HOME.focus_scale;

/// One frame of an entry: the dip's phase, what the image decision made of the page, what the
/// focused card was drawn at and what the first shelf's heading was drawn at.
#[derive(Clone, Copy, Debug)]
struct Frame {
    phase: &'static str,
    paint: PagePaint,
    /// The Recommended page's focused card, drawn width over its settled (focus-scaled) width times
    /// the full lift: 1.0 is a card at rest, 1.09 a card whole in its pop.
    scale: Option<f32>,
    /// The alpha the first shelf's heading ("One") was drawn at, the cascade folded in.
    heading: Option<f32>,
    /// The y the SECOND shelf's heading ("Two") was drawn at: the shelf under the focused one, which
    /// moves by the focused shelf's caption band whenever that opens or shuts.
    below: Option<f32>,
}

/// The product's frame loop plus its page-image decision, recorded.
struct Entry {
    dip: DipImage,
    frames: Vec<Frame>,
}

impl Entry {
    fn new() -> Self { Entry { dip: DipImage::default(), frames: Vec::new() } }

    /// One World frame, then the dispatcher's image decision on it (`World::step_dip`), then the
    /// frame read. `draw` also runs the page's draw through a recording painter.
    fn step(&mut self, w: &mut World, inputs: Vec<InputEvent<u32>>, draw: bool) {
        let Some(paint) = w.step_dip(&mut self.dip, inputs) else { return };
        let phase = w.d.nav.tabs.stack.transition.phase_word();
        let (heading, below) = if draw { heading_runs(w) } else { (None, None) };
        // The test's draw is not the product's image pass: it must not undo the plan above.
        plx_ui::containers::transition::set_page_image_held(!matches!(paint, PagePaint::Live));
        self.frames.push(Frame { phase, paint, scale: drawn_scale(w), heading, below });
    }

    fn frames(&mut self, w: &mut World, n: u32, draw: bool) { for _ in 0..n { self.step(w, vec![], draw); } }

    fn key(&mut self, w: &mut World, key: Key) {
        let t = tick(w.frame + 1);
        self.step(w, script_key(key, t), false);
    }

    /// The first frame after the floor (the dip's `hold`) on which the dip is over: the image
    /// has finished fading IN.
    fn in_end(&self) -> usize { self.in_end_after(0) }

    /// The first frame after the floor on which the page is drawn live again (the image is
    /// replaced by one last capture, or let go).
    fn release(&self) -> usize { self.release_after(0) }

    /// [`Self::in_end`] of the first dip that reaches its floor at or after frame `from` (the
    /// frames before it belong to an earlier dip of the same run).
    fn in_end_after(&self, from: usize) -> usize {
        let floor = self.floor_after(from);
        floor + self.frames[floor..].iter().position(|f| f.phase == "live").expect("the dip's In ended")
    }

    /// [`Self::release`] of the first dip that reaches its floor at or after frame `from`.
    fn release_after(&self, from: usize) -> usize {
        let floor = self.floor_after(from);
        floor + self.frames[floor..].iter()
            .position(|f| matches!(f.paint, PagePaint::ReplacementCapture | PagePaint::Live))
            .expect("the image was released")
    }

    fn floor_after(&self, from: usize) -> usize {
        from + self.frames[from..].iter().position(|f| f.phase == "hold").expect("the dip reached its floor")
    }
}

/// The focused card of the Recommended page on top, as the page places it NOW (`At::Drawn`).
fn drawn_scale(w: &World) -> Option<f32> {
    let key = w.d.focus()?;
    let top = w.d.nav.top_page()?;
    if top.id != key.entry { return None; }
    let screen = &top.inst.as_ref()?.screen;
    screen.as_any()?.downcast_ref::<plx_screens::library::RecommendedScreen>()?;
    let parts = CxParts {
        tick: tick(w.frame),
        press: Default::default(),
        focus: plx_machine::machine::FocusRead { current: Some(key), ..Default::default() },
        owner: InputOwner::Entry(key.entry),
    };
    let cx = parts.cx::<AppHost>(w.bridge.views(), &w.bridge.measure);
    let card = screen.focused_card(&cx, Some(key), Some(At::Drawn))?;
    let (drawn, settled) = (card.rect?, card.rest_rect?);
    Some(drawn.w / settled.w * FULL_LIFT)
}

/// What the page drew the first and second shelves' headings at, from one draw (`None` for a
/// heading it drew none of): the first one's alpha, the cascade (the dip's page alpha, the page's own
/// fades) folded into the ink, and the second one's y. The page draw runs through a recording painter.
fn heading_runs(w: &mut World) -> (Option<f32>, Option<f32>) {
    let drawn = w.drawn();
    let first = |text: &str| drawn.runs.iter().filter(|run| run.text == text).min_by(|a, b| a.at.y.total_cmp(&b.at.y));
    (first("One").map(|run| run.ink[3]), first("Two").map(|run| run.at.y))
}

/// The Library with its hubs published, OK on the header's Recommended control, and the frames
/// after it (`draw`: each also run through the page's draw).
fn warm_entry(name: &str, draw: bool) -> (World, Entry) {
    let mut w = World::movies_library(name, false);
    w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
    w.walk_header_to(9);
    let mut entry = Entry::new();
    entry.frames(&mut w, 2, false);
    entry.key(&mut w, Key::Ok);
    entry.frames(&mut w, 120, draw);
    (w, entry)
}

/// **The page image is let go when the dip's dissolve ends.** The owner saw the dip dim and
/// brighten, then the page stand still and its focus appear half a second late. The page seated
/// its first card with the pop already whole under the image, which opened the caption band and
/// lifted the first heading (layout motion the image waits for); a page held at rest under the image
/// seats nothing to wait for, so the image goes the frame the dissolve is over (two frames of
/// slack for the quiescence tick).
#[test]
fn a_warm_recommended_entry_lets_the_dip_image_go_when_its_dissolve_ends() {
    let _held = ImageHeld;
    let (w, entry) = warm_entry("rec-enter-release", false);
    let (in_end, release) = (entry.in_end(), entry.release());
    assert!(w.on(&AppArg::LibraryRecommended { section: 0 }), "premise: the Recommended page is on top");
    assert!(
        release - in_end <= 2,
        "the dip's dissolve ended on frame {in_end} and the image stood until frame {release} ({} frames), the focus it hid arriving late",
        release - in_end
    );
}

/// **The focused card grows into its lift where it can be seen.** The Library's first poster
/// arrives at rest and grows on the standard spring the frame the image is gone; the Recommended
/// page's was already whole (1.09) under the image, so nothing was left to watch. The scale is read
/// off the page's own placement of the card, every frame.
#[test]
fn a_warm_recommended_entry_grows_its_focused_card_from_rest_after_the_image() {
    let _held = ImageHeld;
    let (_w, entry) = warm_entry("rec-enter-grow", false);
    let release = entry.release();
    let scales: Vec<f32> = entry.frames[release..].iter().map_while(|f| f.scale).collect();
    assert_grows_from_rest(&scales);
}

/// The card's drawn scale on the frames from the image's release on: at rest on the release frame,
/// the lift starting within a couple of frames (the shelf wakes on the first tick after it), growing
/// on every live frame after, and whole in the end.
fn assert_grows_from_rest(scales: &[f32]) {
    assert!(scales.len() > 20, "premise: the page keeps a focused card after the release ({} frames read)", scales.len());
    assert!(scales[0] < 1.05, "the card is drawn at {:.3} the frame the image is released: whole before anyone could see it", scales[0]);
    let start = scales.iter().position(|scale| *scale > 1.0 + 1e-4).expect("the card starts to grow");
    assert!(start <= 3, "the card stands at rest for {start} frames after the image is released: {:?}", &scales[..8]);
    let rising = scales[start - 1..].windows(2).take(8).filter(|pair| pair[1] > pair[0]).count();
    assert_eq!(rising, 8, "the lift must keep growing over its first live frames, not stand: {:?}", &scales[..start + 8]);
    let top = scales.iter().copied().fold(0.0f32, f32::max);
    assert!(top > FULL_LIFT - 0.002, "the card reaches its full lift ({top:.4})");
}

/// A warm Recommended page that has been left for a card's Detail and returned from it: the frames
/// before the BACK are the entry's (header OK, 120 frames, 60 more settled), the Detail's dip and
/// its settle; the returned entry's `from` is the first frame of the BACK, and the last value is
/// where the second shelf's heading stood while the page was settled with its first card lifted
/// (`draw`: the frames from the BACK on are also run through the page's draw, so their `below` is
/// read).
fn warm_return(name: &str, draw: bool) -> (World, Entry, usize, f32) {
    let (mut w, mut entry) = warm_entry(name, false);
    entry.frames(&mut w, 60, false);
    assert!(w.on(&AppArg::LibraryRecommended { section: 0 }), "premise: the Recommended page is on top");
    let (_, stood) = heading_runs(&mut w);
    let stood = stood.expect("premise: the page draws its second shelf's heading");
    entry.key(&mut w, Key::Ok);
    entry.frames(&mut w, 60, false);
    assert!(!w.on(&AppArg::LibraryRecommended { section: 0 }), "premise: OK on the first card opened its Detail");
    let from = entry.frames.len();
    entry.key(&mut w, Key::Back);
    entry.frames(&mut w, 120, draw);
    assert!(w.on(&AppArg::LibraryRecommended { section: 0 }), "premise: BACK returned to the Recommended page");
    (w, entry, from, stood)
}

/// **BACK onto a Recommended page that kept its body releases the dip's image when the dissolve
/// ends.** The page's shelves go dormant under the dip's image and (on a shelf alone) ease to rest;
/// the push froze that ease mid-way (the page is covered, it does not tick), BACK resumed it under
/// the BACK dip's image, and the layout animation it set moving (the heading, the caption band) held
/// the image ~25 frames past the dissolve until the replacement capture snapped from mid-ease to
/// rest. Nothing steps under an image that hides it (`transition::page_image_held`): a dormant
/// shelf under one is at rest on its first tick, and the shelf that returns with the seat is already
/// where it goes.
#[test]
fn a_recommended_page_returning_from_detail_lets_the_dip_image_go_when_its_dissolve_ends() {
    let _held = ImageHeld;
    let (_w, entry, from, _) = warm_return("rec-return-release", false);
    let (in_end, release) = (entry.in_end_after(from), entry.release_after(from));
    assert!(
        release - in_end <= 2,
        "the BACK dip's dissolve ended on frame {in_end} and the image stood until frame {release} ({} frames)",
        release - in_end
    );
}

/// **The returned page's seat comes back lifted, and the shelf under it does not bounce.** BACK from
/// a card's Detail restores the focus to the card it left, and the card stood lifted with its
/// caption band open when the Detail covered the page. The page's shelves are dormant under the BACK
/// dip's image, which used to put the restored seat to rest under it and grow it back once the image
/// was gone: the card regrew, and the caption band with it, moving the next shelf down the page by
/// the band's height. A shelf that RETURNS keeps its seat as it stood (the Library's grid does the
/// same). Read off the page's own placement of the card and the y it drew the second shelf's heading
/// at, every frame from the dip's floor on, the image's release included.
#[test]
fn a_recommended_page_returning_from_detail_keeps_its_focused_card_lifted_and_the_shelf_below_still() {
    let _held = ImageHeld;
    let (_w, entry, from, stood) = warm_return("rec-return-lifted", true);
    let (floor, release) = (entry.floor_after(from), entry.release_after(from));
    let frames = &entry.frames[floor..];
    assert!(release > floor + 6, "premise: the image stands past the floor for a few frames ({} read)", release - floor);
    let scales: Vec<Option<f32>> = frames.iter().map(|f| f.scale).collect();
    assert!(scales.iter().all(|scale| scale.is_some_and(|scale| scale > FULL_LIFT - 0.002)),
        "the focused card is not whole in its lift on every frame of the return (floor ..): {scales:.4?}");
    let below: Vec<f32> = frames.iter().filter_map(|f| f.below).collect();
    assert!(below.len() > 40, "premise: the page's second heading was drawn after the floor ({} frames)", below.len());
    assert!(below.iter().all(|y| (y - stood).abs() < 0.5),
        "the shelf under the focused one moved from y {stood:.1} on the return: {below:.1?}");
}

/// **Cold: the shelves dissolve in instead of cutting in.** The section's hubs are still on their
/// way when the page opens (a spinner stands in), and when they land after the dip the whole run of
/// shelves used to appear in one frame at full strength with the first card already lifted. They
/// now arrive through the shared content cross-fade (`Xfade::mount`), the first card growing once
/// they are in.
#[test]
fn a_cold_recommended_page_dissolves_its_shelves_in_and_then_lifts_the_first_card() {
    let _held = ImageHeld;
    let mut w = World::movies_library_unshelved("rec-enter-cold");
    w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
    w.walk_header_to(9);
    let mut entry = Entry::new();
    entry.key(&mut w, Key::Ok);
    entry.frames(&mut w, 60, false);
    assert!(w.on(&AppArg::LibraryRecommended { section: 0 }), "premise: the Recommended page is on top");
    let release = entry.release();
    assert!(entry.frames[release..].iter().all(|f| f.heading.is_none()),
        "premise: the page draws no shelf while its hubs are on their way");
    let landed = entry.frames.len();
    w.land_shelves();
    entry.frames(&mut w, 40, true);
    let after = &entry.frames[landed..];
    let first = after.iter().position(|f| f.heading.is_some()).expect("the shelves were drawn once they landed");
    let alphas: Vec<f32> = after[first..].iter().map_while(|f| f.heading).collect();
    assert!(alphas[0] < 0.99, "the first shelf's heading is drawn at {:.2} on the first frame it exists: a cut, not a dissolve", alphas[0]);
    assert!(alphas.windows(2).all(|pair| pair[1] >= pair[0]), "the dissolve only rises: {alphas:.2?}");
    assert!(alphas.iter().any(|alpha| *alpha > 0.99), "the dissolve completes: {alphas:.2?}");
    let scale = after[first].scale.expect("the first card is seated by the time the shelves are drawn");
    assert!(scale < 1.05, "the first card is drawn at {scale:.3} on the frame its shelf first appears: lifted before it can be seen");
    let grown = after[first..].iter().filter_map(|f| f.scale).fold(0.0f32, f32::max);
    assert!(grown > FULL_LIFT - 0.002, "the first card grows into its lift once the shelves are in ({grown:.4})");
}

/// **A warm entry is the dip's alone.** The shelves' own dissolve is for a page opened before its
/// hubs exist: with them published, the dip is the arrival, and a second fade under it would still
/// be ramping after the dip is over. From the dip's first live frame on, the first heading is
/// drawn at its full ink.
#[test]
fn a_warm_recommended_entry_is_not_dissolved_twice() {
    let _held = ImageHeld;
    let (_w, entry) = warm_entry("rec-enter-once", true);
    let floor = entry.frames.iter().position(|f| f.phase == "hold").expect("the dip reached its floor");
    let live: Vec<f32> = entry.frames[floor..].iter().filter(|f| f.phase == "live").filter_map(|f| f.heading).collect();
    assert!(live.len() > 40, "premise: the page's shelves were drawn after the dip ({} frames)", live.len());
    assert!(live[0] > 0.999, "the first heading is drawn at {:.3} on the dip's first live frame: a second fade stands on a warm entry", live[0]);
}
