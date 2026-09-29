//! The Detail page's About footer.
//!
//! Only the synopsis card and Languages column are controls. Information and Accessibility remain
//! readable content, not dead focus stops.

use std::ffi::CString;

use crate::metadata::Detail;
use crate::ui::machine::{GroupId, Measure};
use crate::ui::text_view::TextView;
use crate::ui::{theme, Painter, Rect};

pub(crate) const ABOUT_ELEM_RANGE_START: u32 = 1664;
pub(crate) const ABOUT_ELEM_RANGE_END: u32 = 1728;
pub(crate) const ABOUT_GROUP: GroupId = GroupId(5);
pub(crate) const CARD_ELEM: u32 = ABOUT_ELEM_RANGE_START;
pub(crate) const LANGUAGES_ELEM: u32 = ABOUT_ELEM_RANGE_START + 1;

const CARD_W: f32 = 640.0;
const CARD_Y: f32 = 50.0;
const CARD_PAD: f32 = 30.0;
const COL_Y: f32 = 430.0;
const LANG_X: f32 = 760.0;
/// The Languages column's text measure — the width the audio list wraps to, and what the focus
/// plate is [`CARD_PAD`] wider than on each side.
const LANG_W: f32 = 500.0;
/// Air between the block's last line and the MORE mark, matching the About card's own.
const LANG_MORE_LEAD: f32 = 30.0;
/// Air between the synopsis' last line and the MORE pinned on it — the same air the Person bio
/// and the Collection summary keep before theirs.
const MORE_GAP: f32 = theme::space::LG;
/// The About card's synopsis measure.
const SYNOPSIS_W: f32 = CARD_W - 2.0 * CARD_PAD;

/// The About card's synopsis: ONE builder for the height `card_rect` reserves and the block
/// `draw` paints, so the two cannot disagree about the wrap.
fn synopsis<'a>(summary: &'a str, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(summary, theme::size::CAPTION, theme::TEXT_HEADING)
        .with_measure(measure)
        .leading(30.0)
        .max_lines(5)
        .fade_for_more(MORE_GAP)
        .mark_always()
}

/// The Languages column's audio list: ONE builder for the height `languages_rect` reserves and
/// the block `draw_languages` paints. Its MORE is NOT pinned on the last line — it sits on its own
/// row [`LANG_MORE_LEAD`] below the block — so the fade here only marks a TRUNCATED list and the
/// view deliberately does not declare `mark_always`: a complete last line has nothing to run under.
fn audio_view<'a>(list: &'a str, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(list, theme::size::LABEL, theme::TEXT_HEADING)
        .with_measure(measure)
        .leading(32.0)
        .max_lines(6)
        .fade_for_more(MORE_GAP)
}

/// Where the Languages plate's MORE is drawn (`Painter::text`'s `y`) for the plate `plate`.
fn languages_more_y(plate: Rect) -> f32 {
    plate.y + plate.h - CARD_PAD - theme::size::CAPTION as f32
}

pub(crate) fn locate(key: u32, tracks_available: bool) -> Option<usize> {
    match key {
        CARD_ELEM => Some(0),
        LANGUAGES_ELEM if tracks_available => Some(1),
        _ => None,
    }
}

pub(crate) struct Rows {
    dirty: bool,
    identity: Option<(crate::plex::ServerId, String)>,
    info: Vec<(&'static str, String)>,
    orig_audio: Option<String>,
    audio_list: String,
    access: Vec<(&'static str, &'static str)>,
}

impl Rows {
    pub(crate) fn new() -> Self {
        Self {
            dirty: true,
            identity: None,
            info: Vec::new(),
            orig_audio: None,
            audio_list: String::new(),
            access: Vec::new(),
        }
    }

