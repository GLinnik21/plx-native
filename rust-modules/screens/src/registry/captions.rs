//! **The one place a focused poster's caption is read and built** from the Under posters setting
//! (Settings > Libraries > Under posters), for every poster shelf in the app: the Library grid, the
//! Collections page, the Watchlist, Home's shelves and the Library's Recommended shelves.
//!
//! - [`policy`] is the stored setting as the ui's [`CaptionPolicy`], read through
//!   `plx_media::route::under_posters()`. A test pins one setting for its thread with
//!   [`TEST_UNDER_POSTERS`] instead of writing the process-wide cell.
//! - [`label`] words the focused block from `(policy, facts)`: the Title switch decides whether the
//!   title line shows, the Details picker what the second line says. A tile that carries a STATE
//!   keeps it: its `state` line REPLACES the details line whenever the setting shows one (a
//!   Continue Watching tile's time left, a Watchlist title's release date or "Server offline"), and
//!   its amber play mark leads the title line whenever a press plays and the title is shown. With
//!   Details set to Nothing no band is opened ([`plx_ui::caption_policy::band_h`]), so no state
//!   line is shown either.
//! - The band's height is [`plx_ui::caption_policy::band_h`] of the same policy, everywhere: a
//!   shelf is handed the policy ([`plx_ui::cards::Shelf::set_caption_policy`]) and reserves, opens
//!   and reveals its band from it, so the label and the room for it cannot disagree.
use plx_ui::caption_policy::{caption_lines, CaptionFacts, CaptionPolicy, DetailsKind};
use plx_ui::cards::TileLabel;

/// The Under posters setting as the caption policy every poster shelf follows.
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
/// from `facts`, the details line being `state` when the tile has one, and the title led by the play
/// mark when `plays` (a press on this tile starts playback) and the title is shown.
pub fn label(policy: &CaptionPolicy, facts: &CaptionFacts, state: Option<String>, plays: bool) -> TileLabel {
    let [title, details] = caption_lines(policy, facts, plx_platform::i18n::current());
    let details = if policy.details == DetailsKind::None { None } else { state.or(details) };
    let glyph = plays && title.is_some();
    TileLabel { glyph, ..TileLabel::from_lines([title, details]) }
}

/// A poster shelf tile's label under `policy`: Home's shelves and the Library's Recommended
/// shelves. The tile keeps what its caption carries as a STATE: on the Continue Watching deck
/// (`is_continue`) its time left ("8m left", "Show · 8m left", or "Show · New episode" for a
/// next-up episode) is the details line whatever kind the setting asks for (but Nothing), and the
/// amber play mark leads the title while OK on the deck plays (`DeckPress::Play`) and the title is
/// shown. An episode's default details line is its address ("S1 • E8"), as it was on a shelf.
pub fn shelf_label(policy: &CaptionPolicy, item: &plx_data::pms::PmsMovie, is_continue: bool) -> TileLabel {
    let facts = crate::registry::tile_facts::caption_facts(item);
    let state = is_continue
        .then(|| plx_ui::cards::focused_caption(&crate::registry::tile_facts::of(item), true))
        .flatten()
        .map(|c| c.to_string_lossy().into_owned());
    label(policy, &facts, state, is_continue && plx_media::route::deck_press().press_plays())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::caption_policy::DetailsKind;

    fn lines(l: &TileLabel) -> [Option<String>; 2] {
        let text = |c: &Option<std::ffi::CString>| c.as_ref().map(|c| c.to_string_lossy().into_owned());
        [text(&l.title), text(&l.caption)]
    }

    /// A tile's state is its details line under every kind but Nothing, the play mark leads only a
    /// title that is shown, and an episode's default details line is its address.
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
        assert!(!with(false, DetailsKind::Genre, true).glyph, "no title line, nothing to lead");
        let episode = CaptionFacts { title: Some("Pilot".into()), episode: Some((1, 8)), ..Default::default() };
        let p = CaptionPolicy { title: false, details: DetailsKind::YearLength };
        assert_eq!(lines(&label(&p, &episode, None, false)), [None, Some("S1 \u{2022} E8".into())]);
    }
}
