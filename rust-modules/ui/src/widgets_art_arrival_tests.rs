//! **Late art dissolves in over its skeleton** (CONFIRM A1 / N6): a poster that lands after its
//! card drew a skeleton used to cut from the dark slot to full art in one frame, however gently its
//! siblings had faded in with the page. The shared card draw now tracks the art in the card's
//! render history (`card_motion::Scope::arrival`) and dissolves it on the page fade's own duration
//! and curve (`xfade::arrived`).
//!
//! These drive the real path: a stub source, the real render cache fed through `tex::accept` and
//! `tex::prepare`, `resolve_card_art` and `card` on the host GL stubs.

use super::*;
use crate::placeholder::{self, Reason};
use plx_machine::machine::PosterKey;
use crate::tex::{Decoded, PosterReady, Source, Warm};
use std::cell::Cell;

thread_local! {
    /// Does the source hand the cache's key for the art yet?
    static LANDED: Cell<bool> = const { Cell::new(false) };
}

struct LateArt;
impl Source for LateArt {
    fn probe(&self, _: u16, _: &str, _: i32, _: i32, _: bool) -> Option<PosterKey> {
        LANDED.with(Cell::get).then_some(PosterKey(41))
    }
    fn warm(&self, _: u16, _: &str, _: i32, _: i32, _: bool) -> Warm { Warm::Known }
    fn logo(&self, _: u16, _: &str) -> Option<PosterKey> { None }
    fn logo_warm(&self, _: u16, _: &str) -> Warm { Warm::Known }
    fn unresident(&self, _: PosterKey, _: bool) {}
    fn idle(&self) -> bool { true }
}

struct Up;
impl crate::tex::Uploader for Up {
    fn upload(&mut self, d: &Decoded) -> crate::tex::Tex { crate::tex::Tex { id: 77, w: d.w, h: d.h } }
    fn warm(&mut self, _: crate::tex::Tex) {}
    fn free(&mut self, _: crate::tex::Tex) {}
}

/// Fresh cache + source, then the art UPLOADED (resident) but not yet HANDED OUT: the source keeps
/// answering "not yet" until [`land`].
fn rig() {
    static SOURCE: LateArt = LateArt;
    crate::tex::install(&SOURCE);
    crate::tex::reset_for_test(usize::MAX);
    LANDED.with(|l| l.set(false));
    crate::tex::accept(PosterReady {
        key: PosterKey(41),
        result: Ok(Decoded { w: 16, h: 24, rgba: vec![0; 16 * 24 * 4].into_boxed_slice() }),
    });
    let mut budget = crate::frame::Budget::new();
    budget.begin_frame(0);
    let mut present = plx_machine::present::Present::new();
    let mut handle = plx_machine::machine::PresentHandle::of(&mut present);
    crate::tex::prepare(&mut budget, &mut Up, &mut handle, || 0);
}

fn land() { LANDED.with(|l| l.set(true)); }

/// A poster whose card identity belongs to `owner` alone (the motion history is per thread and
/// outlives a test, so no two tests share one).
fn poster(owner: usize) -> TileFacts<'static> {
    TileFacts { owner, thumb: "/library/metadata/41/thumb/1", ..TileFacts::default() }
}

const RECT: Rect = Rect { x: 100.0, y: 100.0, w: 250.0, h: 375.0 };
/// One 60 Hz frame, in the loop's own integer ms.
fn frame_ms(n: u32) -> u32 { 100_000 + n * 50 / 3 }

/// How much of the art `resolve_card_art` shows on frame `n`.
fn shown(owner: usize, n: u32) -> f32 {
    crate::card_motion::begin_frame(frame_ms(n));
    resolve_card_art(Painter::root(), RECT, &Art::Poster(Some(poster(owner)))).shown
}

fn asked_to_repaint() -> bool {
    plx_machine::idle::frame_begin(1.0 / 60.0);
    plx_machine::idle::note_present(0);
    plx_machine::idle::should_present(0)
}

