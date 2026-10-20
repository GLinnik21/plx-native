//! **The room a focused caption opens, graded at the app tier** (`routes.md`, R13): what a poster
//! (or a still) DRAWS under it against the band the Under posters setting reserves for it, on a real
//! `Dispatcher<AppHost>` over a real store. A caption that overruns its band is painted before the
//! next row's tiles (`Grid::draw` paints every caption first), so the row below covers whatever it
//! overruns; these tests read the drawn runs that START in the gap between the focused card and the
//! next row (its caption) against the next row's tile top.

use super::test_support::World;
use super::under_posters_shelf_route_tests::{set, Cell};
use plx_plex::plex::session::UnderPostersDetails as Details;

pub(super) const PICKERS: [Details; 5] =
    [Details::YearLength, Details::Genre, Details::Video, Details::AgeRatingScore, Details::None];

/// **B1.** The Library's Episodes listing (landscape stills) is off the Under posters policy like
/// every other landscape surface: its two-line block (title, release date) is drawn into the legacy
/// band, so the date line never ends under the next row's stills, whatever the setting.
#[test]
fn the_episodes_listings_caption_clears_the_next_row_under_every_setting() {
    let mut w = World::episodes_library("band-episodes");
    let _cell = Cell::default();
    for title in [false, true] {
        for details in PICKERS {
            set(&mut w, title, details);
            let (card, cols) = w.library().probe_cell(0);
            let (below, _) = w.library().probe_cell(cols);
            let setting = format!("title {title}, details {details:?}");
            let drawn = w.drawn();
            let date = plx_ui::fmt::pretty_date("2019-01-03", 2019);
            let under: Vec<_> = drawn.runs.iter()
                .filter(|run| run.at.y >= card.y + card.h && run.at.y < below.y
                    && run.at.x >= card.x - 1.0 && run.at.x < card.x + card.w)
                .collect();
            assert!(under.iter().any(|run| run.text == date),
                "premise: the focused still draws its release date ({setting}): {:?}", drawn.words);
            for run in &under {
                let bottom = run.at.y + run.at.h.max(run.size as f32);
                assert!(bottom <= below.y, "{:?} ends at {bottom} under the next row's tiles at {} ({setting})", run.text, below.y);
            }
        }
    }
}

/// **The band the Library grid opens under its focused row is the policy's** (112 / 80 / 0 px), read
/// off the grid's own geometry on a real `Dispatcher<AppHost>`: the gap to the next row grows over
/// what a closed band (Title Off + Nothing) leaves by the growth the grid documents for each band
/// (`poster_grid::growth_before_for`: 72 px for a two-line band, 40 for a one-line one, 0 for none),
/// and the caption's lines end inside that gap, so the label drawn and the room reserved for it
/// cannot disagree.
#[test]
fn the_library_grid_opens_the_band_its_setting_asks_for() {
    use plx_ui::caption_policy::{band_h, CaptionPolicy};
    use plx_screens::registry::caption_policy_of;
    let mut w = World::movies_library("band-grid", false);
    let _cell = Cell::default();
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.frames(30);
    assert_eq!(w.grid_cell(), Some((0, 0)), "premise: the library opens on its first poster");
    let gap = |w: &mut World| {
        let (card, cols) = w.library().probe_cell(0);
        let (below, _) = w.library().probe_cell(cols);
        below.y - (card.y + card.h)
    };
    set(&mut w, false, Details::None);
    let closed = gap(&mut w);
    for (title, details, band, want) in [(true, Details::YearLength, 112.0, 72.0), (false, Details::YearLength, 80.0, 40.0),
        (true, Details::None, 80.0, 40.0), (false, Details::None, 0.0, 0.0)] {
        set(&mut w, title, details);
        let policy: CaptionPolicy = caption_policy_of(plx_plex::plex::session::UnderPosters { title, details });
        assert_eq!(band_h(&policy), band, "premise: the policy's band");
        let (card, cols) = w.library().probe_cell(0);
        let below = w.library().probe_cell(cols).0;
        let opened = gap(&mut w) - closed;
        assert!((opened - want).abs() < 0.5, "the grid opens {opened} px for title {title}, {details:?}; the policy asks {want}");
        let drawn = w.drawn();
        for line in drawn.runs.iter().filter(|run| run.at.y >= card.y + card.h && run.at.y < below.y
            && run.at.x >= card.x - 1.0 && run.at.x < card.x + card.w) {
            assert!(line.at.y + line.at.h.max(line.size as f32) <= below.y,
                "{:?} ends under the next row (title {title}, {details:?})", line.text);
        }
    }
}
