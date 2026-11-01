//! The Info section's data and geometry: what the playing item says about itself (`info_content`, the
//! meta line's chips, the live playback fact), the card's rect and its action column. The card is drawn
//! by the HUD's Info section (`player_hud::sections`); the screen's keys and focus are the player
//! screen's, over the compiled state's buttons.
use crate::chip_row::{Chip, ChipRow};
use plx_data::metadata;
use plx_ui::consts::{SCR_H, SCR_W};
use plx_ui::theme;
use plx_ui::Rect;

/// The action column's length: `actions` always returns two labels, only their TEXT depends on
/// `is_episode`. [`button_rect`] sizes the column from this constant instead of building the array.
const N_ACTIONS: usize = 2;

/// The ratingKey of the page the playing item belongs to: the show for an episode, the movie
/// itself otherwise. What the Info card's second action opens, and what the `go_to_detail`
/// command opens and is enabled by. `None` when nothing playing has one.
pub fn detail_rk<'a>(meta: metadata::MetadataView<'a>) -> Option<&'a str> {
    meta.now_playing()
        .map(|n| n.detail_rk.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| meta.current().map(|d| d.rk.as_str()))
        .filter(|s| !s.is_empty())
}

/// whether the playing item is an episode (→ "Go to Show") rather than a movie ("Go to Movie").
/// A trailer session does not install a playing-leaf descriptor, so with `now_playing` absent
/// the loaded page is the parent — a show's trailer still says Go to Show.
fn is_episode(meta: metadata::MetadataView<'_>) -> bool {
    meta.now_playing()
        .map(|n| n.is_episode)
        .unwrap_or_else(|| meta.current().is_some_and(|d| d.is_show))
}

/// the action-button labels for the playing item. An ARRAY, not a `Vec`: the count is fixed at
/// [`N_ACTIONS`], so a caller that only needs the count uses that constant instead and this is called
/// only where the actual label TEXT is wanted.
pub(crate) fn actions(meta: metadata::MetadataView<'_>) -> [&'static str; 2] {
    [
        plx_platform::i18n::msg::widgets_info_from_beginning(),
        if is_episode(meta) {
            plx_platform::i18n::msg::widgets_info_go_show()
        } else {
            plx_platform::i18n::msg::widgets_info_go_movie()
        },
    ]
}

/// The card's own rect and its side padding — **pure**, since every input is a fixed layout
/// constant (`SCR_W`/`SCR_H` alone). Shared by the compiled card (the still/title/synopsis layout) and
/// [`button_rect`] (the action column), so the two formulas can never drift apart.
pub(crate) fn card_geometry() -> (Rect, f32) {
    let cx = 80.0f32;
    let cw = SCR_W - 160.0;
    let ch = 236.0f32;
    let cyt = SCR_H - 176.0 - ch; // sit just above the Info/Chapters tabs (tabs at SCR_H-128)
    (Rect::new(cx, cyt, cw, ch), 28.0f32)
}

/// The `i`-th action button's rect, widened for the longest translated action label. The compiled
/// state's buttons stand at it (`compile_tests`), so a stop built from it lands on the pixel the button
/// is drawn at.
pub(crate) fn button_rect(i: usize, measure: &dyn plx_machine::machine::Measure) -> Rect {
    let (card, pad) = card_geometry();
    let labels = [
        plx_platform::i18n::msg::widgets_info_from_beginning_c(),
        plx_platform::i18n::msg::widgets_info_go_show_c(),
        plx_platform::i18n::msg::widgets_info_go_movie_c(),
    ];
    let bw = labels.iter().map(|label| {
        plx_ui::widgets::Button::pill_w_measured(label, theme::size::BODY, true, None, measure)
    }).fold(352.0f32, f32::max);
    let bh = 70.0f32;
    let bx = card.x + card.w - pad - bw;
    let n = N_ACTIONS;
    let total_bh = n as f32 * bh + n.saturating_sub(1) as f32 * 16.0;
    let by0 = card.y + (card.h - total_bh) * 0.5;
    Rect::new(bx, by0 + i as f32 * (bh + 16.0), bw, bh)
}

