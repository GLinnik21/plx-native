use super::*;
use plx_machine::machine::PosterKey;
use plx_ui::tex::{Source, Warm};
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
    fn unresident(&self, _: PosterKey, _: bool) {}
    fn idle(&self) -> bool { true }
}

/// A scrolled Library keeps buffered rows for data/focus geometry, but those rows must not
/// continuously enqueue artwork. In a full source cache, two such warms alternated eviction
/// of the same cold slot and kept an otherwise settled screen decoding/uploading forever.
#[test]
fn scrolled_grid_admits_only_visible_art_and_never_rewarms_hidden_rows() {
    let _guard = plx_base::testlock::serial();
    // Source installation and observations are thread-local to this test. A descriptive draw
    // traverses the actual Part implementation while its primitive declarations avoid GL.
    plx_ui::tex::install(&ArtSpy);
    let painter = plx_ui::Painter::root();
    for episodes in [false, true] {
        let fixture = Fixture::art_listing(episodes);
        let mut page = fixture.screen();
        let layout = page.layout;
        let scroll = layout.row_reveal(20);
        page.scroll.jump(scroll);
        page.scroll_target = scroll;
        page.sync(&fixture.cx(None));
        let window = page.pair.detail.window();
        let (lo, hi) = (window.start, window.end);
        let visible: Vec<_> = (lo..hi).filter(|&i| plx_ui::cards::paint_visible(
            painter, page.pair.detail.rect_at(&fixture.cx(None), i), 1.0, false)).collect();
        assert!(lo > 0 && !visible.is_empty() && visible.len() < hi - lo,
            "the fixture needs both actually visible cards and culled buffered cards");
        // `painted` is the primitive's cull minus the rows wholly over the content edge (the row
        // above the focused one rests under the top fade now): whole rows off the top only.
        let painted = page.pair.detail.painted();
        assert_eq!(painted.end, visible[visible.len() - 1] + 1,
            "Grid::painted is the run of cards the draw's own cull keeps (what the lookahead steps beyond)");
        assert!(painted.start >= visible[0] && (painted.start - visible[0]) % layout.cols() == 0
            && painted.start - visible[0] < 2 * layout.cols(), "{painted:?} vs {visible:?}");
        // The focused card can also be outside the viewport (e.g. while a retained page is
        // translated). Exercise its separate paint path, as well as the ordinary grid loop.
        // Slot 0 is far above the wanted rows, so it holds no key until a seat names it.
        assert!(page.pair.detail.elem_at(0).is_none(), "the far slot is outside the projected windows");
        let far = page.pair.detail.elem_for(&fixture.cx(None), &mut page.keys, 0, layout.cols()).unwrap();
        let hidden_focus = page.key(far);
        for focus in [None, Some(hidden_focus)] {
            ART_REQUESTS.with(|calls| *calls.borrow_mut() = Default::default());
            let cx = fixture.cx(focus);
            {
                let _discovery = plx_ui::frame::backdrop::discover(
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
                let expected: Vec<_> = visible.iter().filter(|i| painted.contains(i)).map(|i| format!("{prefix}{i}")).collect();
                assert_eq!(calls.0.len(), expected.len() * 12, "visible tiles continue resolving artwork");
                for frame in calls.0.chunks_exact(expected.len()) {
                    assert_eq!(frame, expected, "a DRAW must not admit hidden artwork (the at-rest lookahead in `prepare` is the one sanctioned path)");
                }
            });
            ART_REQUESTS.with(|calls| *calls.borrow_mut() = Default::default());
            // The normal paint pass admits warm requests (discovery/source replay does not).
            // Translate the retained page outside the canvas so the production cull keeps all
            // primitives away from GL, but both hidden-card paths still execute normally.
            assert!(!plx_gfx::gfx::blur_source_pass());
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

/// CONFIRM A1 / N6: the SEATED card's poster is the one the reader is looking at, so its request
/// goes first. The worker takes the newest frame's lowest-LRU-age request first
/// (`poster::next_wanted`: "the card drawn first, i.e. reading order") and the grid probes the
/// focused card LAST (z-order: it paints over its neighbours), so a Library whose first poster
/// was seated got its art after every sibling's: a dark slot among eleven fading-in tiles, then a
/// cut. The focused card resolves first; everything else keeps reading order.
#[test]
fn the_focused_card_asks_for_its_art_before_its_neighbours() {
    let _guard = plx_base::testlock::serial();
    plx_ui::tex::install(&ArtSpy);
    let painter = plx_ui::Painter::root();
    for episodes in [false, true] {
        let prefix = if episodes { "/still/" } else { "/poster/" };
        for seated in [0usize, 3] {
            let fixture = Fixture::art_listing(episodes);
            let mut page = fixture.screen();
            page.sync(&fixture.cx(None));
            let layout = page.layout;
            let elem = page.pair.detail.elem_for(&fixture.cx(None), &mut page.keys, seated, layout.cols())
                .expect("the seated card is in the held window");
            let focus = page.key(elem);
            let cx = fixture.cx(Some(focus));
            ART_REQUESTS.with(|calls| *calls.borrow_mut() = Default::default());
            {
                let _discovery = plx_ui::frame::backdrop::discover(
                    std::rc::Rc::new(RefCell::new(Default::default())));
                let mut frame = DrawFrame::new(&cx, painter);
                page.pair.detail.draw(&mut frame, Rect::FULL);
            }
            let asked = ART_REQUESTS.with(|calls| calls.borrow().0.clone());
            let want = format!("{prefix}{seated}");
            assert!(asked.len() > 1 && asked.contains(&want), "the seated card is drawn: {asked:?}");
            assert_eq!(asked[0], want, "episodes={episodes}: the focused card asks first; asked: {asked:?}");
            let rest: Vec<_> = asked.iter().skip(1).cloned().collect();
            let mut reading = rest.clone();
            reading.sort_by_key(|p| p.trim_start_matches(prefix).parse::<usize>().unwrap());
            assert_eq!(rest, reading, "the others keep reading order: {asked:?}");
        }
    }
}
