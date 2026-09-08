//! Meaningful legacy caption and landscape assertions, on the owned production helpers.
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::{self, CARD_H, CARD_W, MARGIN_X};
use crate::ui::fixture::FixtureMeasure;
use crate::ui::machine::{EntryId, InstanceId};
use crate::ui::{Painter, Rect};
use std::os::raw::c_int;
use crate::browse::SecKind;
use super::draw::shelf_label;
use super::layout::{self, Layout, shelf_pitch, COLS, CONTENT_TOP, GRID_RIGHT, MAX_LETTERS,
    RAIL_CAP_PAD, RAIL_TRACK_W, GRID_PITCH as PITCH};

#[test]
fn focused_grid_labels_keep_the_shared_trailing_fact_including_under_a_menu() {
    for (kind, year) in [(0, 1994), (1, 2022)] {
        let item = PmsMovie { kind, year, title: "Synthetic title".into(),
            dur_ns: 60 * 60 * 1_000_000_000, resume_ms: 35 * 60 * 1000,
            ..Default::default() };
        let label = super::parts::grid_label(&item);
        assert_eq!(label.title.unwrap().to_str().unwrap(), "Synthetic title");
        assert_eq!(label.caption.unwrap().to_str().unwrap(), year.to_string(),
            "grid cards use the shared non-deck caption, not remaining playback time");
    }
    let item = PmsMovie { title: "Undated".into(), ..Default::default() };
    assert!(super::parts::grid_label(&item).caption.is_none());
    let episode = PmsMovie { kind: 3, year: 2020, season_index: 2, ep_index: 7,
        ..Default::default() };
    assert_eq!(super::parts::grid_label(&episode).caption.unwrap().to_str().unwrap(), "S2 • E7");
}

#[test]
fn a_focused_grid_caption_stays_inside_the_rail_reserved_band() {
    let p = Painter::root();
    let card = |col: usize| {
        Rect::new(
            MARGIN_X + col as f32 * (CARD_W + layout::GRID_GAP),
            CONTENT_TOP + Layout::new(false, &[], 40, true).grid_top(),
            CARD_W,
            CARD_H,
        )
    };

    let (x, w) = card_row::label_band(p, card(COLS - 1), &super::parts::GRID_STYLE);
    assert!(
        x + w <= GRID_RIGHT + 0.01,
        "the last column's label reaches {} against the content edge {GRID_RIGHT}",
        x + w,
    );

    let (x0, w0) = card_row::label_band(p, card(0), &super::parts::GRID_STYLE);
    let (home_x0, _) = card_row::label_band(p, card(0), &RowStyle::HOME);
    assert_eq!(
        x0, home_x0,
        "the right-edge rail reserve must not move the first-column label: {x0} vs {home_x0}",
    );
    assert!(x0 >= 0.0 && x0 + w0 <= GRID_RIGHT + 0.01);
}

