//! Application-owned data for the shared bar. Capture before stepping a frame; paint consumes
//! borrowed strings and never opens the session file or polls the Browse vocabulary.

use plx_ui::containers::tabs::StripMember;
use plx_machine::machine::{FocusKey, Measure};
use plx_ui::icons::Icon;
use plx_ui::widgets::{self, ChromeRead, ProfileChipRead, TabFace, TabLabels, TopFocus};
use plx_data::stores::browse::{DirectoryView, SecKind};
use plx_screens::registry::HomeTab;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pill {
    Home,
    Section(SecKind),
    Watchlist,
    Search,
}

impl Pill {
    /// The destination this pill opens: the one map to [`HomeTab`] (and so to its strip key).
    pub(crate) fn tab(self) -> HomeTab {
        match self {
            Pill::Home => HomeTab::Home,
            Pill::Section(kind) => HomeTab::of_kind(kind),
            Pill::Watchlist => HomeTab::Watchlist,
            Pill::Search => HomeTab::Search,
        }
    }

    /// The pill of a destination. The inverse of [`tab`](Self::tab).
    fn of_tab(tab: HomeTab) -> Pill {
        match (tab, tab.section()) {
            (_, Some(kind)) => Pill::Section(kind),
            (HomeTab::Watchlist, None) => Pill::Watchlist,
            (HomeTab::Search, None) => Pill::Search,
            (_, None) => Pill::Home,
        }
    }
}

/// The strip's destination keys in strip order: Home, one key per type pill, the Watchlist
/// bookmark, Search. The one
/// place that order is written; `refresh` publishes it and the pill maps below read it, so no
/// map assumes which index Search holds.
fn strip_keys(directory: DirectoryView<'_>) -> Vec<u32> {
    let mut keys = vec![widgets::TAB_KEY_HOME];
    keys.extend((0..directory.tab_count()).filter_map(|i| directory.tab_kind(i))
        .map(|kind| HomeTab::of_kind(kind).strip_key()));
    keys.push(widgets::TAB_KEY_WATCHLIST);
    keys.push(widgets::TAB_KEY_SEARCH);
    keys
}

pub(crate) fn pill_at(directory: DirectoryView<'_>, i: usize) -> Pill {
    pill_in(i, &strip_keys(directory))
}

/// The destination a strip index names, by the KEY published at that index.
fn pill_in(i: usize, keys: &[u32]) -> Pill {
    keys.get(i).copied().and_then(HomeTab::from_strip_key).map_or(Pill::Home, Pill::of_tab)
}

pub(crate) fn pill_of(directory: DirectoryView<'_>, pill: Pill) -> Option<usize> {
    pill_index(pill, &strip_keys(directory))
}

fn pill_index(pill: Pill, keys: &[u32]) -> Option<usize> {
    let key = pill.tab().strip_key();
    keys.iter().position(|&k| k == key)
}

#[derive(Default)]
pub(crate) struct ChromeSnapshot {
    tabs_generation: Option<u32>,
    profile_generation: Option<u32>,
    session_watch: plx_plex::plex::session::VisibleSessionWatch,
    faces: Vec<TabFace>,
    pub(crate) keys: Vec<u32>,
    widths: Vec<f32>,
    thumb: String,
    /// The profile's full chip label and initial, as captured: the chip's name is ELIDED from
    /// these to the room the strip leaves ([`widgets::chip_name_budget`]), so a change of the
    /// strip's widths re-elides without re-reading the session.
    label: String,
    initial_text: String,
    initial: std::ffi::CString,
    name: std::ffi::CString,
    name_w: f32,
    /// A pulse asked for and not yet handed to the strip's renderer: the destination key.
    pulse: Option<u32>,
}

impl ChromeSnapshot {
    /// **Pulse the strip destination `key`** (~1.35x scale and a glow, decorative): the Watchlist
    /// mark after a title was put on the list. The APPLICATION asks, never a screen; the bridge
    /// hands it to the strip's renderer on its next chrome update. A frame is requested so an
    /// idle app wakes for it.
    pub(crate) fn pulse(&mut self, key: u32) {
        self.pulse = Some(key);
        plx_machine::idle::invalidate();
    }

