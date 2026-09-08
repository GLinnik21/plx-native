//! Meaningful legacy caption and landscape assertions, on the owned production helpers.
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::CARD_H;
use std::os::raw::c_int;
use super::draw::shelf_label;
use super::layout::{Layout, shelf_pitch, GRID_PITCH as PITCH};

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
