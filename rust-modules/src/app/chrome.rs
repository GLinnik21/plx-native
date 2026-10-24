//! Application-owned data for the shared bar. Capture before stepping a frame; paint consumes
//! borrowed strings and never opens the session file or polls the Browse vocabulary.

use plx_ui::containers::tabs::StripMember;
use plx_machine::machine::{FocusKey, Measure};
use plx_ui::icons::Icon;
use plx_ui::widgets::{self, ChromeRead, ProfileChipRead, TabFace, TabLabels, TopFocus};
use plx_data::stores::browse::{DirectoryView, SecKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pill {
    Home,
    Section(SecKind),
    LiveTv,
    Watchlist,
    Search,
}

/// The strip's destination keys in strip order: Home, one key per type pill, Live TV, the
/// Watchlist bookmark, Search. Live TV is always offered: a server without a DVR shows an empty
/// page. shortcut: the pill does not follow the hubs (the strip rebuilds on the directory's
/// generation only); hide it once a hubs-driven rebuild exists. The one
/// place that order is written; `refresh` publishes it and the pill maps below read it, so no
/// map assumes which index Search holds.
fn strip_keys(directory: DirectoryView<'_>) -> Vec<u32> {
    let mut keys = vec![widgets::TAB_KEY_HOME];
    keys.extend((0..directory.tab_count()).filter_map(|i| directory.tab_kind(i)).map(|kind| match kind {
        SecKind::Movie => widgets::TAB_KEY_MOVIES,
        SecKind::Show => widgets::TAB_KEY_SHOWS,
    }));
    keys.push(widgets::TAB_KEY_LIVETV);
    keys.push(widgets::TAB_KEY_WATCHLIST);
    keys.push(widgets::TAB_KEY_SEARCH);
    keys
}

pub(crate) fn pill_at(directory: DirectoryView<'_>, i: usize) -> Pill {
    pill_in(i, &strip_keys(directory))
}

/// The destination a strip index names, by the KEY published at that index.
fn pill_in(i: usize, keys: &[u32]) -> Pill {
    match keys.get(i).copied() {
        Some(widgets::TAB_KEY_MOVIES) => Pill::Section(SecKind::Movie),
        Some(widgets::TAB_KEY_SHOWS) => Pill::Section(SecKind::Show),
        Some(widgets::TAB_KEY_LIVETV) => Pill::LiveTv,
        Some(widgets::TAB_KEY_WATCHLIST) => Pill::Watchlist,
        Some(widgets::TAB_KEY_SEARCH) => Pill::Search,
        _ => Pill::Home,
    }
}

pub(crate) fn pill_of(directory: DirectoryView<'_>, pill: Pill) -> Option<usize> {
    pill_index(pill, &strip_keys(directory))
}

fn pill_index(pill: Pill, keys: &[u32]) -> Option<usize> {
    let key = match pill {
        Pill::Home => widgets::TAB_KEY_HOME,
        Pill::Section(SecKind::Movie) => widgets::TAB_KEY_MOVIES,
        Pill::Section(SecKind::Show) => widgets::TAB_KEY_SHOWS,
        Pill::LiveTv => widgets::TAB_KEY_LIVETV,
        Pill::Watchlist => widgets::TAB_KEY_WATCHLIST,
        Pill::Search => widgets::TAB_KEY_SEARCH,
    };
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
        if self.tabs_generation != Some(generation) {
            self.keys = strip_keys(directory);
            self.faces = self.keys.iter().map(|&key| match key {
                widgets::TAB_KEY_HOME => TabFace::Label(plx_platform::i18n::msg::browse_chrome_home().into()),
                widgets::TAB_KEY_MOVIES => TabFace::Label(plx_platform::i18n::msg::browse_kind_movies().into()),
                widgets::TAB_KEY_SHOWS => TabFace::Label(plx_platform::i18n::msg::browse_kind_tv_shows().into()),
                // "TV", not the page's "TV en directo": the strip keeps its glass only up to
                // `GLASS_TRACK_MAX`, and the longer word put the product's bar past it.
                widgets::TAB_KEY_LIVETV => TabFace::Label(plx_platform::i18n::msg::browse_chrome_live_tv().into()),
                widgets::TAB_KEY_WATCHLIST => TabFace::Icon(Icon::Bookmark),
                _ => TabFace::Icon(Icon::Search),
            }).collect();
            self.widths = widgets::tab_widths(&self.faces, measure);
            self.tabs_generation = Some(generation);
        }
        let generation = captured.map_or_else(plx_plex::plex::session::current_gen, |(profile, _)| profile.generation);
        let session_changed = captured.is_none() && self.session_watch.changed();
        if self.profile_generation != Some(generation) || session_changed {
            let current = captured.map_or_else(plx_plex::plex::session::current, |(profile, _)| profile.user.clone());
            let account = if let Some((_, saved)) = captured { saved.account(current.as_ref()) }
                else { plx_plex::plex::session::peek().account(current.as_ref()) };
            self.thumb = current.map(|user| user.thumb).unwrap_or_default();
            let label = plx_screens::account_menu::chip_label(&account);
            let initial = account.name.as_deref().and_then(|name| name.chars().next())
                .map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            (self.initial, self.name, self.name_w) =
                widgets::profile_chip_text(&label, &initial, measure);
            self.profile_generation = Some(generation);
        }
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
        (self.initial, self.name, self.name_w) =
            widgets::profile_chip_text(name, initial, measure);
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
            widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_LIVETV, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
        let words: Vec<&str> = snapshot.faces.iter().filter_map(|face| match face {
            TabFace::Label(word) => Some(word.as_str()),
            TabFace::Icon(_) => None,
        }).collect();
        assert_eq!(&words[..4], &["Home", "Movies", "TV Shows", "TV"]);
        assert_eq!(snapshot.faces.last(), Some(&TabFace::Icon(Icon::Search)));
        assert_eq!(snapshot.faces[snapshot.faces.len() - 2], TabFace::Icon(Icon::Bookmark),
            "the Watchlist bookmark sits just before Search");
        let mut members = Vec::new();
        snapshot.members(0, None, 0.0, &mut members);
        assert_eq!(members.iter().map(|member| member.elem).collect::<Vec<_>>(),
            vec![widgets::TAB_KEY_CHIP, widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES,
                widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_LIVETV, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
    }

    #[test]
    fn every_projected_pill_round_trips_by_stable_section_identity() {
        use plx_data::stores::browse::SecKind::{Movie, Show};
        let keys = [widgets::TAB_KEY_HOME, widgets::TAB_KEY_MOVIES, widgets::TAB_KEY_SHOWS,
            widgets::TAB_KEY_LIVETV, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH];
        assert_eq!(pill_in(0, &keys), Pill::Home);
        assert_eq!(pill_in(1, &keys), Pill::Section(Movie));
        assert_eq!(pill_in(2, &keys), Pill::Section(Show));
        assert_eq!(pill_in(3, &keys), Pill::LiveTv);
        assert_eq!(pill_in(4, &keys), Pill::Watchlist);
        assert_eq!(pill_in(5, &keys), Pill::Search);
        assert_eq!(pill_in(6, &keys), Pill::Home);
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
            widgets::TAB_KEY_SHOWS, widgets::TAB_KEY_LIVETV, widgets::TAB_KEY_WATCHLIST, widgets::TAB_KEY_SEARCH]);
        assert_eq!(pill_of(view, Pill::LiveTv), Some(3));
        assert_eq!(pill_of(view, Pill::Watchlist), Some(4));
        assert_eq!(pill_of(view, Pill::Search), Some(5));
        assert_eq!(pill_at(view, 3), Pill::LiveTv);
        assert_eq!(pill_at(view, 4), Pill::Watchlist);
        assert_eq!(pill_at(view, 5), Pill::Search);
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
