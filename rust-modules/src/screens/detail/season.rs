//! Season-strip layout and delayed loading for [`super::DetailScreen`].

use std::hash::{Hash, Hasher};

use crate::metadata::Detail;
use crate::ui::machine::{GroupId, Measure};
use crate::ui::widgets::{self, SelMark, StripLay, TabGround, TabPill, TabStrip};
use crate::ui::{on_axis, theme, Env, Painter, Rect, View};

pub(crate) const SEASON_ELEM_RANGE_START: u32 = 64;
pub(crate) const SEASON_ELEM_RANGE_END: u32 = 128;
pub(crate) const SEASON_GROUP: GroupId = GroupId(1);
pub(crate) const ROW_H: f32 = crate::ui::widgets::StatusOverlay::CTRL_H;
pub(crate) const SETTLE_S: f32 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestoreStep {
    Wait,
    Request(usize),
    Ready,
    Retire,
}

pub(crate) fn restore_step(
    detail: Option<&Detail>,
    season_number: Option<i64>,
    requested: bool,
    loading: bool,
) -> RestoreStep {
    let Some(number) = season_number else {
        return RestoreStep::Ready;
    };
    let Some(detail) = detail else {
        return RestoreStep::Wait;
    };
    let Some(index) = detail
        .seasons
        .iter()
        .position(|season| season.index == number)
    else {
        return RestoreStep::Retire;
    };
    if detail.cur_season == index && !loading {
        RestoreStep::Ready
    } else if !requested {
        RestoreStep::Request(index)
    } else if loading {
        RestoreStep::Wait
    } else {
        RestoreStep::Retire
    }
}

pub(crate) fn elem(index: usize) -> Option<u32> {
    (index < (SEASON_ELEM_RANGE_END - SEASON_ELEM_RANGE_START) as usize)
        .then_some(SEASON_ELEM_RANGE_START + index as u32)
}

pub(crate) fn locate(key: u32) -> Option<usize> {
    (SEASON_ELEM_RANGE_START..SEASON_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - SEASON_ELEM_RANGE_START) as usize)
}

pub(crate) fn watch_state(season: &crate::metadata::Season) -> crate::ui::widgets::PosterMark {
    if season.watched() {
        crate::ui::widgets::PosterMark::Watched
    } else if season.viewed_leaf_count > 0 {
        crate::ui::widgets::PosterMark::InProgress
    } else {
        crate::ui::widgets::PosterMark::None
    }
}

pub(crate) struct Metrics {
    dirty: bool,
    fingerprint: u64,
    lays: Vec<StripLay>,
}

impl Metrics {
    pub(crate) fn new() -> Self {
        Self {
            dirty: true,
            fingerprint: 0,
            lays: Vec::new(),
        }
    }

    pub(crate) fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn update(&mut self, d: &Detail, measure: &dyn Measure) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        d.sid.raw().hash(&mut h);
        d.rk.hash(&mut h);
        for s in &d.seasons {
            s.index.hash(&mut h);
            s.title.hash(&mut h);
        }
        let fp = h.finish().max(1);
        if fp == self.fingerprint {
            return;
        }
        self.fingerprint = fp;
        self.lays = widgets::strip_layout_measured(
            d.seasons.iter().map(|season| {
                if season.title.is_empty() {
                    format!("Season {}", season.index)
                } else {
                    season.title.clone()
                }
            }),
            crate::ui::consts::MARGIN_X,
            theme::size::BODY,
            widgets::STRIP_GAP,
            measure,
        );
    }

    pub(crate) fn lays(&self) -> &[StripLay] {
        &self.lays
    }

    pub(crate) fn rect(&self, index: usize, top: f32, scroll: f32) -> Option<Rect> {
        self.lays.get(index).map(|lay| {
            let r = widgets::strip_pill_rect(lay, top, ROW_H);
            Rect::new(r.x - scroll, r.y, r.w, r.h)
        })
    }

    pub(crate) fn scroll_target(&self, current: f32, index: usize) -> f32 {
        let Some(r) = self.rect(index, 0.0, 0.0) else {
            return current;
        };
        let lo = r.x + r.w + widgets::STRIP_ADVANCE
            - (crate::ui::consts::SCR_W - crate::ui::consts::MARGIN_X);
        let hi = r.x - widgets::STRIP_ADVANCE - crate::ui::consts::MARGIN_X;
        crate::ui::card_row::reveal(current, lo, hi, f32::MAX)
    }
}

