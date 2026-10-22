//! **The one place a focused poster's caption is read and built** from the Under posters setting
//! (Settings > Libraries > Under posters), for EVERY poster tile: the Library grid, the Collections
//! page, the Watchlist, Home's shelves, the Library's Recommended poster shelves, Search's poster rows,
//! the person page's filmography, Detail's Related and collection shelves and the Collection screen's
//! members. (A landscape still or a headshot is not a poster and is not on the policy: the Library's
//! Episodes listing, Search's Episode and Person rows, Detail's Extras and Cast and Recommended's
//! landscape rows keep the legacy block, [`still_label`] / [`still_block`].)
//!
//! - [`policy`] is the stored setting as the ui's [`CaptionPolicy`], read through
//!   `plx_media::route::under_posters()`. A test pins one setting for its thread with
//!   [`TEST_UNDER_POSTERS`] instead of writing the process-wide cell.
//! - [`label`] words the focused block from `(policy, facts)`: the Title switch decides whether the
//!   title line shows, the Details picker what the second line says. **A line the setting turns on
//!   is never drawn empty, for any kind of tile** (`plx_ui::caption_policy::caption_lines`): the title
//!   line is the name a reader knows the tile by (`tile_facts::caption_facts` puts a season's SHOW
//!   there, an episode keeps its own title), and a details kind the tile has no facts for says its
//!   identity instead (a season "Season 6" or "Specials", an episode "S1 · E8", a show or movie its year, a collection
//!   its size) and a tile with not even that says what KIND it is ("Episode", "Movie", "Show", "Collection"), so a
//!   band the setting opens is never open over nothing. A tile that carries a STATE
//!   keeps it (a filmography poster's character is one, as a Continue Watching tile's time left is): its `state` line REPLACES the details line whenever the setting shows one (a
//!   Continue Watching tile's time left, a Watchlist title's release date or "Server offline"), and
//!   its amber play mark leads the FIRST line drawn whenever a press plays (the title, else the
//!   details line: a deck that plays on OK announces it under every setting that draws a line). With
//!   Title Off and Details set to Nothing no band is opened ([`plx_ui::caption_policy::band_h`]) and
//!   no line carries a mark; with Details Nothing the state line is dropped with the details line, so
//!   only the title (if on) shows.
//! - The band's height is [`plx_ui::caption_policy::band_h`] of the same policy, everywhere: a
//!   shelf is handed the policy ([`plx_ui::cards::Shelf::set_caption_policy`]) and reserves, opens
//!   and reveals its band from it, so the label and the room for it cannot disagree.
use plx_ui::caption_policy::{caption_lines, CaptionFacts, CaptionPolicy, DetailsKind};
use plx_ui::cards::TileLabel;

/// The Under posters setting as the caption policy the screens listed in the module doc follow.
pub fn policy() -> CaptionPolicy {
    #[cfg(test)]
    if let Some(setting) = TEST_UNDER_POSTERS.with(|cell| cell.get()) {
        return crate::registry::caption_policy_of(setting);
    }
    crate::registry::caption_policy_of(plx_media::route::under_posters())
}

#[cfg(test)]
thread_local! {
    /// A per-thread Under posters setting for a test, so it never writes the process-wide cell the
    /// other tests read through [`policy`].
    pub(crate) static TEST_UNDER_POSTERS: std::cell::Cell<Option<plx_plex::plex::session::UnderPosters>> =
        const { std::cell::Cell::new(None) };
}

/// The focused block of one poster under `policy`: `[title, details]` as the setting words them
/// from `facts`, the details line being `state` when the tile has one, and the first line drawn led by
/// the play mark when `plays` (a press on this tile starts playback).
pub fn label(policy: &CaptionPolicy, facts: &CaptionFacts, state: Option<String>, plays: bool) -> TileLabel {
    let [title, details] = caption_lines(policy, facts, plx_platform::i18n::current());
    let details = if policy.details == DetailsKind::None { None } else { state.or(details) };
    // The mark leads the FIRST line drawn (the title, else the details line), so a press that plays
    // is announced under every setting that draws a line; Nothing with the title off draws none.
    let glyph = plays && (title.is_some() || details.is_some());
    let label = TileLabel { glyph, ..TileLabel::from_lines([title, details]) };
    // A name the face cut rides the details line (`caption_lines`): it stays under its own tile
    // (never under the next column) and glides there when it is longer than the tile.
    if facts.name_cut { label.within_tile() } else { label }
}

