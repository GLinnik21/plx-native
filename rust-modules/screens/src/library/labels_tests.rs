//! Meaningful legacy caption and landscape assertions, on the owned production helpers.
use plx_data::pms::PmsMovie;
use plx_media::route::DeckPress;
use plx_ui::cards::{self as ui_cards, RowStyle};
use plx_ui::consts::{self, CARD_H, CARD_W, MARGIN_X};
use plx_ui::fixture::FixtureMeasure;
use plx_machine::machine::{EntryId, InstanceId};
use plx_ui::{Painter, Rect};
use std::os::raw::c_int;
use plx_data::stores::browse::SecKind;
use super::draw::shelf_label;
use super::layout::{self, Layout, ShelfRun, shelf_pitch, COLS, CONTENT_TOP, GRID_RIGHT, MAX_LETTERS,
    RAIL_CAP_PAD, RAIL_TRACK_W};

#[test]
fn grid_labels_identify_episodes_and_seasons() {
    let episode = PmsMovie { kind: 3, title: "The Bear".into(), show_title: "The Bear".into(),
        season_index: 3, ep_index: 4, ..Default::default() };
    assert_eq!(super::parts::grid_label(&episode).title.unwrap().to_str().unwrap(), "S3 · E4",
        "an episode without its own title must identify the episode, not repeat the show");
    let season = PmsMovie { kind: 2, title: "Season 3".into(), show_title: "The Bear".into(),
        season_index: 3, year: 2024, ..Default::default() };
    let label = super::parts::grid_label(&season);
    assert_eq!(label.title.unwrap().to_str().unwrap(), "Season 3");
    assert_eq!(label.caption.unwrap().to_str().unwrap(), "The Bear",
        "a mixed library of seasons must name the show each season belongs to");
}

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
    assert_eq!(super::parts::grid_label(&episode).caption.unwrap().to_str().unwrap(), "2020",
        "the still overlay carries the episode address; the focus caption adds its release date");
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

    let style = Layout::new(false, &[], 40, true).style();
    let (x, w) = ui_cards::label_band(p, card(COLS - 1), &style);
    assert!(
        x + w <= GRID_RIGHT + 0.01,
        "the last column's label reaches {} against the content edge {GRID_RIGHT}",
        x + w,
    );

    let (x0, w0) = ui_cards::label_band(p, card(0), &style);
    let (home_x0, _) = ui_cards::label_band(p, card(0), &RowStyle::HOME);
    assert_eq!(
        x0, home_x0,
        "the right-edge rail reserve must not move the first-column label: {x0} vs {home_x0}",
    );
    assert!(x0 >= 0.0 && x0 + w0 <= GRID_RIGHT + 0.01);
}