pub(crate) fn draw(
    p: Painter,
    metrics: &Metrics,
    tabs: TabStrip,
    selected: usize,
    focused: Option<usize>,
    scroll: f32,
    pop: f32,
) {
    let p = p.translate(-scroll, 0.0);
    tabs.draw(p, 0.0, ROW_H, TabGround::Plated { pop });
    let env = Env::inert();
    for lay in metrics.lays() {
        let pill = widgets::strip_pill_rect(lay, 0.0, ROW_H);
        if !on_axis(pill.x - scroll, pill.w, crate::ui::consts::SCR_W, 0.0) {
            continue;
        }
        let (fm, sm) = tabs.mixes((pill.x, pill.w));
        TabPill::new(lay.label.as_ptr(), theme::size::BODY, pill)
            .plated()
            .mix(fm, sm)
            .draw(&env, p);
    }
    let _ = (selected, focused);
}

pub(crate) fn update_tabs(
    tabs: &mut TabStrip,
    metrics: &Metrics,
    selected: Option<usize>,
    focused: Option<usize>,
    dt: f32,
) {
    tabs.update(
        selected.map(|i| i as i32).unwrap_or(-1),
        focused.map(|i| i as i32).unwrap_or(-1),
        |i| widgets::strip_span(metrics.lays(), i, ROW_H),
        SelMark::Lands,
        dt,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_frozen_season_keys_round_trip() {
        for i in 0..64 {
            assert_eq!(locate(elem(i).unwrap()), Some(i));
        }
        assert_eq!(elem(64), None);
    }

    fn detail() -> Detail {
        Detail {
            seasons: vec![
                crate::metadata::Season {
                    rk: "s1".into(),
                    index: 1,
                    title: "Season 1".into(),
                    leaf_count: 1,
                    viewed_leaf_count: 0,
                },
                crate::metadata::Season {
                    rk: "s3".into(),
                    index: 3,
                    title: "Season 3".into(),
                    leaf_count: 1,
                    viewed_leaf_count: 0,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn the_latch_waits_for_the_show_then_asks_once_and_retires_on_the_season() {
        let d = detail();
        assert_eq!(restore_step(None, Some(3), false, false), RestoreStep::Wait);
        assert_eq!(
            restore_step(Some(&d), Some(3), false, false),
            RestoreStep::Request(1)
        );
        assert_eq!(
            restore_step(Some(&d), Some(3), true, true),
            RestoreStep::Wait
        );
        let mut landed = d;
        landed.cur_season = 1;
        assert_eq!(
            restore_step(Some(&landed), Some(3), true, false),
            RestoreStep::Ready
        );
    }

    #[test]
    fn a_failed_season_fetch_retires_the_latch_instead_of_asking_again() {
        assert_eq!(
            restore_step(Some(&detail()), Some(3), true, false),
            RestoreStep::Retire
        );
    }

    #[test]
    fn a_superseding_open_and_a_missing_season_both_retire_it() {
        assert_eq!(
            restore_step(Some(&detail()), Some(99), false, false),
            RestoreStep::Retire
        );
        assert_eq!(
            restore_step(Some(&detail()), None, false, false),
            RestoreStep::Ready
        );
    }

    #[test]
    fn season_tab_pills_cover_their_note_and_never_overlap() {
        let d = detail();
        let mut metrics = Metrics::new();
        metrics.update(&d, &crate::ui::fixture::FixtureMeasure);
        for pair in metrics.lays().windows(2) {
            let left = widgets::strip_pill_rect(&pair[0], 0.0, ROW_H);
            let right = widgets::strip_pill_rect(&pair[1], 0.0, ROW_H);
            assert!(left.x + left.w + widgets::STRIP_GAP <= right.x + 0.01);
        }
    }

    #[test]
    fn season_watch_state_uses_both_container_endpoints() {
        let mut season = detail().seasons.remove(0);
        season.leaf_count = 2;
        assert_eq!(watch_state(&season), crate::ui::widgets::PosterMark::None);
        season.viewed_leaf_count = 1;
        assert_eq!(
            watch_state(&season),
            crate::ui::widgets::PosterMark::InProgress
        );
        season.viewed_leaf_count = season.leaf_count;
        assert_eq!(
            watch_state(&season),
            crate::ui::widgets::PosterMark::Watched
        );
    }

    #[test]
    fn season_metrics_rebuild_once_after_a_same_identity_landing() {
        let measure = crate::ui::fixture::FixtureMeasure;
        let mut metrics = Metrics::new();
        let mut d = detail();
        metrics.update(&d, &measure);
        let before = metrics.lays()[0].label.to_str().unwrap().to_string();
        d.seasons[0].title = "Specials".into();
        metrics.update(&d, &measure);
        assert_eq!(metrics.lays()[0].label.to_str().unwrap(), before);
        metrics.invalidate();
        metrics.update(&d, &measure);
        assert_eq!(metrics.lays()[0].label.to_str().unwrap(), "Specials");
    }
}
