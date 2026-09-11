//! Application-owned data for the shared bar. Capture before stepping a frame; paint consumes
//! borrowed strings and never opens the session file or polls the Browse vocabulary.

use crate::ui::containers::tabs::StripMember;
use crate::ui::dispatch::STRIP_BASE;
use crate::ui::machine::{FocusKey, Measure};
use crate::ui::widgets::{self, ChromeRead, ProfileChipRead, TabLabels, TopFocus};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pill {
    Home,
    Section(crate::browse::SecKind),
    Search,
}

pub(crate) fn tab_count() -> usize {
    1 + crate::browse::tab_count() + 1
}

pub(crate) fn pill_at(i: usize) -> Pill {
    pill_in(i, tab_count() - 1, crate::browse::tab_kind)
}

fn pill_in(
    i: usize,
    search: usize,
    kind_at: impl Fn(usize) -> Option<crate::browse::SecKind>,
) -> Pill {
    if i == search {
        Pill::Search
    } else if let Some(section) = i.checked_sub(1) {
        kind_at(section).map(Pill::Section).unwrap_or(Pill::Home)
    } else {
        Pill::Home
    }
}

pub(crate) fn pill_of(pill: Pill) -> Option<usize> {
    let search = tab_count() - 1;
    pill_index(pill, search, crate::browse::tab_of_kind)
}

fn pill_index(
    pill: Pill,
    search: usize,
    tab_of: impl Fn(crate::browse::SecKind) -> Option<usize>,
) -> Option<usize> {
    match pill {
        Pill::Home => Some(0),
        Pill::Section(kind) => tab_of(kind).map(|i| i + 1),
        Pill::Search => Some(search),
    }
}

#[derive(Default)]
pub(crate) struct ChromeSnapshot {
    tabs_generation: Option<u32>,
    profile_generation: Option<u32>,
    labels: Vec<String>,
    keys: Vec<u32>,
    widths: Vec<f32>,
    thumb: String,
    initial: std::ffi::CString,
    name: std::ffi::CString,
    name_w: f32,
}

impl ChromeSnapshot {
    pub(crate) fn refresh(&mut self, measure: &dyn Measure) {
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
            let label = crate::screens::account_menu::chip_label(&account);
            let initial = account.name.as_deref().and_then(|name| name.chars().next())
                .map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            (self.initial, self.name, self.name_w) =
                widgets::profile_chip_text(&label, &initial, measure);
            self.profile_generation = Some(generation);
        }
    }

    pub(crate) fn labels(&self) -> TabLabels<'_> {
        TabLabels { generation: self.tabs_generation.unwrap_or(0), labels: &self.labels }
    }

    pub(crate) fn library_selection(&self, kind: crate::browse::SecKind) -> u32 {
        let elem = STRIP_BASE + match kind { crate::browse::SecKind::Movie => 1, crate::browse::SecKind::Show => 2 };
        self.keys.iter().position(|key| *key == elem).unwrap_or(0) as u32
    }

    pub(crate) fn search_selection(&self) -> u32 {
        self.keys.iter().position(|key| *key == STRIP_BASE + 3).unwrap_or(0) as u32
    }

    pub(crate) fn profile(&self) -> ProfileChipRead<'_> {
        ProfileChipRead { thumb: &self.thumb, initial: &self.initial, name: &self.name,
            name_w: self.name_w }
    }

    pub(crate) fn read(&self, chip_expand: f32) -> ChromeRead<'_> {
        ChromeRead { profile: self.profile(), labels: self.labels(), chip_expand }
    }

    pub(crate) fn focus(&self, focus: Option<FocusKey<u32>>) -> TopFocus {
        match focus.map(|focus| focus.elem) {
            Some(key) if key == STRIP_BASE + 4 => TopFocus::Chip,
            Some(key) => self.keys.iter().position(|&id| id == key).map(TopFocus::Pill).unwrap_or(TopFocus::Away),
            None => TopFocus::Away,
        }
    }

    /// `scroll` is the shared strip's current offset (`StripRender::scroll_pos`, owned by
    /// `app::bridge::Bridge`) — a parameter because this snapshot holds no `StripRender` of its
    /// own; the `Bridge` methods that call this are the one place both live.
    pub(crate) fn members(&self, selected: i32, focus: Option<FocusKey<u32>>, scroll: f32, out: &mut Vec<StripMember<u32>>) {
        out.clear();
        out.push(StripMember::new(STRIP_BASE + 4, widgets::CHIP_FRAME));
        widgets::tab_members(&self.widths, &self.keys, selected, self.focus(focus), scroll, out);
    }

    #[cfg(test)]
    pub(crate) fn seed_for_test(&mut self, name: &str, initial: &str, labels: &[&str], measure: &dyn Measure) {
        self.tabs_generation = Some(1);
        self.labels = labels.iter().map(|s| (*s).to_owned()).collect();
        self.keys = (0..self.labels.len()).map(|i| STRIP_BASE + i as u32).collect();
        self.widths = widgets::tab_widths(&self.labels, measure);
        self.profile_generation = Some(1);
        self.thumb.clear();
        (self.initial, self.name, self.name_w) =
            widgets::profile_chip_text(name, initial, measure);
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
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();
        let mut snapshot = ChromeSnapshot {
            profile_generation: Some(crate::plex::session::current_gen()), ..Default::default()
        };
        snapshot.refresh(&crate::ui::fixture::FixtureMeasure);
        assert_eq!(snapshot.keys, vec![STRIP_BASE, STRIP_BASE + 1, STRIP_BASE + 2, STRIP_BASE + 3]);
        assert_eq!(&snapshot.labels[..3], &["Home", "Movies", "TV Shows"]);
        let mut members = Vec::new();
        snapshot.members(0, None, 0.0, &mut members);
        assert_eq!(members.iter().map(|member| member.elem).collect::<Vec<_>>(),
            vec![STRIP_BASE + 4, STRIP_BASE, STRIP_BASE + 1, STRIP_BASE + 2, STRIP_BASE + 3]);
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    }

    #[test]
    fn every_projected_pill_round_trips_by_stable_section_identity() {
        use crate::browse::SecKind::{Movie, Show};
        let kinds = [Movie, Show];
        let at = |i| kinds.get(i).copied();
        let search = kinds.len() + 1;
        assert_eq!(pill_in(0, search, at), Pill::Home);
        assert_eq!(pill_in(1, search, at), Pill::Section(Movie));
        assert_eq!(pill_in(2, search, at), Pill::Section(Show));
        assert_eq!(pill_in(search, search, at), Pill::Search);
        assert_eq!(pill_in(search + 1, search, at), Pill::Home);
        let pos = |kind| kinds.iter().position(|&candidate| candidate == kind);
        for i in 0..=search {
            assert_eq!(pill_index(pill_in(i, search, at), search, pos), Some(i));
        }
        assert_eq!(pill_index(Pill::Section(Show), search, |_| None), None,
            "a type the captured strip no longer contains borrows no other position");
        assert_eq!(pill_in(1, 3, |_| None), Pill::Home,
            "an unfilled section slot falls back to the one fixed destination");
    }

    #[test]
    fn shared_widgets_read_no_live_application_vocabulary() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/widgets.rs"),
        ).expect("read widgets.rs");
        let live = src.lines().filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>().join("\n");
        for forbidden in ["crate::browse::", "crate::plex::session::", "crate::screens::"] {
            assert!(!live.contains(forbidden),
                "shared widgets must consume captured app projections, found {forbidden}");
        }
    }
}
