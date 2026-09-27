use super::*;
use crate::ui::machine::PosterKey;
use crate::ui::tex::{Source, Warm};
use std::cell::RefCell;

thread_local! {
    static ART_REQUESTS: RefCell<(Vec<String>, Vec<String>)> = const { RefCell::new((Vec::new(), Vec::new())) };
}

struct ArtSpy;
impl Source for ArtSpy {
    fn probe(&self, _: u16, path: &str, _: i32, _: i32, _: bool) -> Option<PosterKey> {
        ART_REQUESTS.with(|calls| calls.borrow_mut().0.push(path.into()));
        None
    }
    fn warm(&self, _: u16, path: &str, _: i32, _: i32, _: bool) -> Warm {
        ART_REQUESTS.with(|calls| calls.borrow_mut().1.push(path.into()));
        Warm::Claimed
    }
    fn logo(&self, _: u16, _: &str) -> Option<PosterKey> { None }
    fn logo_warm(&self, _: u16, _: &str) -> Warm { Warm::Known }
    fn unresident(&self, _: PosterKey) {}
    fn idle(&self) -> bool { true }
}

/// A scrolled Library keeps buffered rows for data/focus geometry, but those rows must not
/// continuously enqueue artwork. In a full source cache, two such warms alternated eviction
/// of the same cold slot and kept an otherwise settled screen decoding/uploading forever.
#[test]
fn scrolled_grid_admits_only_visible_art_and_never_rewarms_hidden_rows() {
    let _guard = crate::testlock::serial();
    // Source installation and observations are thread-local to this test. A descriptive draw
    // traverses the actual Part implementation while its primitive declarations avoid GL.
    crate::ui::tex::install(&ArtSpy);
    let painter = crate::ui::Painter::root();
    for episodes in [false, true] {
        let sid = crate::plex::ServerId::from_raw(0);
        let mut fixture = Fixture::new();
        fixture.listing = crate::browse::view::ListingSnapshot::fixture(sid,
            (0..1200).map(|i| Some(crate::pms::PmsMovie {
                sid, rk: i.to_string(), kind: if episodes { 3 } else { 0 },
                thumb: format!("/poster/{i}"), still: format!("/still/{i}"),
                ..Default::default()
            })).collect(), Vec::new()).with_library_type(if episodes {
                crate::browse::LibraryType::Episodes
            } else { crate::browse::LibraryType::Shows });
        let mut page = fixture.screen();
        let layout = page.layout;
        let scroll = layout.row_reveal(20);
        page.pair.detail.set_geometry(layout, scroll, layout, scroll);
        let (lo, hi) = page.pair.detail.visible_window();
        let visible: Vec<_> = (lo..hi).filter(|&i| crate::ui::card_row::paint_visible(
            painter, page.pair.detail.rect_at(i, false, 1.0), 1.0, false)).collect();
        assert!(lo > 0 && !visible.is_empty() && visible.len() < hi - lo,
            "the fixture needs both actually visible cards and culled buffered cards");
        // The focused card can also be outside the viewport (e.g. while a retained page is
        // translated). Exercise its separate paint path, as well as the ordinary grid loop.
        let hidden_focus = page.key(page.pair.detail.elem_at(0).unwrap());
        for focus in [None, Some(hidden_focus)] {
            ART_REQUESTS.with(|calls| *calls.borrow_mut() = Default::default());
            let cx = fixture.cx(focus);
            {
                let _discovery = crate::ui::frame::backdrop::discover(
                    std::rc::Rc::new(RefCell::new(Default::default())));
                for _ in 0..12 {
                    let mut frame = DrawFrame::new(&cx, painter);
                    page.pair.detail.draw(&mut frame, Rect::FULL);
                }
            }
            ART_REQUESTS.with(|calls| {
                let calls = calls.borrow();
                assert!(calls.1.is_empty(), "hidden tiles must enqueue no warm requests; got {}", calls.1.len());
                let prefix = if episodes { "/still/" } else { "/poster/" };
                let expected: Vec<_> = visible.iter().map(|i| format!("{prefix}{i}")).collect();
                assert_eq!(calls.0.len(), expected.len() * 12, "visible tiles continue resolving artwork");
                for frame in calls.0.chunks_exact(expected.len()) {
                    assert_eq!(frame, expected, "hidden artwork must not enter the source by any path");
                }
            });
            ART_REQUESTS.with(|calls| *calls.borrow_mut() = Default::default());
            // The normal paint pass admits warm requests (discovery/source replay does not).
            // Translate the retained page outside the canvas so the production cull keeps all
            // primitives away from GL, but both hidden-card paths still execute normally.
            assert!(!crate::gfx::blur_source_pass());
            for _ in 0..12 {
                let mut frame = DrawFrame::new(&cx, painter.translate(2.0 * SCR_W, 0.0));
                page.pair.detail.draw(&mut frame, Rect::FULL);
            }
            ART_REQUESTS.with(|calls| {
                let calls = calls.borrow();
                assert!(calls.1.is_empty(), "hidden tiles must enqueue no warm requests; got {}", calls.1.len());
                assert!(calls.0.is_empty(), "culled tiles must not resolve artwork either");
            });
        }
    }
}