#[test]
fn owned_library_overscan_probe_covers_every_legacy_edge() {
    let measure = FixtureMeasure;
    let rail = layout::rail_geom(MAX_LETTERS);
    let rail_rect = Rect::new(
        rail.1 - RAIL_TRACK_W * 0.5,
        rail.0 - RAIL_CAP_PAD,
        RAIL_TRACK_W,
        rail.2 + 2.0 * RAIL_CAP_PAD,
    );
    let bare = Layout::new(false, &[], 40, true);
    let head = Layout::new(true, &[crate::ui::consts::ROW_PITCH], 40, true);
    let screen = crate::screens::library::LibraryScreen::new(
        EntryId(1),
        InstanceId(1),
        SecKind::Movie,
    );
    let chip_width = crate::ui::value_chip::ValueChip::width(
        &measure,
        c"Library",
        c" · Cinema",
        None,
    );
    let control_width = crate::ui::value_chip::ValueChip::width(
        &measure,
        c"Sort",
        c" · Title",
        None,
    );
    let rects = [
        (
            "library A–Z rail track",
            rail_rect,
        ),
        (
            "library grid, first column",
            Rect::new(Layout::grid_x(0), bare.row_y(0, 0.0), CARD_W, CARD_H),
        ),
        (
            "library grid, last column",
            Rect::new(Layout::grid_x(COLS - 1), bare.row_y(0, 0.0), CARD_W, CARD_H),
        ),
        (
            "library chip (document head)",
            Rect::new(MARGIN_X, CONTENT_TOP, chip_width, crate::ui::widgets::StatusOverlay::CTRL_H),
        ),
        (
            "library shelf heading (first)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + head.shelf_origin(0) - crate::ui::consts::TITLE_DY,
                CARD_W,
                crate::ui::consts::TITLE_DY,
            ),
        ),
        (
            "library shelf tile (first)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + head.shelf_origin(0) + crate::ui::consts::CARD_DY,
                CARD_W,
                CARD_H,
            ),
        ),
        (
            "library grid heading (no chip, no shelves)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + bare.grid_block_top(),
                CARD_W,
                crate::ui::consts::TITLE_DY,
            ),
        ),
        (
            "library grid control row (no chip, no shelves)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + bare.grid_block_top()
                    + crate::ui::consts::TITLE_DY
                    + crate::ui::consts::CARD_DY,
                control_width,
                crate::ui::widgets::StatusOverlay::CTRL_H,
            ),
        ),
        ("library failure read-out band", screen.status_frame()),
    ];
    assert_eq!(rects.len(), 9, "the complete legacy Library probe must contribute nine bounds");
    for (name, rect) in rects {
        assert!(
            consts::inside_safe(rect),
            "{name} at ({}, {}) {}x{} leaves the safe area",
            rect.x,
            rect.y,
            rect.w,
            rect.h,
        );
    }
}