/// **What the Info section says about the playing item**, resolved once: `now_playing` describes the
/// episode (show title + SxEy + its still) or the movie, the loaded `Detail` backs the genres, and the
/// capability badges come from the PLAYING item's own tracks (`current()` is the show, with episode 1's
/// streams, during a show-page episode play, or another item entirely). The ONE reading of those
/// fields: the HUD renderer's Info section arms draw from it.
pub(crate) struct InfoContent<'a> {
    /// the playing leaf's own name: the episode's, else the movie's (the show-title + SxEy treatment
    /// lives on the transport HUD under the playbar, not here)
    pub title: String,
    pub summary: String,
    pub year: i64,
    pub dur_ms: i64,
    pub rating: String,
    /// the 16:9 still (episode) or landscape art (movie)
    pub thumb: String,
    pub genres: &'a [String],
    pub audio: &'a [metadata::Stream],
    pub subs: &'a [metadata::Stream],
}

/// The playing item's [`InfoContent`], or `None` when nothing playing has a name to show.
pub(crate) fn info_content<'a>(meta: metadata::MetadataView<'a>) -> Option<InfoContent<'a>> {
    let np = meta.now_playing();
    let d = meta.current();
    if np.is_none() && d.is_none() {
        return None;
    }
    let is_ep = np.map(|n| n.is_episode).unwrap_or(false);
    let big_title = np.map(|n| n.title.clone()).or_else(|| d.map(|x| x.title.clone())).unwrap_or_default();
    let ep_name = np.map(|n| n.ep_title.clone()).unwrap_or_default();
    let summary = np
        .map(|n| n.summary.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| x.summary.clone()))
        .unwrap_or_default();
    let thumb = np
        .map(|n| n.thumb.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| d.map(|x| if !x.art.is_empty() { x.art.clone() } else { x.thumb.clone() }))
        .unwrap_or_default();
    let (audio, subs): (&[metadata::Stream], &[metadata::Stream]) = match meta.playing() {
        Some(t) => (&t.audio, &t.subs),
        None => (d.map(|x| x.audio.as_slice()).unwrap_or(&[]), d.map(|x| x.subs.as_slice()).unwrap_or(&[])),
    };
    Some(InfoContent {
        title: if is_ep { ep_name } else { big_title },
        summary,
        year: np.map(|n| n.year).or_else(|| d.map(|x| x.year)).unwrap_or(0),
        dur_ms: np.map(|n| n.dur_ms).or_else(|| d.map(|x| x.dur_ms)).unwrap_or(0),
        rating: np.map(|n| n.rating.clone()).filter(|s| !s.is_empty()).or_else(|| d.map(|x| x.rating.clone())).unwrap_or_default(),
        thumb,
        genres: d.map(|x| x.genres.as_slice()).unwrap_or(&[]),
        audio,
        subs,
    })
}

/// A package's `facts` cell as the chip builder reads it (`DrawOp::Facts`): which of the item's own
/// facts, in what order, how many genres, whether the live playback fact follows, which badges, in
/// what priority order, and the inks.
pub(crate) struct FactsShape<'a> {
    /// `genres`, `year`, `duration`, in the order they run
    pub order: &'a [&'a str],
    /// `content_rating`, `audio_format`, `cc`, `sdh`, `ad`, in drop-priority order
    pub badges: &'a [&'a str],
    pub genres_max: usize,
    pub playback_fact: bool,
    pub ink: [f32; 4],
    pub bold: i32,
}