/// The defect: art that lands after the skeleton went from nothing to everything on one frame.
/// Now the first frame it is resident it shows nothing yet (the skeleton is the picture), each
/// frame after raises it a step on the page fade's curve, and it is whole after the page fade's
/// own duration, with no step anywhere near the whole range.
#[test]
fn art_that_lands_after_the_skeleton_dissolves_in_over_the_page_fades_duration() {
    let _guard = plx_base::testlock::serial();
    rig();
    let owner = 9_101;
    for n in 0..5 {
        assert_eq!(shown(owner, n), 0.0, "frame {n}: still a skeleton (no texture handed out yet)");
    }
    land();
    let mut seen = Vec::new();
    for n in 5..30 {
        seen.push(shown(owner, n));
    }
    assert_eq!(seen[0], 0.0, "the landing frame still shows the skeleton: {seen:?}");
    assert!(seen.windows(2).all(|w| w[1] >= w[0]), "the dissolve never goes back: {seen:?}");
    assert!(seen.windows(2).all(|w| w[1] - w[0] < 0.25), "no step is the whole range: {seen:?}");
    assert!(seen.iter().any(|&a| a > 0.0 && a < 1.0), "there is a dissolve to see: {seen:?}");
    let whole = seen.iter().position(|&a| a == 1.0).expect("the art ends whole");
    // `xfade::IN_MS` is 140 ms = 8.4 frames at 60 Hz; the ramp starts on the landing frame.
    let frames = whole as f32 * 50.0 / 3.0;
    assert!((frames - crate::xfade::IN_MS).abs() <= 50.0 / 3.0 + 0.1, "whole after {frames} ms: {seen:?}");
    assert!(seen[whole..].iter().all(|&a| a == 1.0), "and stays whole: {seen:?}");
    // It rides the page fade's own curve and clock, not a second one.
    let landed = frame_ms(5);
    for (i, &a) in seen.iter().enumerate().take(whole) {
        assert_eq!(a, crate::xfade::arrived((frame_ms(5 + i as u32) - landed) as f32), "frame {i} after landing");
    }
}

/// Art that was resident when the card first drew has no skeleton to dissolve over: it shows at
/// once, and so does a card whose fade was never started (a hit on its first draw).
#[test]
fn art_there_from_the_first_draw_shows_whole() {
    let _guard = plx_base::testlock::serial();
    rig();
    land();
    let owner = 9_102;
    for n in 0..4 {
        assert_eq!(shown(owner, n), 1.0, "frame {n}");
    }
}

/// A texture that goes away and comes back is late art again; a card that stopped being drawn
/// forgets (a card scrolled off and back is not a skeleton).
#[test]
fn a_card_that_was_not_drawn_the_frame_before_does_not_dissolve() {
    let _guard = plx_base::testlock::serial();
    rig();
    let owner = 9_103;
    assert_eq!(shown(owner, 0), 0.0, "a skeleton");
    // Not drawn on frame 1 (another card is): the history does not carry the wait over the gap.
    crate::card_motion::begin_frame(frame_ms(1));
    resolve_card_art(Painter::root(), RECT, &Art::Poster(Some(poster(9_999))));
    land();
    assert_eq!(shown(owner, 2), 1.0, "the card was not on screen the frame before: no skeleton to dissolve over");
}

/// The dissolve asks the frame gate for every frame of itself, and stops asking once whole (a
/// settled page must not hold the panel awake).
#[test]
fn a_dissolve_in_flight_repaints_and_a_whole_picture_does_not() {
    let _guard = plx_base::testlock::serial();
    rig();
    let owner = 9_104;
    shown(owner, 0);
    land();
    asked_to_repaint();
    for n in 1..4 {
        shown(owner, n);
        assert!(asked_to_repaint(), "frame {n} of the dissolve must repaint");
    }
    for n in 4..30 {
        shown(owner, n);
    }
    asked_to_repaint();
    shown(owner, 30);
    assert!(!asked_to_repaint(), "a whole picture asks for nothing");
}

/// The skeleton under a dissolving poster is NOT a placeholder the frame owes: a frame dump holds
/// on every counted placeholder at a frozen clock, so counting it would hold the dissolve's first
/// frame for ever. While the card waits it is counted; once its art is dissolving it is not.
#[test]
fn the_skeleton_under_a_dissolving_poster_is_not_counted() {
    let _guard = plx_base::testlock::serial();
    rig();
    let owner = 9_105;
    let drawn = |n: u32| {
        crate::card_motion::begin_frame(frame_ms(n));
        placeholder::capture_declared(|| card(Painter::root(), RECT, Art::Poster(Some(poster(owner))), 8.0, false, 1.0, 0.0)).1
    };
    assert_eq!(drawn(0).of(Reason::CardSkeleton), 1, "waiting: a placeholder");
    land();
    for n in 1..4 {
        let f = drawn(n);
        assert_eq!(f.count, 0, "frame {n}: the art is here and dissolving in, nothing is owed: {f:?}");
    }
}