#[test]
fn a_focused_poster_tile_always_fills_the_caption_rung_it_reserves() {
    // Port the original Library assertion against the owned screen's production helper.
    let caption = |item: PmsMovie, is_continue| {
        let shelf = crate::browse::section_hubs::Shelf {
            id: "x".into(), title: "Recently Added".into(), is_continue,
            landscape: false, items: vec![item],
        };
        shelf_label(&shelf, 0).caption.map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    };
    assert_eq!(caption(PmsMovie { kind: 1, title: "The Bear".into(), year: 2022,
        ..Default::default() }, false), "2022", "shows reserve the same caption rung as films");
    assert_eq!(caption(PmsMovie { kind: 0, title: "Stardust".into(), year: 2007,
        ..Default::default() }, false), "2007");
    assert_eq!(caption(PmsMovie { kind: 0, title: "Stardust".into(), year: 2007,
        dur_ns: 60 * 60 * 1_000_000_000, resume_ms: 35 * 60 * 1000,
        ..Default::default() }, true), "25 min left", "a Continue Watching tile reports time left");
    assert_eq!(caption(PmsMovie { kind: 1, title: "Untitled".into(),
        ..Default::default() }, false), "", "missing source data does not invent a caption");
}

    #[test]
    fn home_and_library_leave_the_same_air_under_a_focused_label() {
        use crate::ui::consts::{CARD_DY, ROW_PITCH, TITLE_DY, UNDER_LABEL_AIR};
        let home_air = ROW_PITCH - TITLE_DY - CARD_DY - CARD_H - card_row::UNDER_LABEL_H;
        let library_air = PITCH - CARD_H - card_row::UNDER_LABEL_H;
        assert_eq!(home_air, UNDER_LABEL_AIR);
        assert_eq!(library_air, UNDER_LABEL_AIR);
        assert_eq!(
            home_air, library_air,
            "Home and Library must agree on the air under a focused label"
        );
    }

    #[test]
    fn a_focused_episode_tile_reveals_the_episode_and_not_the_show() {
        let ep = |s: c_int, e: c_int, title: &str, show: &str| PmsMovie {
            kind: 3,
            season_index: s,
            ep_index: e,
            title: title.into(),
            show_title: show.into(),
            ..Default::default()
        };
        let shelf = |items: Vec<PmsMovie>| crate::browse::section_hubs::Shelf {
            id: "tv.recentlyreleased".into(),
            title: "Recently Released Episodes".into(),
            is_continue: false,
            landscape: true,
            items,
        };

        // `TileLabel` holds `CString`s for the draw; these read them back as text
        let title = |l: &card_row::TileLabel| {
            l.title
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let caption = |l: &card_row::TileLabel| {
            l.caption
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };

        let dated = |m: PmsMovie| PmsMovie {
            aired: "2026-09-11".into(),
            ..m
        };

        let sh = shelf(vec![dated(ep(3, 4, "Violet", "The Bear"))]);
        let l = shelf_label(&sh, 0);
        assert_eq!(title(&l), "Violet", "the EPISODE's NAME takes the title rung");
        assert_eq!(
            caption(&l),
            "11 Sep 2026",
            "…over its one trailing FACT, which on a discovery shelf is the release date",
        );
        assert!(
            !title(&l).contains("The Bear") && !caption(&l).contains("The Bear"),
            "the show is printed on the artwork and must not be repeated below it"
        );
        assert!(
            !caption(&l).contains("S3") && !caption(&l).contains("E4"),
            "…and NEITHER is the address, which moved onto the artwork on 2026-09-05: four tiles \
             of one show differ only by number, so the number cannot be the thing behind focus"
        );

        // **Continue Watching trails time left instead**, because for a part-watched episode that
        // is the fact you came for. The shelf decides and not the item.
        let mut deck = shelf(vec![PmsMovie {
            dur_ns: 30 * 60 * 1_000_000_000,
            resume_ms: 6 * 60 * 1000,
            ..dated(ep(3, 4, "Violet", "The Bear"))
        }]);
        deck.is_continue = true;
        assert_eq!(caption(&shelf_label(&deck, 0)), "24 min left");
        // …and a deck tile never started has no time to report, so it falls back to the date
        // rather than to an empty rung.
        let mut next_up = shelf(vec![dated(ep(3, 5, "Ice Chips", "The Bear"))]);
        next_up.is_continue = true;
        assert_eq!(caption(&shelf_label(&next_up, 0)), "11 Sep 2026");

        // …an episode the server dated to nothing at all draws ONE rung, not an empty second one
        let sh = shelf(vec![ep(3, 4, "Violet", "The Bear")]);
        let l = shelf_label(&sh, 0);
        assert_eq!(title(&l), "Violet");
        assert_eq!(caption(&l), "");
        // …and an episode with no title of its own falls back to its address on the title rung
        let sh = shelf(vec![dated(ep(3, 4, "", "The Bear"))]);
        assert_eq!(title(&shelf_label(&sh, 0)), "S3 \u{b7} E4");
        // an episode whose title IS the show's says it once, as the address
        let sh = shelf(vec![dated(ep(2, 1, "X", "X"))]);
        assert_eq!(title(&shelf_label(&sh, 0)), "S2 \u{b7} E1");

        // …and a POSTER shelf is untouched: its tiles still name the item on focus
        let mut poster = shelf(vec![dated(ep(3, 4, "Violet", "The Bear"))]);
        poster.landscape = false;
        assert_eq!(title(&shelf_label(&poster, 0)), "Violet");
    }

    #[test]
    fn an_episode_shelf_takes_a_shorter_band_than_a_poster_shelf() {
        use crate::ui::consts::ROW_PITCH;
        // graded at the FOCUSED band, where the two shapes' difference is the tile height alone
        let open = 1.0; // owned layout uses the shared under-band expansion ratio
        let poster = shelf_pitch(false, open);
        let landscape = shelf_pitch(true, open);
        assert_eq!(poster, ROW_PITCH);
        assert!(landscape < poster, "a landscape row is shorter");
        assert_eq!(poster - landscape, CARD_H - RowStyle::EPISODE.h);

        // …and the document sums the ACTUAL pitches, so a mixed page puts the grid where the
        // shelves above it really end
        let mixed = Layout::new(true, &[landscape, poster, landscape], 40, true);
        assert_eq!(mixed.shelf_origin(0), mixed.library_h());
        assert_eq!(mixed.shelf_origin(1), mixed.library_h() + landscape);
        assert_eq!(mixed.shelf_origin(2), mixed.library_h() + landscape + poster);
        assert_eq!(
            mixed.grid_block_top(),
            mixed.library_h() + 2.0 * landscape + poster
        );
        // the uniform case is unchanged, which is what every other geometry test asserts
        let uniform = Layout::new(true, &[poster; 3], 40, true);
        assert_eq!(uniform.grid_block_top(), uniform.library_h() + 3.0 * poster);
    }