/// **The metadata line and the capability badges, built and measured** but not yet fit or drawn: the
/// item's own facts as ONE text run (parts a dot apart exactly as the Detail page's identity line
/// spaces them, `widgets::DottedRun`), the live playback fact in its own quieter run (what the server
/// is actually sending, [`playback_now`]: REGULAR weight and tertiary ink, because it is a fact about
/// this session and not a property of the item; no chip and no capsule, a conversion that worked is
/// not a warning), then the badges. Every candidate's width is known before any of them touches the
/// screen, so the caller's `row.fit(tw)` decides what fits ([`ChipRow`]: tail-first drop, issue #26:
/// the chips used to draw straight under the action column).
///
/// The row's actual chip COUNT, not "there might be something to say", is what the caller's layout
/// must reserve a line for: a single candidate wider than the room fits down to nothing.
pub(crate) fn info_chips(
    c: &InfoContent<'_>,
    shape: &FactsShape<'_>,
    ps: &plx_media::route::PlaybackSession,
    measure: &dyn plx_machine::machine::Measure,
) -> ChipRow {
    // What the server is actually sending, straight off the resolved route. `has_url`, not
    // `!url().is_empty()`: the card draws at ~60/s, and `url()` clones the longest string in the app.
    let now_fact = shape.playback_fact.then(|| {
        playback_now(
            plx_media::route::has_url(ps) && !plx_media::route::play_pending(),
            plx_media::route::is_transcoding(ps),
            plx_media::route::is_remux(ps),
            &plx_media::route::stream_vcodec(ps),
        )
    });
    let now_fact = now_fact.flatten();
    let mut chips = ChipRow::new();
    let mut meta = Vec::new();
    for part in shape.order {
        match *part {
            "genres" => meta.extend(c.genres.iter().take(shape.genres_max).cloned()),
            "year" if c.year > 0 => meta.push(c.year.to_string()),
            "duration" if c.dur_ms > 0 => meta.push(plx_ui::fmt::duration(c.dur_ms)),
            _ => {}
        }
    }
    let has_meta = !meta.is_empty();
    if has_meta {
        chips.push(Chip::run(&meta, false, shape.bold, shape.ink, measure));
    }
    // It continues the meta run, so it leads with a dot when there is one. Whether it is needed depends
    // only on whether the meta line EXISTS, not on whether it ends up FITTING: a chip only draws once
    // every higher-priority chip already fit.
    if let Some(fact) = now_fact.as_ref() {
        chips.push(Chip::run(std::slice::from_ref(fact), has_meta, 0, theme::TEXT_TERTIARY, measure));
    }
    for badge in shape.badges {
        match *badge {
            "content_rating" if !c.rating.is_empty() => chips.push(Chip::badge(c.rating.clone(), measure)),
            "audio_format" => {
                if let Some(tag) = c.audio.first().and_then(|s| audio_badge(&s.codec)) {
                    chips.push(Chip::badge(tag, measure));
                }
            }
            "cc" if !c.subs.is_empty() => chips.push(Chip::badge(plx_platform::i18n::msg::widgets_badge_cc().to_string(), measure)),
            "sdh" if c.subs.iter().any(|s| s.sdh) => chips.push(Chip::badge(plx_platform::i18n::msg::widgets_badge_sdh().to_string(), measure)),
            "ad" if c.audio.iter().any(|s| s.ad) => chips.push(Chip::badge(plx_platform::i18n::msg::widgets_badge_ad().to_string(), measure)),
            _ => {}
        }
    }
    chips
}

// ---- helpers ----

/// A premium audio format worth badging on the meta line, named by the ONE codec map
/// ([`metadata::friendly_codec`]); everyday codecs (AAC/MP3/…) get no badge.
fn audio_badge(codec: &str) -> Option<String> {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "truehd" | "eac3" | "ec-3" | "ac3" | "dts" | "dca"
    )
    .then(|| metadata::friendly_codec(codec))
}

/// A VIDEO codec's display name. The sibling of [`metadata::friendly_codec`], which is the one
/// AUDIO/subtitle map and spells h264 as "H264" — the dotted form is the one everybody else's
/// player uses for the video lane, and this is the only place the app names a video codec to a
/// user. An unknown codec is upper-cased rather than dropped: "Converting · AV1" is still the
/// truth on a set that cannot decode AV1 (the dev set, and every set whose codec table lists no AV1
/// decoder), and inventing a friendly name for a codec we have never seen would not be.
pub fn video_codec_name(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" => "H.264".to_string(),
        "hevc" | "h265" | "hvc1" | "hev1" => "HEVC".to_string(),
        other => other.to_uppercase(),
    }
}

