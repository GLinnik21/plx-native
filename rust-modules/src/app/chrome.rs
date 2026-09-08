//! Application-owned data for the shared bar. Capture before stepping a frame; paint consumes
//! borrowed strings and never opens the session file or polls the Browse vocabulary.

use crate::ui::containers::tabs::StripMember;
use crate::ui::dispatch::STRIP_BASE;
use crate::ui::machine::{FocusKey, Measure};
use crate::ui::widgets::{self, ProfileChipRead, TabLabels, TopFocus};

#[derive(Default)]
pub(super) struct ChromeSnapshot {
    tabs_generation: Option<u32>,
    profile_generation: Option<u32>,
    labels: Vec<String>,
    keys: Vec<u32>,
    widths: Vec<f32>,
    thumb: String,
    label: String,
    initial: String,
}

impl ChromeSnapshot {
    pub(super) fn refresh(&mut self, measure: &dyn Measure) {
        let generation = crate::browse::tabs_gen();
        if self.tabs_generation != Some(generation) {
            self.labels.clear();
            self.keys.clear();
            self.labels.push("Home".into());
            self.keys.push(STRIP_BASE);
            for i in 0..crate::browse::tab_count() {
                let Some(kind) = crate::browse::tab_kind(i) else { continue };
                self.labels.push(crate::browse::tab_title(i).into());
                self.keys.push(STRIP_BASE + match kind {
                    crate::browse::SecKind::Movie => 1,
                    crate::browse::SecKind::Show => 2,
                });
            }
            self.labels.push(String::new());
            self.keys.push(STRIP_BASE + 3);
            self.widths = widgets::tab_widths(&self.labels, measure);
            self.tabs_generation = Some(generation);
        }
        let generation = crate::plex::session::current_gen();
        if self.profile_generation != Some(generation) {
            let current = crate::plex::session::current();
            let account = crate::plex::session::peek().account(current.as_ref());
            self.thumb = current.map(|user| user.thumb).unwrap_or_default();
            self.label = crate::ui::account_menu::chip_label(&account);
            self.initial = account.name.as_deref().and_then(|name| name.chars().next())
                .map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            self.profile_generation = Some(generation);
        }
    }

    pub(super) fn labels(&self) -> TabLabels<'_> {
        TabLabels { generation: self.tabs_generation.unwrap_or(0), labels: &self.labels }
    }

    pub(super) fn profile(&self) -> ProfileChipRead<'_> {
        ProfileChipRead { generation: self.profile_generation.unwrap_or(0), thumb: &self.thumb,
            label: &self.label, initial: &self.initial }
    }

    pub(super) fn focus(&self, focus: Option<FocusKey<u32>>) -> TopFocus {
        match focus.map(|focus| focus.elem) {
            Some(key) if key == STRIP_BASE + 4 => TopFocus::Chip,
            Some(key) => self.keys.iter().position(|&id| id == key).map(TopFocus::Pill).unwrap_or(TopFocus::Away),
            None => TopFocus::Away,
        }
    }

    pub(super) fn members(&self, selected: i32, focus: Option<FocusKey<u32>>, out: &mut Vec<StripMember<u32>>) {
        out.clear();
        out.push(StripMember::new(STRIP_BASE + 4, widgets::CHIP_FRAME));
        widgets::tab_members(&self.widths, &self.keys, selected, self.focus(focus), out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::machine::EntryId;

    #[test]
    fn published_bar_focus_uses_destination_identity_not_position() {
        let snapshot = ChromeSnapshot { labels: vec!["Home".into(), "TV Shows".into(), String::new()],
            keys: vec![STRIP_BASE, STRIP_BASE + 2, STRIP_BASE + 3], ..Default::default() };
        let at = |id| Some(FocusKey { entry: EntryId(1), elem: STRIP_BASE + id });
        assert_eq!(snapshot.focus(at(2)), TopFocus::Pill(1));
        assert_eq!(snapshot.focus(at(3)), TopFocus::Pill(2));
        assert_eq!(snapshot.focus(at(1)), TopFocus::Away);
        assert_eq!(snapshot.focus(at(4)), TopFocus::Chip);
        for elem in [0, 1] {
            assert_eq!(snapshot.focus(Some(FocusKey { entry: EntryId(1), elem })), TopFocus::Away);
        }
    }

    #[test]
    fn four_libraries_on_two_servers_publish_two_type_destinations() {
        let _guard = crate::testlock::serial();
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        let mut snapshot = ChromeSnapshot {
            profile_generation: Some(crate::plex::session::current_gen()), ..Default::default()
        };
        snapshot.refresh(&crate::ui::fixture::FixtureMeasure);
        assert_eq!(snapshot.keys, vec![STRIP_BASE, STRIP_BASE + 1, STRIP_BASE + 2, STRIP_BASE + 3]);
        assert_eq!(&snapshot.labels[..3], &["Home", "Movies", "TV Shows"]);
        let mut members = Vec::new();
        snapshot.members(0, None, &mut members);
        assert_eq!(members.iter().map(|member| member.elem).collect::<Vec<_>>(),
            vec![STRIP_BASE + 4, STRIP_BASE, STRIP_BASE + 1, STRIP_BASE + 2, STRIP_BASE + 3]);
        crate::browse::reset();
    }
}