    pub(crate) fn update(&mut self, d: &Detail) {
        if !self.dirty
            && self
                .identity
                .as_ref()
                .is_some_and(|(sid, rk)| *sid == d.sid && rk == &d.rk)
        {
            return;
        }
        self.dirty = false;
        self.identity = Some((d.sid, d.rk.clone()));
        self.info.clear();
        let released = crate::ui::fmt::pretty_date(&d.aired, d.year);
        if !released.is_empty() {
            self.info.push((crate::i18n::msg::browse_detail_released(), released));
        }
        let dur = if d.dur_ms > 0 {
            d.dur_ms
        } else {
            d.episodes.first().map(|e| e.dur_ms).unwrap_or(0)
        };
        if dur > 0 {
            self.info.push((crate::i18n::msg::browse_detail_runtime(), crate::ui::fmt::dur_long(dur)));
        }
        self.info.push((
            crate::i18n::msg::browse_detail_rated(),
            if d.rating.is_empty() {
                crate::i18n::msg::browse_detail_unrated().into()
            } else {
                d.rating.clone()
            },
        ));
        if !d.countries.is_empty() {
            self.info
                .push((crate::i18n::msg::browse_detail_origins(), d.countries.join(", ")));
        }
        self.orig_audio = d.audio.first().map(|a| {
            if a.lang.is_empty() {
                crate::i18n::msg::browse_detail_unknown().into()
            } else {
                a.lang.clone()
            }
        });
        self.audio_list = d
            .audio
            .iter()
            .take(8)
            .map(|a| {
                let lang = if a.lang.is_empty() {
                    crate::i18n::msg::browse_detail_unknown()
                } else {
                    &a.lang
                };
                format!("{} ({})", lang, a.codec.to_uppercase())
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.access.clear();
        if !d.subs.is_empty() {
            self.access.push((
                crate::i18n::msg::widgets_badge_cc(),
                crate::i18n::msg::browse_detail_closed_captions(),
            ));
        }
        if d.subs.iter().any(|s| s.sdh) {
            self.access.push((crate::i18n::msg::widgets_badge_sdh(), crate::i18n::msg::browse_detail_sdh()));
        }
        if d.audio.iter().any(|a| a.ad) {
            self.access.push((
                crate::i18n::msg::widgets_badge_ad(),
                crate::i18n::msg::browse_detail_audio_description(),
            ));
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn card_rect(&self, d: &Detail, top: f32, measure: &dyn Measure) -> Rect {
        let h = synopsis(&d.summary, measure).measure_h(SYNOPSIS_W).max(30.0);
        Rect::new(
            crate::ui::consts::MARGIN_X,
            top + CARD_Y,
            CARD_W,
            CARD_PAD + 100.0 + h + CARD_PAD,
        )
    }

    pub(crate) fn languages_rect(&self, top: f32, measure: &dyn Measure) -> Rect {
        let mut h = 68.0;
        if let Some(orig) = &self.orig_audio {
            h += pair_h(orig, measure);
        }
        if !self.audio_list.is_empty() {
            h += 34.0 + audio_view(&self.audio_list, measure).measure_h(LANG_W);
        }
        // The plate wears the ABOUT CARD's padding — `CARD_PAD` on all four sides — because the
        // owner named that card as the reference for what these insets should look like
        // (2026-09-18). Its height therefore has to carry the MORE mark too: MORE is INK, the
        // block's measured `h` ends at the last audio line, and pinning the mark to the plate's
        // own bottom edge is what used to leave it all but touching it while the heading sat under
        // 48px of air.
        Rect::new(
            LANG_X - CARD_PAD,
            top + COL_Y - CARD_PAD,
            LANG_W + 2.0 * CARD_PAD,
            CARD_PAD + h + LANG_MORE_LEAD + theme::size::CAPTION as f32 + CARD_PAD,
        )
    }

    /// `tracks` is the PAGE's answer to "is there a file for the track sheet to describe"
    /// (`DetailScreen::tracks_available`), passed in rather than asked of the sheet: it decides
    /// whether the Languages column carries a MORE affordance, and it has to be the same bit
    /// `about::locate` gates the element on or the column reads as pressable and is not.
    pub(crate) fn draw(
        &self,
        p: Painter,
        d: &Detail,
        top: f32,
        focused: Option<u32>,
        tracks: bool,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        let x = crate::ui::consts::MARGIN_X;
        p.text(
            crate::i18n::msg::browse_detail_about_c().as_ptr(),
            x,
            top,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            0,
            1,
        );
        let card = self.card_rect(d, top, measure);
        if focused == Some(CARD_ELEM) {
            crate::ui::widgets::text_block_highlight(p, card);
        }
        let ix = card.x + CARD_PAD;
        text_at(
            p,
            ix,
            card.y + CARD_PAD,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            1,
            &crate::text::elide_by(&d.title, card.w - 2.0 * CARD_PAD, false, |t| {
                measure.width_str(t, theme::size::HEADLINE, true)
            }),
        );
        if !d.genres.is_empty() {
            text_at(
                p,
                ix,
                card.y + CARD_PAD + 44.0,
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
                0,
                &crate::text::elide_by(&d.genres.join(", "), card.w - 2.0 * CARD_PAD, false, |t| {
                    measure.width_str(t, theme::size::CAPTION, false)
                }),
            );
        }
        synopsis(&d.summary, measure)
            .draw(p, Rect::new(ix, card.y + CARD_PAD + 100.0, SYNOPSIS_W, 0.0));
        p.text(
            crate::ui::text_view::more_mark().as_ptr(),
            card.x + card.w - CARD_PAD,
            card.y + card.h - CARD_PAD - theme::size::CAPTION as f32,
            theme::size::CAPTION,
            theme::TEXT_TERTIARY,
            2,
            1,
        );

        self.draw_information(p, x, top + COL_Y, measure);
        self.draw_languages(p, top + COL_Y, focused == Some(LANGUAGES_ELEM), tracks, measure);
        self.draw_accessibility(p, 1360.0, top + COL_Y, measure);
    }

    fn draw_information(&self, p: Painter, x: f32, y: f32, measure: &dyn Measure) {
        text_at(
            p,
            x,
            y,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            1,
            crate::i18n::msg::browse_detail_information(),
        );
        let mut yy = y + 68.0;
        for (label, value) in &self.info {
            yy += draw_pair(p, x, yy, label, value, measure);
        }
    }

    fn draw_languages(&self, p: Painter, y: f32, focused: bool, tracks: bool, measure: &dyn Measure) {
        if focused {
            crate::ui::widgets::text_block_highlight(p, self.languages_rect(y - COL_Y, measure));
        }
        text_at(
            p,
            LANG_X,
            y,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            1,
            crate::i18n::msg::browse_detail_languages(),
        );
        let mut yy = y + 68.0;
        if let Some(orig) = &self.orig_audio {
            yy += draw_pair(p, LANG_X, yy, crate::i18n::msg::browse_detail_original_audio(), orig, measure);
        }
        if !self.audio_list.is_empty() {
            text_at(
                p,
                LANG_X,
                yy,
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
                0,
                crate::i18n::msg::browse_detail_audio(),
            );
            audio_view(&self.audio_list, measure)
                .draw(p, Rect::new(LANG_X, yy + 34.0, LANG_W, 0.0));
        }
        if tracks {
            let plate = self.languages_rect(y - COL_Y, measure);
            p.text(
                crate::ui::text_view::more_mark().as_ptr(),
                plate.x + plate.w - CARD_PAD,
                languages_more_y(plate),
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
                2,
                1,
            );
        }
    }

    fn draw_accessibility(
        &self,
        p: Painter,
        x: f32,
        y: f32,
        measure: &dyn crate::ui::machine::Measure,
    ) {
        text_at(
            p,
            x,
            y,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            1,
            crate::i18n::msg::browse_detail_accessibility(),
        );
        if self.access.is_empty() {
            text_at(
                p,
                x,
                y + 68.0,
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
                0,
                "\u{2014}",
            );
            return;
        }
        let mut yy = y + 64.0;
        for (label, desc) in &self.access {
            crate::ui::widgets::badge(
                p,
                x,
                yy + crate::ui::widgets::BADGE_H * 0.5,
                label,
                None,
                crate::ui::widgets::BadgeStyle::Filled,
                measure,
            );
            let h = TextView::new(desc, theme::size::CAPTION, theme::TEXT_HEADING)
                .with_measure(measure)
                .leading(30.0)
                .max_lines(4)
                .draw(p, Rect::new(x, yy + 52.0, 500.0, 0.0));
            yy += 52.0 + h + 26.0;
        }
    }
}

fn pair_h(value: &str, measure: &dyn Measure) -> f32 {
    34.0 + TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING)
        .bold()
        .with_measure(measure)
        .leading(30.0)
        .max_lines(2)
        .measure_h(520.0)
        .max(30.0)
        + 22.0
}

fn draw_pair(p: Painter, x: f32, y: f32, label: &str, value: &str, measure: &dyn Measure) -> f32 {
    text_at(
        p,
        x,
        y,
        theme::size::CAPTION,
        theme::TEXT_TERTIARY,
        0,
        label,
    );
    let h = TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING)
        .bold()
        .with_measure(measure)
        .leading(30.0)
        .max_lines(2)
        .draw(p, Rect::new(x, y + 34.0, 520.0, 0.0));
    34.0 + h.max(30.0) + 22.0
}

fn text_at(p: Painter, x: f32, y: f32, size: i32, color: [f32; 4], bold: i32, text: &str) -> f32 {
    CString::new(text)
        .ok()
        .map(|s| p.text(s.as_ptr(), x, y, size, color, 0, bold))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue 15: the About card's MORE is pinned on the synopsis' last line whether or not the
    /// text was truncated, so a synopsis that fits in exactly its five lines with a long last
    /// line runs under MORE unless that line fades too.
    #[test]
    fn an_untruncated_last_line_reaching_under_more_fades() {
        let measure = crate::ui::fixture::FixtureMeasure;
        let fits = |n: usize| {
            let text = "word ".repeat(n);
            (!synopsis(&text, &measure).truncates(SYNOPSIS_W)).then_some(text)
        };
        let longest = (1..2000).map_while(fits).last().expect("some synopsis fits");
        let view = synopsis(&longest, &measure);
        let mark = measure.width(crate::ui::text_view::more_mark(), theme::size::CAPTION, true);
        assert!(!view.truncates(SYNOPSIS_W));
        assert!(view.last_line_w(SYNOPSIS_W) > SYNOPSIS_W - (mark + MORE_GAP),
            "precondition: the last line ends inside MORE's rect and its air");
        assert!(view.last_line_fades(SYNOPSIS_W), "text running under MORE must fade");
        let short = synopsis("A short synopsis.", &measure);
        assert!(!short.last_line_fades(SYNOPSIS_W), "a line clear of MORE stays plain");
    }

    /// The Languages column's MORE is not the About card's shape: it sits on its own row below the
    /// audio list, so even a complete list whose last line runs the full measure never reaches
    /// under it — and that line must NOT fade.
    #[test]
    fn the_languages_more_sits_below_the_audio_list_so_a_complete_last_line_stays_plain() {
        let measure = crate::ui::fixture::FixtureMeasure;
        let fits = |n: usize| {
            let text = "English (AAC), ".repeat(n);
            (!audio_view(&text, &measure).truncates(LANG_W)).then_some(text)
        };
        let longest = (1..200).map_while(fits).last().expect("some list fits");
        let mut rows = Rows::new();
        rows.audio_list = longest.clone();
        let plate = rows.languages_rect(0.0, &measure);
        let view = audio_view(&longest, &measure);
        let list_top = COL_Y + 68.0 + 34.0;
        let list_h = view.measure_h(LANG_W);
        let last_line_bottom = view.last_line_cap_y(list_top, list_h) + view.line_h();
        assert!(languages_more_y(plate) >= last_line_bottom,
            "MORE ({}) is below the last audio line ({last_line_bottom})", languages_more_y(plate));
        assert!(!view.last_line_fades(LANG_W), "a complete list never fades");
    }

    #[test]
    fn only_the_card_and_available_languages_are_focusable() {
        assert_eq!(locate(CARD_ELEM, false), Some(0));
        assert_eq!(locate(LANGUAGES_ELEM, false), None);
        assert_eq!(locate(LANGUAGES_ELEM, true), Some(1));
        assert_eq!(locate(ABOUT_ELEM_RANGE_START + 2, true), None);
    }

    #[test]
    fn about_focus_only_ever_lands_on_the_card_and_a_clickable_languages_column() {
        let without_tracks: Vec<_> = (ABOUT_ELEM_RANGE_START..ABOUT_ELEM_RANGE_END)
            .filter_map(|key| locate(key, false).map(|_| key))
            .collect();
        let with_tracks: Vec<_> = (ABOUT_ELEM_RANGE_START..ABOUT_ELEM_RANGE_END)
            .filter_map(|key| locate(key, true).map(|_| key))
            .collect();
        assert_eq!(without_tracks, vec![CARD_ELEM]);
        assert_eq!(with_tracks, vec![CARD_ELEM, LANGUAGES_ELEM]);
    }

    #[test]
    fn a_same_identity_metadata_landing_invalidates_cached_about_rows() {
        let mut rows = Rows::new();
        let first = Detail {
            sid: crate::plex::ServerId::UNSET,
            rk: "movie".into(),
            rating: "PG".into(),
            dur_ms: 60_000,
            ..Default::default()
        };
        rows.update(&first);
        assert!(rows
            .info
            .iter()
            .any(|(label, value)| *label == "Rated" && value == "PG"));

        let second = Detail {
            rating: "R".into(),
            dur_ms: 120_000,
            ..first
        };
        rows.update(&second);
        assert!(
            rows.info
                .iter()
                .any(|(label, value)| *label == "Rated" && value == "PG"),
            "without a landing invalidation, repeated reads remain O(1)"
        );
        rows.invalidate();
        rows.update(&second);
        assert!(rows
            .info
            .iter()
            .any(|(label, value)| *label == "Rated" && value == "R"));
        assert!(rows
            .info
            .iter()
            .any(|(label, value)| *label == "Run Time" && value == "2 min"));
    }
}
