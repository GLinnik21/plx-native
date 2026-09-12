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
            self.info.push(("Released", released));
        }
        let dur = if d.dur_ms > 0 {
            d.dur_ms
        } else {
            d.episodes.first().map(|e| e.dur_ms).unwrap_or(0)
        };
        if dur > 0 {
            self.info.push(("Run Time", crate::ui::fmt::dur_long(dur)));
        }
        self.info.push((
            "Rated",
            if d.rating.is_empty() {
                "NR".into()
            } else {
                d.rating.clone()
            },
        ));
        if !d.countries.is_empty() {
            self.info
                .push(("Regions of Origin", d.countries.join(", ")));
        }
        self.orig_audio = d.audio.first().map(|a| {
            if a.lang.is_empty() {
                "Unknown".into()
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
                    "Unknown"
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
                "CC",
                "Closed captions refer to subtitles in available languages with the addition of relevant non-dialogue information.",
            ));
        }
        if d.subs.iter().any(|s| s.sdh) {
            self.access.push(("SDH", "Subtitles for the deaf and hard of hearing (SDH) refer to subtitles in the original language with the addition of relevant non-dialogue information."));
        }
        if d.audio.iter().any(|a| a.ad) {
            self.access.push((
                "AD",
                "Audio descriptions (AD) refer to a narration track describing what is happening on screen, to provide context for those who are blind or have low vision.",
            ));
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn card_rect(&self, d: &Detail, top: f32, measure: &dyn Measure) -> Rect {
        let width = CARD_W - 2.0 * CARD_PAD;
        let synopsis = TextView::new(&d.summary, theme::size::CAPTION, theme::TEXT_HEADING)
            .with_measure(measure)
            .leading(30.0)
            .max_lines(5);
        let h = synopsis.measure_h(width).max(30.0);
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
            h += 34.0
                + TextView::new(&self.audio_list, theme::size::LABEL, theme::TEXT_HEADING)
                    .with_measure(measure)
                    .leading(32.0)
                    .max_lines(6)
                    .measure_h(500.0);
        }
        Rect::new(LANG_X - 26.0, top + COL_Y - 36.0, 560.0, h + 60.0)
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
            c"About".as_ptr(),
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
        TextView::new(&d.summary, theme::size::CAPTION, theme::TEXT_HEADING)
            .with_measure(measure)
            .leading(30.0)
            .max_lines(5)
            .fade_last(90.0)
            .draw(
                p,
                Rect::new(ix, card.y + CARD_PAD + 100.0, card.w - 2.0 * CARD_PAD, 0.0),
            );
        p.text(
            c"MORE".as_ptr(),
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
            "Information",
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
            "Languages",
        );
        let mut yy = y + 68.0;
        if let Some(orig) = &self.orig_audio {
            yy += draw_pair(p, LANG_X, yy, "Original Audio", orig, measure);
        }
        if !self.audio_list.is_empty() {
            text_at(
                p,
                LANG_X,
                yy,
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
                0,
                "Audio",
            );
            TextView::new(&self.audio_list, theme::size::LABEL, theme::TEXT_HEADING)
                .with_measure(measure)
                .leading(32.0)
                .max_lines(6)
                .fade_last(90.0)
                .draw(p, Rect::new(LANG_X, yy + 34.0, 500.0, 0.0));
        }
        if tracks {
            p.text(
                c"MORE".as_ptr(),
                LANG_X + 500.0,
                self.languages_rect(y - COL_Y, measure).y + self.languages_rect(y - COL_Y, measure).h - 30.0,
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
            "Accessibility",
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