    /// The pending pulse as the strip index of its destination; `None` when nothing is pending or
    /// the destination is not on the strip (a Home with no Watchlist mark). Consumes it.
    pub(crate) fn take_pulse(&mut self) -> Option<usize> {
        let key = self.pulse.take()?;
        self.keys.iter().position(|k| *k == key)
    }

    /// Forget a pending pulse: the bar is not on screen, so it would play late on the next page.
    pub(crate) fn drop_pulse(&mut self) {
        self.pulse = None;
    }

    pub(crate) fn refresh(&mut self, measure: &dyn Measure, directory: DirectoryView<'_>) {
        self.refresh_with_profile(measure, directory, None);
    }

    pub(crate) fn refresh_with_profile(&mut self, measure: &dyn Measure, directory: DirectoryView<'_>,
        captured: Option<(&plx_plex::plex::session::CurrentProfile, &plx_plex::plex::session::Session)>) {
        let generation = directory.tabs_gen();
        let strip_changed = self.tabs_generation != Some(generation);
        if strip_changed {
            self.keys = strip_keys(directory);
            self.faces = self.keys.iter().map(|&key| match key {
                widgets::TAB_KEY_HOME => TabFace::Label(plx_platform::i18n::msg::browse_chrome_home().into()),
                widgets::TAB_KEY_MOVIES => TabFace::Label(plx_platform::i18n::msg::browse_kind_movies().into()),
                widgets::TAB_KEY_SHOWS => TabFace::Label(plx_platform::i18n::msg::browse_kind_tv_shows().into()),
                widgets::TAB_KEY_VIDEOS => TabFace::Label(plx_platform::i18n::msg::browse_kind_other_videos().into()),
                widgets::TAB_KEY_WATCHLIST => TabFace::Icon(Icon::Bookmark),
                _ => TabFace::Icon(Icon::Search),
            }).collect();
            // The Other Videos pill names its library when it is the only one; the translated type
            // word is already in the face, and stays when the title cannot be used.
            if let (Some(at), Some(title)) = (
                self.keys.iter().position(|&key| key == widgets::TAB_KEY_VIDEOS),
                directory.tab_library_title(SecKind::Video),
            ) {
                widgets::fit_title_face(&mut self.faces, at, title, measure);
            }
            self.widths = widgets::tab_widths(&self.faces, measure);
            self.tabs_generation = Some(generation);
        }
        let generation = captured.map_or_else(plx_plex::plex::session::current_gen, |(profile, _)| profile.generation);
        let session_changed = captured.is_none() && self.session_watch.changed();
        let mut profile_changed = false;
        if self.profile_generation != Some(generation) || session_changed {
            let current = captured.map_or_else(plx_plex::plex::session::current, |(profile, _)| profile.user.clone());
            let account = if let Some((_, saved)) = captured { saved.account(current.as_ref()) }
                else { plx_plex::plex::session::peek().account(current.as_ref()) };
            self.thumb = current.map(|user| user.thumb).unwrap_or_default();
            self.label = plx_screens::account_menu::chip_label(&account);
            self.initial_text = account.name.as_deref().and_then(|name| name.chars().next())
                .map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            self.profile_generation = Some(generation);
            profile_changed = true;
        }
        if profile_changed || strip_changed {
            self.elide_chip_name(measure);
        }
    }

    /// Elide the chip's name to the room the strip's CURRENT widths leave it. Called when the
    /// profile changes and when the strip does (a library kind appearing or leaving changes the
    /// track), because the budget is a function of both and drawing measures nothing.
    fn elide_chip_name(&mut self, measure: &dyn Measure) {
        let budget = widgets::chip_name_budget(widgets::tab_track_w(&self.widths));
        (self.initial, self.name, self.name_w) =
            widgets::profile_chip_text(&self.label, &self.initial_text, budget, measure);
    }