#[test]
fn episode_grid_art_and_focus_labels_use_the_landscape_card_contract() {
    let item = PmsMovie { kind: 3, still: "/episode/still".into(),
        thumb: "/show/poster".into(), ..Default::default() };
    let plx_ui::widgets::Art::Still(Some(art)) = super::parts::grid_art(&item) else {
        panic!("episodes must use their own still, not the show poster");
    };
    assert_eq!(plx_ui::widgets::still_key(&art), "/episode/still");
    let layout = Layout::new(false, &[], 40, true).with_episodes(true);
    let rect = Rect::new(layout.cell_x(layout.cols() - 1), layout.row_y(0, 0.0),
        layout.card_w(), layout.card_h());
    let (x, width) = ui_cards::label_band(Painter::root(), rect, &layout.style());
    assert!(x + width <= GRID_RIGHT + 0.01, "episode focus label must keep the rail clear");
    let season = PmsMovie { kind: 2, ..item };
    assert!(matches!(super::parts::grid_art(&season), plx_ui::widgets::Art::Poster(Some(_))));
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
    let head = Layout::new(true, &[plx_ui::consts::ROW_PITCH], 40, true);
    let head_run = ShelfRun::of(&[plx_ui::consts::ROW_PITCH]);
    let screen = crate::library::LibraryScreen::new(
        EntryId(1),
        InstanceId(1),
        SecKind::Movie,
    );
    // A singleton library draws no selector at all since issue #100/#165 (`LibraryScreen::sync`
    // clears `self.libraries` outright below two candidates), so there is no more solo "chip" rect
    // to probe here — the document head's real geometry, for any drawn row, is the shared pill
    // strip. Two favourites is the smallest row that ever reaches the screen.
    let pill_lays = plx_ui::widgets::strip_layout_measured(
        ["Cinema".to_string(), "Cinema 2".to_string()].into_iter(),
        MARGIN_X + plx_ui::widgets::STRIP_PAD,
        plx_ui::theme::size::BODY,
        plx_ui::widgets::STRIP_GAP_WIDE,
        &measure,
    );
    let pill_rect = plx_ui::widgets::strip_pill_rect(
        &pill_lays[0],
        CONTENT_TOP,
        plx_ui::widgets::StatusOverlay::CTRL_H,
    );
    let control_width = plx_ui::value_chip::ValueChip::width(
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
            Rect::new(bare.cell_x(0), bare.row_y(0, 0.0), CARD_W, CARD_H),
        ),
        (
            "library grid, last column",
            Rect::new(bare.cell_x(COLS - 1), bare.row_y(0, 0.0), CARD_W, CARD_H),
        ),
        (
            "library pill strip (document head)",
            pill_rect,
        ),
        (
            "library shelf heading (first)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + head.shelf_origin(&head_run, 0) - plx_ui::consts::TITLE_DY,
                CARD_W,
                plx_ui::consts::TITLE_DY,
            ),
        ),
        (
            "library shelf tile (first)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + head.shelf_origin(&head_run, 0) + plx_ui::consts::CARD_DY,
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
                plx_ui::consts::TITLE_DY,
            ),
        ),
        (
            "library grid control row (no chip, no shelves)",
            Rect::new(
                MARGIN_X,
                CONTENT_TOP + bare.grid_block_top()
                    + plx_ui::consts::TITLE_DY
                    + plx_ui::consts::CARD_DY,
                control_width,
                plx_ui::widgets::StatusOverlay::CTRL_H,
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

/// The deck's focused title carries the amber ▶ only while OK on the deck plays.
#[test]
fn the_deck_title_carries_the_play_glyph_only_while_the_press_plays() {
    let _guard = plx_base::testlock::serial();
    let prior = plx_media::route::deck_press();
    let glyph = |mode, is_continue| {
        plx_media::route::restore_deck_press(mode);
        let shelf = plx_data::browse::section_hubs::Shelf {
            id: "x".into(), key: String::new(), link: None, total: 0, title: "On Deck".into(), is_continue,
            landscape: false, items: vec![PmsMovie { kind: 0, title: "Stardust".into(), ..Default::default() }],
        };
        shelf_label(&shelf, 0).glyph
    };
    let seen = (
        glyph(DeckPress::Play, true), glyph(DeckPress::Details, true),
        glyph(DeckPress::Play, false), glyph(DeckPress::Details, false),
    );
    plx_media::route::restore_deck_press(prior);
    assert_eq!(seen, (true, false, false, false));
}

#[test]
fn a_focused_poster_tile_always_fills_the_caption_rung_it_reserves() {
    // Port the original Library assertion against the owned screen's production helper.
    let caption = |item: PmsMovie, is_continue| {
        let shelf = plx_data::browse::section_hubs::Shelf {
            id: "x".into(), key: String::new(), link: None, total: 0, title: "Recently Added".into(), is_continue,
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
        use plx_ui::consts::{CARD_DY, ROW_PITCH, TITLE_DY, UNDER_LABEL_AIR};
        let home_air = ROW_PITCH - TITLE_DY - CARD_DY - CARD_H - ui_cards::UNDER_LABEL_H;
        let focused = Layout::new(false, &[], 2, true).with_grid_focus(Some(0));
        let library_air = focused.row_y(1, 0.0) - focused.row_y(0, 0.0)
            - CARD_H - ui_cards::UNDER_LABEL_H;
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
        let shelf = |items: Vec<PmsMovie>| plx_data::browse::section_hubs::Shelf {
            id: "tv.recentlyreleased".into(),
            key: String::new(),
            link: None,
            total: 0,
            title: "Recently Released Episodes".into(),
            is_continue: false,
            landscape: true,
            items,
        };

        // `TileLabel` holds `CString`s for the draw; these read them back as text
        let title = |l: &ui_cards::TileLabel| {
            l.title
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let caption = |l: &ui_cards::TileLabel| {
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
            "9/11/2026",
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
        assert_eq!(caption(&shelf_label(&next_up, 0)), "9/11/2026");

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
        use plx_ui::consts::ROW_PITCH;
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
        let run = ShelfRun::of(&[landscape, poster, landscape]);
        assert_eq!(mixed.shelf_origin(&run, 0), mixed.library_h());
        assert_eq!(mixed.shelf_origin(&run, 1), mixed.library_h() + landscape);
        assert_eq!(mixed.shelf_origin(&run, 2), mixed.library_h() + landscape + poster);
        assert_eq!(
            mixed.grid_block_top(),
            mixed.library_h() + 2.0 * landscape + poster
        );
        // the uniform case is unchanged, which is what every other geometry test asserts
        let uniform = Layout::new(true, &[poster; 3], 40, true);
        assert_eq!(uniform.grid_block_top(), uniform.library_h() + 3.0 * poster);
    }

/// The artwork FALLBACK CHAIN: the episode's own still, then the show's POSTER, and its
/// backdrop only as a last resort.
///
/// The middle rung was the backdrop until `Library Screens.dc.html` E ruled otherwise — "where
/// an episode has no still, the tile falls back to the show's poster in the same frame,
/// cover-fitted, label and all. A crop is better than a row of mixed tile shapes." Both are
/// show-level images, so neither escapes the identical-tiles problem; what decides it is that
/// the poster is the show's IDENTIFYING artwork, which is what this tile's own label is about.
///
/// (Moved from `browse::section_hubs`'s tests: `still_key` is the UI's, so a test of it is
/// not the data layer's to own.)
#[test]
fn a_landscape_tile_prefers_the_episodes_own_still() {
    use crate::registry::tile_facts;
    use plx_ui::widgets::still_key;
    let full = PmsMovie {
        still: "/still".into(),
        art: "/art".into(),
        thumb: "/poster".into(),
        ..Default::default()
    };
    assert_eq!(still_key(&tile_facts::of(&full)), "/still");

    // no still — an ordinary answer for a specials folder or an item mid-scan
    let no_still = PmsMovie {
        art: "/art".into(),
        thumb: "/poster".into(),
        ..Default::default()
    };
    assert_eq!(
        still_key(&tile_facts::of(&no_still)),
        "/poster",
        "the show's own poster, cover-fitted in the same frame"
    );

    // …and the backdrop only when there is no poster either
    let bare = PmsMovie {
        art: "/art".into(),
        ..Default::default()
    };
    assert_eq!(still_key(&tile_facts::of(&bare)), "/art");
}

/// **A PARSED episode draws its OWN still even when its show has no poster.** The parse half of
/// this (`pms`'s `an_episode_keeps_its_own_still_without_a_show_poster`) grades what lands in
/// `PmsMovie::still`; this is the half that grades what the tile then DRAWS from it. `still`
/// used to be populated only when `grandparentThumb` was present, so with the show poster absent
/// the episode's own 16:9 frame survived only in `thumb`, and `still_key` — which prefers `art`
/// over `thumb` when there is no `still` — drew the show's shared backdrop: exactly the "every
/// episode is the same picture" symptom the landscape row was built to end.
#[test]
fn a_parsed_episode_draws_its_own_still_with_or_without_a_show_poster() {
    use crate::registry::tile_facts;
    use plx_ui::widgets::still_key;
    let ep = |gp: &str| {
        let it = plx_plex::plex::Metadata {
            kind: "episode".into(),
            rating_key: "9".into(),
            title: "The Meeting".into(),
            thumb: "/ep/still".into(),
            art: "/show/art".into(),
            grandparent_thumb: gp.into(),
            grandparent_title: "The Office".into(),
            ..Default::default()
        };
        plx_data::pms::parse_item(&it, plx_plex::plex::ServerId::from_raw(0))
    };

    // the show HAS a poster: the poster substitution stands, and the still is kept beside it
    let with = ep("/show/poster");
    assert_eq!(still_key(&tile_facts::of(&with)), "/ep/still");

    // …and with no show poster the still is STILL the episode's own frame, not the backdrop
    let without = ep("");
    assert_eq!(
        still_key(&tile_facts::of(&without)),
        "/ep/still",
        "…and never the show's shared art, which is the same picture on every episode"
    );
}