/// **A glass source replay must not take the visible walk's history** (DEBUG-5; FRAMES-4 a1-coll:
/// the seated Collections tile went from skeleton to whole art and name in ONE frame).
///
/// The frame structure is `app::run::draw`'s non-player path: the DISCOVERY walk
/// (`backdrop::discover`), then one REPLAY of the page per stale glass source
/// (`backdrop::enter(sources, ceiling)`, `blur_snapshot_direct`), then the VISIBLE walk
/// (`glass.walk(Z::ALL)`, "main.ui"). Every walk draws the card. Before the fix each walk's draw was
/// its own occurrence in `card_motion`'s history, keyed by the draw's place in the whole frame. A
/// replay runs only on a frame whose glass source went stale, and the dissolve that starts when late
/// art lands is exactly what makes the header glass's source stale: measured in the simulator
/// (`frames/debug5/a1-coll-t3`), the seated tile had 2 draws a frame until its fan landed, then 3
/// for the ten frames of its dissolve. The visible walk's draw moved from occurrence 1 to occurrence
/// 2, which had no sample the frame before, so it read as art "there from the first draw" and showed
/// whole while the discovery and replay draws dissolved. A card whose art landed while a replay
/// already ran the frame before (the Library's seated card, a1-lib) kept its occurrence and
/// dissolved, which is why only some seated cards popped.
///
/// Now the history advances once per card per frame: occurrences count only within the current walk
/// and the frame's later walks reuse the first walk's sample (`card_motion`'s module doc), so a
/// replay that starts on the landing frame no longer moves the visible draw to an occurrence with no
/// record.
///
/// What the VISIBLE walk draws is what the panel shows; it must dissolve whatever replays ran.
#[test]
fn late_art_dissolves_in_the_visible_walk_when_a_glass_source_replay_starts_with_it() {
    use crate::frame::backdrop::{self, Sources, Z};
    use std::{cell::RefCell, rc::Rc};
    let _guard = plx_base::testlock::serial();
    rig();
    let sources = Rc::new(RefCell::new(Sources::default()));
    let owner = 9_106;
    let art = || Art::Poster(Some(poster(owner)));
    // One frame as the application draws it: `(discovery, replay if a source is stale, visible)`.
    let frame = |n: u32, replay: bool| -> (f32, Option<f32>, f32) {
        crate::card_motion::begin_frame(frame_ms(n));
        let discovered = {
            let _walk = backdrop::discover(sources.clone());
            resolve_card_art(Painter::root(), RECT, &art()).shown
        };
        let replayed = replay.then(|| {
            let _walk = backdrop::enter(sources.clone(), Z::CHROME);
            resolve_card_art(Painter::root(), RECT, &art()).shown
        });
        let _walk = backdrop::enter(sources.clone(), Z::ALL);
        (discovered, replayed, resolve_card_art(Painter::root(), RECT, &art()).shown)
    };
    // The page is still: no glass source is stale, nothing replays, the card is a skeleton.
    for n in 0..5 {
        assert_eq!(frame(n, false).2, 0.0, "frame {n}: a skeleton (no texture handed out yet)");
    }
    land();
    // The art lands; its dissolve keeps the glass above it stale, so a replay runs every frame of it.
    let drawn: Vec<_> = (5..30).map(|n| frame(n, n < 15)).collect();
    let visible: Vec<f32> = drawn.iter().map(|d| d.2).collect();
    assert_eq!(visible[0], 0.0, "the landing frame still shows the skeleton in the VISIBLE walk: {visible:?}");
    assert!(visible.windows(2).all(|w| w[1] >= w[0] && w[1] - w[0] < 0.25), "a dissolve, never a step: {visible:?}");
    let whole = visible.iter().position(|&a| a == 1.0).expect("the art ends whole");
    assert!(whole >= 5, "the visible art takes at least five frames to come in, got {whole}: {visible:?}");
    // A source walk replays the visible frame: the glass must filter the picture the panel shows.
    for (i, (discovered, replayed, shown)) in drawn.iter().enumerate() {
        assert_eq!(*discovered, *shown, "frame {i} after landing: the discovery walk draws what is shown");
        if let Some(replayed) = replayed {
            assert_eq!(*replayed, *shown, "frame {i} after landing: the glass source replay draws what is shown");
        }
    }
}