/// A poster shelf tile's label under `policy`: Home's shelves and the Library's Recommended
/// shelves. The tile keeps what its caption carries as a STATE: on the Continue Watching deck
/// (`is_continue`) its time left ("8m left", "Show · 8m left", or "Show · New episode" for a
/// next-up episode) is the details line whatever kind the setting asks for (but Nothing), and the
/// amber play mark leads the first line drawn while OK on the deck plays (`DeckPress::Play`). An
/// episode's default details line is its address ("S1 · E8"), as it was on a shelf.
pub fn shelf_label(policy: &CaptionPolicy, item: &plx_data::pms::PmsMovie, is_continue: bool) -> TileLabel {
    let facts = crate::registry::tile_facts::caption_facts(item);
    let state = is_continue
        .then(|| plx_ui::cards::cw_caption(&crate::registry::tile_facts::of(item)))
        .flatten()
        .map(|c| c.to_string_lossy().into_owned());
    label(policy, &facts, state, is_continue && plx_media::route::deck_press().press_plays())
}

/// **A landscape STILL's focused block** (an episode's 16:9 art): NOT a poster and so off the Under
/// posters policy, in its own legacy band (114 px, [`plx_ui::cards::LEGACY_BAND_H`]: this 92 px
/// block and its air), under every setting: the episode's own title (its address when it has none,
/// or when it only repeats the show's name) over one fact, the time it has left on the Continue
/// Watching deck (`is_continue`), else its release date. `credit` is the server a result is borrowed
/// from (Search's results), named after the fact. The Library's Episodes listing, the Recommended
/// page's landscape rows and Search's Episode row read this ONE builder, which makes the block with
/// [`still_block`]; a line the row has nothing for is absent, never drawn empty.
pub fn still_label(item: &plx_data::pms::PmsMovie, is_continue: bool, credit: &str) -> TileLabel {
    let name = if item.title.is_empty() || item.title == item.show_title {
        plx_ui::fmt::episode_address(i64::from(item.season_index), i64::from(item.ep_index))
    } else {
        item.title.clone()
    };
    let fact = if is_continue && item.resume_frac().is_some() {
        plx_ui::fmt::time_left(item.dur_ns / 1_000_000 - item.resume_ms)
    } else if item.aired.is_empty() && item.year <= 0 {
        String::new()
    } else {
        plx_ui::fmt::pretty_date(&item.aired, i64::from(item.year))
    };
    let facts = [fact, credit.to_owned()].into_iter().filter(|part| !part.is_empty()).collect();
    still_block(&name, &plx_ui::caption_policy::join(facts).unwrap_or_default())
}