/// The live playback fact for the meta line: **what the server is actually sending**, read off the
/// running stream and never predicted.
///
/// That distinction is the whole design of this line. Everything else on the row is a property of
/// the ITEM (genre, year, runtime), knowable before Play; this is a property of THIS SESSION, and
/// the only honest source for it is `route`'s own record of what was resolved — `stream_vcodec` is
/// the codec the `/decision` OUTPUT named, not the one the profile asked for. Predicting it from
/// the source file is how a client ends up telling the user it is direct playing a file the server
/// quietly re-encoded.
///
/// Three answers, because the server has three behaviours and only one of them touches the pixels:
/// a direct play (we pull the file), a container-only REMUX (the video is always copied and the
/// audio is copied when the profile admits it, converted otherwise — Plex's own "Direct Stream";
/// the mock has no case for it because its model has only "direct play" and "converts", but
/// calling a remux a conversion would state that the server re-encoded the pixels when it did
/// not), and a real re-encode, which names the codec it is producing. Hardware vs software is
/// deliberately NOT stated: that is the server's runtime choice and it reaches the client only in
/// the live transcode session, so naming it would be a guess.
///
/// **No warning belongs on this line.** The tone-mapping case in particular lives on the DETAIL
/// page, before Play: this card is behind a keypress, so a warning here is one the user finds only
/// after watching a washed-out picture. That is `Player Screen.dc.html`'s own standing comment
/// ("Warning before Play is the DETAIL page's job"), and it contradicts a bullet in the **design
/// project's** `CLAUDE.md` — the Pass-rules file in the Claude Design project, NOT this repo's root
/// `CLAUDE.md`, which says nothing about tone mapping — that lists this card beside the detail
/// facts row. The mock is the later artifact and its reasoning matches the owner's standing rule
/// that a warning exists to be seen BEFORE Play, so the code follows it; the two documents still
/// need reconciling by the owner, and that is a docs edit, not a behaviour change.
///
/// PURE, so every arm is host-testable. `streaming` is "there is a resolved stream to describe".
/// The caller composes it from `route::has_url()` **and** `!route::play_pending()`, and the second
/// half is load-bearing rather than belt-and-braces: `request_play` does not clear `URL`,
/// `TSESSION` or `CUR_REMUX` (only a real stop does, in `engine::teardown`), so on an item→item
/// switch — Up Next, or Play from a detail page while a session is live — every one of those three
/// still describes the PREVIOUS item for the whole resolve. Without the pending test this line
/// would state the last session's fact as this one's.
pub fn playback_now(
    streaming: bool,
    transcoding: bool,
    remux: bool,
    vcodec: &str,
) -> Option<String> {
    if !streaming {
        return None;
    }
    if !transcoding {
        return Some(plx_platform::i18n::msg::widgets_playback_direct_play().to_string());
    }
    if remux {
        return Some(plx_platform::i18n::msg::widgets_playback_direct_stream().to_string());
    }
    let name = video_codec_name(vcodec);
    // a re-encode whose output codec we somehow do not know still converted — say that much
    Some(if name.is_empty() {
        plx_platform::i18n::msg::widgets_playback_converting().to_string()
    } else {
        plx_platform::i18n::msg::widgets_playback_converting_codec(&name)
    })
}

#[cfg(test)]
mod tests {
    use super::playback_now;