    pub(crate) fn labels(&self) -> TabLabels<'_> {
        TabLabels { generation: self.tabs_generation.unwrap_or(0), faces: &self.faces }
    }

    /// The strip key published at `selected` (an index into [`Self::keys`], as `members` takes it):
    /// the page's own pill, for the landing rule `TabContainer::strip_selected` states.
    pub(crate) fn key_at(&self, selected: i32) -> Option<u32> {
        self.keys.get(selected.max(0) as usize).copied()
    }

    pub(crate) fn profile(&self) -> ProfileChipRead<'_> {
        // The avatar is drawn against the BROWSED server, which is read where the chrome is
        // published: the library does not ask which server is current.
        ProfileChipRead { src: plx_plex::plex::current_server().raw(), thumb: &self.thumb,
            initial: &self.initial, name: &self.name, name_w: self.name_w }
    }

    pub(crate) fn read(&self, chip_expand: f32) -> ChromeRead<'_> {
        ChromeRead { profile: self.profile(), labels: self.labels(), chip_expand }
    }

    pub(crate) fn focus(&self, focus: Option<FocusKey<u32>>) -> TopFocus {
        match focus.map(|focus| focus.elem) {
            Some(key) if key == widgets::TAB_KEY_CHIP => TopFocus::Chip,
            Some(key) => self.keys.iter().position(|&id| id == key).map(TopFocus::Pill).unwrap_or(TopFocus::Away),
            None => TopFocus::Away,
        }
    }

    /// `scroll` is the shared strip's current offset (`StripRender::scroll_pos`, owned by
    /// `app::bridge::Bridge`) — a parameter because this snapshot holds no `StripRender` of its
    /// own; the `Bridge` methods that call this are the one place both live.
    pub(crate) fn members(&self, selected: i32, focus: Option<FocusKey<u32>>, scroll: f32, out: &mut Vec<StripMember<u32>>) {
        out.clear();
        out.push(StripMember::new(widgets::TAB_KEY_CHIP, widgets::CHIP_FRAME));
        widgets::tab_members(&self.widths, &self.keys, selected, self.focus(focus), scroll, out);
    }

    #[cfg(test)]
    pub(crate) fn seed_for_test(&mut self, name: &str, initial: &str, labels: &[&str], measure: &dyn Measure) {
        self.tabs_generation = Some(1);
        // An empty label is the Search slot: the production strip ends in the Search icon face.
        self.faces = labels.iter()
            .map(|s| if s.is_empty() { TabFace::Icon(Icon::Search) } else { TabFace::Label((*s).to_owned()) })
            .collect();
        self.keys = (0..self.faces.len()).map(|i| plx_ui::dispatch::STRIP_BASE + i as u32).collect();
        self.widths = widgets::tab_widths(&self.faces, measure);
        self.profile_generation = Some(1);
        self.thumb.clear();
        self.label = name.to_owned();
        self.initial_text = initial.to_owned();
        self.elide_chip_name(measure);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::stores::browse::{DirectorySnapshot, SectionView, SrcRow};
    use plx_machine::machine::EntryId;

    fn directory(kinds: &[SecKind]) -> DirectorySnapshot {
        DirectorySnapshot::fixture(7, 0, kinds.iter().enumerate().map(|(i, &kind)| SectionView {
            sid: Some(plx_plex::plex::ServerId::from_raw((i / 2) as u16)),
            key: i as i64 + 1,
            kind,
            row: SrcRow { section: i, title: format!("Library {i}"), pinned: true,
                ..Default::default() },
        }).collect())
    }

    #[test]
    fn session_refresh_rebuilds_the_profile_chip_without_a_profile_switch() {
        let _serial = plx_base::testlock::serial();
        let _session = plx_plex::plex::session::TempSession::new("chrome-session-refresh");
        let mut snapshot = ChromeSnapshot::default();
        let directory = directory(&[]);
        plx_plex::plex::session::install_transient_for_test(true);
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, directory.view());
        assert_eq!(snapshot.name.to_str().unwrap(), "Sign in");
        plx_plex::plex::session::save(&plx_plex::plex::session::Session {
            client_id: "synthetic-client".into(), account_token: "synthetic-token".into(),
            home_users: vec![plx_plex::plex::session::HomeUserRef {
                title: "Synthetic owner".into(), admin: true, ..Default::default()
            }], ..Default::default()
        });
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, directory.view());
        assert_eq!(snapshot.name.to_str().unwrap(), "Synthetic owner");
    }

    /// A font (or a language) that lengthens the pills, standing in for one that fills the bar.
    struct Wide;
    impl Measure for Wide {
        fn width(&self, s: &std::ffi::CStr, sz: i32, _bold: bool) -> f32 {
            s.to_bytes().len() as f32 * sz as f32 * 3.0
        }
        fn cap_h(&self, sz: i32) -> f32 { sz as f32 * 0.7 }
        fn line_h(&self, sz: i32) -> f32 { sz as f32 * 1.2 }
    }

    /// The chip's name is elided to the room the strip leaves it, so a change of the STRIP (a
    /// library kind appearing or leaving) re-elides it with the profile untouched: the owner
    /// holds the full label and re-asks `chip_name_budget` whenever `tabs_generation` moves, and
    /// drawing measures nothing. A bar too wide to leave the name any room shows none.
    #[test]
    fn the_chip_name_is_re_elided_when_only_the_strip_changes() {
        let _serial = plx_base::testlock::serial();
        let directory = directory(&[SecKind::Movie, SecKind::Show]);
        let mut snapshot = ChromeSnapshot::default();
        // the first refresh captures whatever profile is seated; the label is then a long one, as
        // a profile with a long name would have captured, and only the STRIP moves from here on
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, directory.view());
        snapshot.label = "A profile name long enough to overflow any budget the bar could leave it".into();
        snapshot.tabs_generation = None;
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, directory.view());
        let roomy = snapshot.name_w;
        let budget = widgets::chip_name_budget(widgets::tab_track_w(&snapshot.widths));
        assert!(budget > 0.0, "the ordinary strip leaves the chip a name");
        assert!(roomy > 0.0 && roomy <= budget, "name {roomy} against a budget of {budget}");
        assert!(snapshot.name.to_str().unwrap().ends_with('\u{2026}'), "a long name is elided");

        // the strip is rebuilt with wider pills; the profile generation does not move
        snapshot.tabs_generation = None;
        snapshot.refresh(&Wide, directory.view());
        assert!(widgets::tab_track_w(&snapshot.widths) > 1500.0, "the fixture strip must fill the bar");
        assert_eq!(widgets::chip_name_budget(widgets::tab_track_w(&snapshot.widths)), 0.0);
        assert!(snapshot.name.as_bytes().is_empty() && snapshot.name_w == 0.0,
            "no room, no name: {:?} ({})", snapshot.name, snapshot.name_w);
        assert_eq!(widgets::chip_cap(1.0, snapshot.name_w), widgets::chip_cap(0.0, 0.0),
            "and the capsule stays the closed circle");

        // …and back: the name returns to the full budget of the ordinary strip
        snapshot.tabs_generation = None;
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, directory.view());
        assert_eq!(snapshot.name_w, roomy);
    }

    #[test]
    fn published_bar_focus_uses_destination_identity_not_position() {
        let snapshot = ChromeSnapshot { faces: vec![TabFace::Label("Home".into()),
                TabFace::Label("TV Shows".into()), TabFace::Icon(Icon::Search)],
            keys: vec![widgets::TAB_KEY_HOME, widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_SEARCH], ..Default::default() };
        let at = |elem| Some(FocusKey { entry: EntryId(1), elem });
        assert_eq!(snapshot.focus(at(widgets::TAB_KEY_SHOWS)), TopFocus::Pill(1));
        assert_eq!(snapshot.focus(at(widgets::TAB_KEY_SEARCH)), TopFocus::Pill(2));
        assert_eq!(snapshot.focus(at(widgets::TAB_KEY_MOVIES)), TopFocus::Away);
        assert_eq!(snapshot.focus(at(widgets::TAB_KEY_CHIP)), TopFocus::Chip);
        for elem in [0, 1] {
            assert_eq!(snapshot.focus(Some(FocusKey { entry: EntryId(1), elem })), TopFocus::Away);
        }
    }

    #[test]
    fn four_libraries_on_two_servers_publish_two_type_destinations() {
        let directory = directory(&[SecKind::Movie, SecKind::Show, SecKind::Movie, SecKind::Show]);
        let mut snapshot = ChromeSnapshot {
            profile_generation: Some(plx_plex::plex::session::current_gen()), ..Default::default()
        };
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, directory.view());
        assert_eq!(snapshot.keys, vec![widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES,
            widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
        let words: Vec<&str> = snapshot.faces.iter().filter_map(|face| match face {
            TabFace::Label(word) => Some(word.as_str()),
            TabFace::Icon(_) => None,
        }).collect();
        assert_eq!(&words[..3], &["Home", "Movies", "TV Shows"]);
        assert_eq!(snapshot.faces.last(), Some(&TabFace::Icon(Icon::Search)));
        assert_eq!(snapshot.faces[snapshot.faces.len() - 2], TabFace::Icon(Icon::Bookmark),
            "the Watchlist bookmark sits just before Search");
        let mut members = Vec::new();
        snapshot.members(0, None, 0.0, &mut members);
        assert_eq!(members.iter().map(|member| member.elem).collect::<Vec<_>>(),
            vec![widgets::TAB_KEY_CHIP, widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES,
                widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
    }

    /// **Three types are four type-and-home pills: Home, Movies, TV Shows, Other Videos**, then the
    /// bookmark and Search. A household without TV Shows keeps Other Videos after Movies.
    #[test]
    fn other_videos_is_the_last_type_pill_before_the_bookmark_and_search() {
        let all = directory(&[SecKind::Movie, SecKind::Show, SecKind::Video]);
        let mut snapshot = ChromeSnapshot {
            profile_generation: Some(plx_plex::plex::session::current_gen()), ..Default::default()
        };
        snapshot.refresh(&plx_ui::fixture::FixtureMeasure, all.view());
        assert_eq!(snapshot.keys, vec![widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES, widgets::TAB_KEY_SHOWS,
            widgets::TAB_KEY_VIDEOS, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
        let words: Vec<&str> = snapshot.faces.iter().filter_map(|face| match face {
            TabFace::Label(word) => Some(word.as_str()),
            TabFace::Icon(_) => None,
        }).collect();
        // the fixture's one Other Videos library is titled "Library 2", and the only one names its pill
        assert_eq!(words, ["Home", "Movies", "TV Shows", "Library 2"]);

        let no_shows = directory(&[SecKind::Movie, SecKind::Video]);
        assert_eq!(strip_keys(no_shows.view()), vec![widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES,
            widgets::TAB_KEY_VIDEOS, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
        let only_videos = directory(&[SecKind::Video]);
        assert_eq!(strip_keys(only_videos.view()), vec![widgets::TAB_KEY_HOME, widgets::TAB_KEY_VIDEOS,
            widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
    }

    /// A pill is an identity: Other Videos keeps its destination whichever pills sit before it, and
    /// vanishes (no borrowed position) when it is switched off.
    #[test]
    fn the_other_videos_pill_round_trips_whatever_sits_before_it() {
        let full = directory(&[SecKind::Movie, SecKind::Show, SecKind::Video]);
        let no_shows = directory(&[SecKind::Movie, SecKind::Video]);
        let none = directory(&[SecKind::Movie, SecKind::Show]);
        assert_eq!(pill_of(full.view(), Pill::Section(SecKind::Video)), Some(3));
        assert_eq!(pill_of(no_shows.view(), Pill::Section(SecKind::Video)), Some(2));
        assert_eq!(pill_of(none.view(), Pill::Section(SecKind::Video)), None);
        assert_eq!(pill_at(full.view(), 3), Pill::Section(SecKind::Video));
        assert_eq!(pill_at(no_shows.view(), 2), Pill::Section(SecKind::Video));
        assert_eq!(Pill::Section(SecKind::Video).tab(), HomeTab::Videos);
        for kind in plx_data::browse::TAB_KINDS {
            assert_eq!(Pill::Section(kind).tab(), HomeTab::of_kind(kind), "{kind:?}");
            assert_eq!(Pill::of_tab(HomeTab::of_kind(kind)), Pill::Section(kind), "{kind:?}");
        }
        for pill in [Pill::Home, Pill::Watchlist, Pill::Search] {
            assert_eq!(Pill::of_tab(pill.tab()), pill);
        }
    }

    #[test]
    fn every_projected_pill_round_trips_by_stable_section_identity() {
        use plx_data::stores::browse::SecKind::{Movie, Show};
        let keys = [widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES, widgets::TAB_KEY_SHOWS,
            widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH];
        assert_eq!(pill_in(0, &keys), Pill::Home);
        assert_eq!(pill_in(1, &keys), Pill::Section(Movie));
        assert_eq!(pill_in(2, &keys), Pill::Section(Show));
        assert_eq!(pill_in(3, &keys), Pill::Watchlist);
        assert_eq!(pill_in(4, &keys), Pill::Search);
        assert_eq!(pill_in(5, &keys), Pill::Home);
        for i in 0..keys.len() {
            assert_eq!(pill_index(pill_in(i, &keys), &keys), Some(i));
        }
        assert_eq!(pill_index(Pill::Section(Show), &[widgets::TAB_KEY_HOME, widgets::TAB_KEY_SEARCH]), None,
            "a type the captured strip no longer contains borrows no other position");
    }

    #[test]
    fn an_icon_pill_before_search_does_not_shift_the_other_destinations() {
        use plx_data::stores::browse::SecKind::{Movie, Show};
        // A further icon destination (a key none of the maps names) sits before Search.
        let extra = plx_ui::dispatch::STRIP_BASE + 9;
        let keys = [widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES, widgets::TAB_KEY_SHOWS, extra,
            widgets::TAB_KEY_SEARCH];
        assert_eq!(pill_in(3, &keys), Pill::Home, "an unknown key is not Search and not a section");
        assert_eq!(pill_in(4, &keys), Pill::Search);
        assert_eq!(pill_in(2, &keys), Pill::Section(Show));
        assert_eq!(pill_index(Pill::Search, &keys), Some(4));
        assert_eq!(pill_index(Pill::Section(Movie), &keys), Some(1));
    }

    #[test]
    fn pill_maps_follow_the_published_strip_keys() {
        let directory = directory(&[SecKind::Movie, SecKind::Show]);
        let view = directory.view();
        assert_eq!(strip_keys(view), vec![widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES,
            widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
        assert_eq!(pill_of(view, Pill::Watchlist), Some(3));
        assert_eq!(pill_of(view, Pill::Search), Some(4));
        assert_eq!(pill_at(view, 3), Pill::Watchlist);
        assert_eq!(pill_at(view, 4), Pill::Search);
    }

    /// **The Other Videos pill wears the library's own title** (#599 addendum): exactly one
    /// favourite Other Videos library names the pill after itself, and every other case keeps the
    /// translated type word. Widths are the shipped bold face's whole-pixel advances
    /// (`ShippedMeasure`), the model the strip's budget is derived from, not a television.
    mod other_videos_pill {
        use super::*;
        use plx_base::fontcov::advances::ShippedMeasure;
        use plx_platform::i18n::{language_on_this_thread_for_test, Preference, SHIPPED};

        /// A household's section table: kind, the server's title for it, whether it is a favourite.
        fn libraries(rows: &[(SecKind, &str, bool)]) -> DirectorySnapshot {
            DirectorySnapshot::fixture(7, 0, rows.iter().enumerate().map(|(i, &(kind, title, pinned))| SectionView {
                sid: Some(plx_plex::plex::ServerId::from_raw(0)),
                key: i as i64 + 1,
                kind,
                row: SrcRow { section: i, title: title.to_owned(), pinned, ..Default::default() },
            }).collect())
        }

        fn strip(directory: &DirectorySnapshot) -> ChromeSnapshot {
            let mut snapshot = ChromeSnapshot {
                profile_generation: Some(plx_plex::plex::session::current_gen()), ..Default::default()
            };
            snapshot.refresh(&ShippedMeasure, directory.view());
            snapshot
        }

        /// The text on the Other Videos pill; the test fails when the strip has none.
        fn video_label(snapshot: &ChromeSnapshot) -> String {
            let at = snapshot.keys.iter().position(|&key| key == widgets::TAB_KEY_VIDEOS)
                .expect("the household has an Other Videos pill");
            match &snapshot.faces[at] {
                TabFace::Label(text) => text.clone(),
                TabFace::Icon(icon) => panic!("the Other Videos pill is a label, not {icon:?}"),
            }
        }

        fn label_of(title: &str) -> String {
            video_label(&strip(&libraries(&[(SecKind::Movie, "Movies", true),
                (SecKind::Show, "TV Shows", true), (SecKind::Video, title, true)])))
        }

        fn text_w(text: &str) -> f32 {
            use plx_machine::machine::Measure;
            ShippedMeasure.width_str(text, plx_ui::theme::size::BODY, true)
        }

        #[test]
        fn one_other_videos_library_names_the_pill() {
            let snapshot = strip(&libraries(&[(SecKind::Movie, "Movies", true),
                (SecKind::Show, "TV Shows", true), (SecKind::Video, "Home Movies", true)]));
            assert_eq!(video_label(&snapshot), "Home Movies");
            let words: Vec<&str> = snapshot.faces.iter().filter_map(|face| match face {
                TabFace::Label(word) => Some(word.as_str()),
                TabFace::Icon(_) => None,
            }).collect();
            assert_eq!(words, ["Home", "Movies", "TV Shows", "Home Movies"],
                "only the Other Videos pill is renamed");
        }

        #[test]
        fn two_other_videos_libraries_keep_the_type_label() {
            let two = libraries(&[(SecKind::Movie, "Movies", true), (SecKind::Video, "Home Movies", true),
                (SecKind::Video, "Holiday Clips", true)]);
            assert_eq!(video_label(&strip(&two)), "Other Videos", "two libraries have no one name");
            let one_off = libraries(&[(SecKind::Movie, "Movies", true), (SecKind::Video, "Home Movies", true),
                (SecKind::Video, "Holiday Clips", false)]);
            assert_eq!(video_label(&strip(&one_off)), "Home Movies",
                "a library switched off in Favorite libraries is not one of the two");
            let other_off = libraries(&[(SecKind::Movie, "Movies", true), (SecKind::Video, "Home Movies", false),
                (SecKind::Video, "Holiday Clips", true)]);
            assert_eq!(video_label(&strip(&other_off)), "Holiday Clips", "the pill names the library it opens");
        }

        #[test]
        fn a_long_title_is_elided_to_the_room_the_strip_leaves() {
            let title = "Family holidays at the lake house with all of the cousins, 2019 to 2024";
            assert!(title.chars().count() > 60);
            let face = label_of(title);
            assert!(face.ends_with('\u{2026}'), "a title this long is cut and says so, not {face:?}");
            assert!(title.starts_with(face.trim_end_matches('\u{2026}')), "it is a prefix of the title: {face:?}");
            assert!(face.chars().count() > 8, "most of the room is used, not just a stub: {face:?}");
            let w = text_w(&face);
            assert!(w <= widgets::TAB_TITLE_MAX, "{face:?} is {w} px of text, over the {} px cap", widgets::TAB_TITLE_MAX);
            let snapshot = strip(&libraries(&[(SecKind::Movie, "Movies", true), (SecKind::Show, "TV Shows", true),
                (SecKind::Video, title, true)]));
            assert!(widgets::chip_name_room(widgets::tab_track_w(&snapshot.widths)) >= widgets::TAB_TITLE_ROOM,
                "the unfurled chip keeps its room for an ordinary name beside the strip");
        }

        #[test]
        fn an_empty_blank_or_control_only_title_falls_back() {
            for title in ["", "   ", "\n\t", "\u{0}", "\r\n \u{7}"] {
                assert_eq!(label_of(title), "Other Videos", "{title:?} names nothing");
            }
            assert_eq!(label_of("Home\u{0}Movies"), "Home Movies", "a NUL inside a title is a space");
            assert_eq!(label_of("  Home \t\n  Movies  "), "Home Movies", "runs of white space are one space");
        }

        #[test]
        fn format_characters_and_controls_are_cleaned() {
            assert_eq!(label_of("Home\u{200B}Movies"), "HomeMovies", "a zero-width space draws nothing");
            assert_eq!(label_of("\u{202E}Clips\u{202C}"), "Clips", "bidi controls are dropped");
            assert_eq!(label_of("\u{FEFF}Clips\u{2066}\u{2069}"), "Clips", "a byte-order mark and isolates too");
            assert_eq!(label_of("Home\u{AD}Movies"), "HomeMovies", "a soft hyphen draws nothing");
            assert_eq!(label_of("\u{200B}\u{202E}"), "Other Videos", "nothing visible is nothing");
        }

        #[test]
        fn a_cjk_or_emoji_title_does_not_panic_and_fits_or_ends_in_an_ellipsis() {
            // The host measure prices only the Latin face (a CJK glyph costs the notdef width), so
            // this grades the contract and not the pixels: some of the title is used, and the
            // answer is whole characters that either fit or end in the ellipsis.
            for title in ["家族のビデオ", "家族のビデオ家族のビデオ家族のビデオ家族のビデオ家族のビデオ家族のビデオ家族のビデオ",
                "Clips \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} \u{1F1E9}\u{1F1EA} holiday", "e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}"] {
                let face = label_of(title);
                let first = title.chars().next().unwrap();
                assert!(face.starts_with(first), "{title:?} names the pill, not {face:?}");
                assert!(face.ends_with('\u{2026}') || text_w(&face) <= 240.0,
                    "{face:?} neither fits nor says it was cut");
            }
        }

        #[test]
        fn a_title_equal_to_another_pill_falls_back() {
            for title in ["movies", "MOVIES", "TV Shows", "tv shows", "Home", "hOME"] {
                assert_eq!(label_of(title), "Other Videos", "{title:?} would make two pills read alike");
            }
            assert_eq!(label_of("Movies 2"), "Movies 2", "only an equal label collides");
            assert_eq!(label_of("Homes"), "Homes");
        }

        #[test]
        fn the_collision_test_uses_unicode_lowercase() {
            let _ru = language_on_this_thread_for_test(Preference::Ru);
            let movies = plx_platform::i18n::msg::browse_kind_movies();
            assert_ne!(movies.to_uppercase(), movies, "a cased script");
            assert_eq!(label_of(&movies.to_uppercase()), plx_platform::i18n::msg::browse_kind_other_videos(),
                "{movies:?} in capitals is the same word, and the same pill twice");
            assert_eq!(label_of(&format!("{movies} 2")), format!("{movies} 2"), "a different word is used");
        }

        #[test]
        fn the_title_is_not_translated() {
            let two = libraries(&[(SecKind::Video, "Home Movies", true), (SecKind::Video, "Holiday Clips", true)]);
            let one = libraries(&[(SecKind::Video, "Home Movies", true)]);
            let mut type_words = std::collections::BTreeSet::new();
            for preference in SHIPPED {
                let _language = language_on_this_thread_for_test(preference);
                assert_eq!(video_label(&strip(&one)), "Home Movies", "{preference:?}: the server's word");
                let generic = plx_platform::i18n::msg::browse_kind_other_videos();
                assert_eq!(video_label(&strip(&two)), generic, "{preference:?}: the type word is translated");
                type_words.insert(generic);
            }
            assert!(type_words.len() > 1, "the type word differs between languages: {type_words:?}");
        }

        #[test]
        fn movies_and_tv_shows_pills_never_wear_a_library_title() {
            let snapshot = strip(&libraries(&[(SecKind::Movie, "Films of Mine", true),
                (SecKind::Show, "Series of Mine", true), (SecKind::Video, "Clips", true)]));
            let words: Vec<&str> = snapshot.faces.iter().filter_map(|face| match face {
                TabFace::Label(word) => Some(word.as_str()),
                TabFace::Icon(_) => None,
            }).collect();
            assert_eq!(words, ["Home", "Movies", "TV Shows", "Clips"],
                "a type pill is a type, and only Other Videos may take its library's name");
        }
    }

    #[test]
    fn chrome_production_reads_only_its_retained_directory() {
        let src = include_str!("chrome.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        assert!(!production.contains("plx_data::browse::"),
            "Chrome must read only its retained Browse directory");
    }

    #[test]
    fn shared_widgets_read_no_live_application_vocabulary() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/widgets.rs"),
        ).expect("read widgets.rs");
        let live = src.lines().filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>().join("\n");
        for forbidden in ["plx_data::browse::", "plx_plex::plex::session::", "plx_screens::"] {
            assert!(!live.contains(forbidden),
                "shared widgets must consume captured app projections, found {forbidden}");
        }
    }
}