/// **The one place a landscape block's two lines are made** (shadowed like every caption, a line
/// with no words absent and never drawn empty): [`still_label`] words an episode's, and Detail's
/// Extras (a clip's name over its kind) hand theirs here, so no landscape row hand-builds a label
/// that looks different from its neighbours.
pub fn still_block(title: &str, fact: &str) -> TileLabel {
    let line = |text: &str| Some(text.to_owned()).filter(|text| !text.is_empty());
    TileLabel::from_lines([line(title), line(fact)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::caption_policy::DetailsKind;

    fn lines(l: &TileLabel) -> [Option<String>; 2] {
        let text = |c: &Option<std::ffi::CString>| c.as_ref().map(|c| c.to_string_lossy().into_owned());
        [text(&l.title), text(&l.caption)]
    }

    /// **Every landscape block is the one builder's**: an episode still, a Search result borrowed from
    /// another server, and an Extra's clip all come out shadowed (like every caption), a line with no
    /// words absent, the credit after the fact, an untitled episode by its address.
    #[test]
    fn every_landscape_block_is_made_by_the_one_builder() {
        let ep = plx_data::pms::PmsMovie { kind: 3, title: "Pilot".into(), show_title: "Show".into(), season_index: 1, ep_index: 8,
            aired: "2016-04-21".into(), ..Default::default() };
        let date = plx_ui::fmt::pretty_date("2016-04-21", 0);
        assert_eq!(lines(&still_label(&ep, false, "")), [Some("Pilot".into()), Some(date.clone())]);
        assert_eq!(lines(&still_label(&ep, false, "pal")), [Some("Pilot".into()), Some(format!("{date} \u{b7} pal"))]);
        let untitled = plx_data::pms::PmsMovie { title: "Show".into(), ..ep.clone() };
        assert_eq!(lines(&still_label(&untitled, false, ""))[0].as_deref(), Some("S1 \u{b7} E8"),
            "a title that only repeats the show reads the address");
        let extra = still_block("Making of", "Featurette");
        assert_eq!(lines(&extra), [Some("Making of".into()), Some("Featurette".into())]);
        assert_eq!(lines(&still_block("", "Featurette")), [None, Some("Featurette".into())], "no title, no empty line");
        assert!(extra.shadowed && still_label(&ep, false, "").shadowed, "one look for every landscape block");
    }

    /// A tile's state is its details line under every kind but Nothing, the play mark leads the first
    /// line drawn, and an episode's default details line is its address.
    #[test]
    fn a_state_line_replaces_the_details_line_and_the_play_mark_needs_a_title() {
        let facts = CaptionFacts { title: Some("Heat".into()), year: Some(1995), genre: Some("Crime".into()), ..Default::default() };
        let state = || Some("8m left".to_owned());
        let with = |title, details, plays| label(&CaptionPolicy { title, details }, &facts, state(), plays);
        for details in [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore] {
            assert_eq!(lines(&with(true, details, true)), [Some("Heat".into()), Some("8m left".into())], "{details:?}");
        }
        assert_eq!(lines(&with(true, DetailsKind::None, false)), [Some("Heat".into()), None], "Nothing shows no state either");
        assert!(with(true, DetailsKind::Genre, true).glyph && !with(true, DetailsKind::Genre, false).glyph);
        assert!(with(false, DetailsKind::Genre, true).glyph, "Title Off: the details line carries the mark");
        assert!(with(true, DetailsKind::None, true).glyph, "Title On + Nothing: the title carries it");
        assert!(!with(false, DetailsKind::None, true).glyph, "no line drawn, nothing to lead");
        let episode = CaptionFacts { title: Some("Pilot".into()), episode: Some((1, 8)), ..Default::default() };
        let p = CaptionPolicy { title: false, details: DetailsKind::YearLength };
        assert_eq!(lines(&label(&p, &episode, None, false)), [None, Some("S1 \u{b7} E8".into())]);
    }

    /// A SEASON on a shelf (Recently Added TV) is named by its show and, with no fact the picked
    /// details kind can say, by its own title: every setting that turns a line on draws it.
    #[test]
    fn a_season_tile_is_its_show_over_its_own_title_under_every_setting() {
        let season = plx_data::pms::PmsMovie {
            kind: 2, title: "Season 6".into(), show_title: "The Office".into(), season_index: 6, ..Default::default()
        };
        for details in [DetailsKind::YearLength, DetailsKind::Genre, DetailsKind::Video, DetailsKind::AgeRatingScore] {
            let at = |title| lines(&shelf_label(&CaptionPolicy { title, details }, &season, false));
            assert_eq!(at(true), [Some("The Office".into()), Some("Season 6".into())], "{details:?}");
            assert_eq!(at(false), [None, Some("Season 6".into())], "{details:?}");
        }
        let at = |title| lines(&shelf_label(&CaptionPolicy { title, details: DetailsKind::None }, &season, false));
        assert_eq!(at(true), [Some("The Office".into()), None]);
        assert_eq!(at(false), [None, None]);
    }
}