    /// The meta line's live fact, arm by arm. It is the one thing on that row the app could get
    /// wrong by GUESSING, so each arm pins a different way of guessing:
    ///   * a direct play says so and names no codec — there is no conversion to describe;
    ///   * a container remux is NOT a conversion (the video is copied, and the audio too when the
    ///     profile admits it), and calling it one would state that the server touched the pixels
    ///     when it did not — `route::is_remux`'s own doc
    ///     is that these are different facts;
    ///   * a re-encode names the codec the server is actually PRODUCING (`route::stream_vcodec` is
    ///     the `/decision` OUTPUT), spelled the way a viewer reads it, and HEVC output is reachable
    ///     only on a Pass server — which this function neither knows nor asks, because it reports
    ///     what happened rather than what was allowed;
    ///   * and with no stream resolved there is no fact at all: during the resolve window nothing
    ///     has been sent yet, and stating a fact about it would be exactly the prediction the whole
    ///     line refuses to make.
    #[test]
    fn the_meta_lines_playback_fact_describes_the_stream_that_is_running() {
        assert_eq!(
            playback_now(true, false, false, "hevc").as_deref(),
            Some("Direct Play")
        );
        // remux: `vcodec` is the SOURCE codec copied through, and it is still not a conversion
        assert_eq!(
            playback_now(true, true, true, "hevc").as_deref(),
            Some("Direct Stream")
        );
        assert_eq!(
            playback_now(true, true, false, "h264").as_deref(),
            Some("Converting \u{b7} H.264")
        );
        assert_eq!(
            playback_now(true, true, false, "hevc").as_deref(),
            Some("Converting \u{b7} HEVC")
        );
        // a codec map we have never met is upper-cased, not dropped and not renamed
        assert_eq!(
            playback_now(true, true, false, "av1").as_deref(),
            Some("Converting \u{b7} AV1")
        );
        // a re-encode whose output codec never landed still converted
        assert_eq!(
            playback_now(true, true, false, "").as_deref(),
            Some("Converting")
        );
        // nothing resolved yet → no fact, on every combination of the flags
        for (t, r) in [(false, false), (true, false), (true, true)] {
            assert!(
                playback_now(false, t, r, "h264").is_none(),
                "no stream, no fact"
            );
        }
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    /// Two buttons stack vertically, sharing an x and a width, in the order the card places them —
    /// "From Beginning" above "Go to Show"/"Go to Movie".
    #[test]
    fn the_two_buttons_share_a_column_and_stack_in_draw_order() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let (a, b) = (button_rect(0, &measure), button_rect(1, &measure));
        assert_eq!(a.x, b.x);
        assert_eq!(a.w, b.w);
        assert!(a.y < b.y, "From Beginning sits above the second action");
    }
}

#[cfg(test)]
mod detail_tests {
    use super::*;

    // TEST ONLY: a thread-confined store, so `set_current_for_test`/`apply` and the `view()`
    // this test's `detail_rk`/`is_episode` calls read from are the SAME owner, not two disconnected
    // `MetadataStore::default()`s (same pattern as `screens::detail::tests`'s `TEST_METADATA`).
    thread_local! {
        static TEST_METADATA: std::cell::UnsafeCell<plx_data::stores::metadata::MetadataStore> =
            std::cell::UnsafeCell::new(plx_data::stores::metadata::MetadataStore::default());
    }

    fn test_store() -> &'static mut plx_data::stores::metadata::MetadataStore {
        TEST_METADATA.with(|cell| unsafe { &mut *cell.get() })
    }

    #[test]
    fn go_to_after_a_trailer_opens_the_loaded_parent() {
        let _g = plx_base::testlock::serial();
        test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(None));
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            rk: "parent-movie".into(),
            kind: "movie".into(),
            ..Default::default()
        }));
        assert_eq!(detail_rk(test_store().view()), Some("parent-movie"));
        assert!(!is_episode(test_store().view()), "a movie parent labels Go to Movie");

        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            rk: "parent-show".into(),
            kind: "show".into(),
            is_show: true,
            ..Default::default()
        }));
        assert_eq!(detail_rk(test_store().view()), Some("parent-show"));
        assert!(is_episode(test_store().view()), "a show parent labels Go to Show");

        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(plx_data::metadata::Detail {
            sid: plx_plex::plex::ServerId::UNSET,
            rk: "parent-show".into(),
            kind: "show".into(),
            is_show: true,
            title: "Show".into(),
            extras: vec![plx_data::metadata::Extra {
                rk: "9".into(),
                title: "Official Trailer".into(),
                dur_ms: 120_000,
                part: "/p".into(),
                ..Default::default()
            }],
            ..Default::default()
        }));
        let trailer = plx_data::metadata::trailer_now_playing(test_store().state(), plx_plex::plex::ServerId::UNSET, "9");
        test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(trailer));
        assert_eq!(detail_rk(test_store().view()), Some("parent-show"));
        assert!(is_episode(test_store().view()), "an installed show-trailer card still says Go to Show");
        assert_eq!(
            test_store().view().now_playing().map(|n| n.dur_ms),
            Some(120_000),
            "the extra's duration, not the show's"
        );

        test_store().run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(None));
        plx_data::metadata::set_current_for_test(test_store().state_mut(), None);
    }
}
