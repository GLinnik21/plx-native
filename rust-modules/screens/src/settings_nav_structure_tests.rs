//! **One navigation architecture, enforced structurally** (docs/settings-form.md, "Navigation").
//!
//! For every form page the Settings family mounts — the root (signed out, and signed in to a
//! multi-user account), Playback, and Audio & Subtitles with a loaded snapshot — and for every
//! `Nav` row of each, driven through the REAL dispatcher and focus engine: OK, RIGHT (rule 8) and a
//! pointer click each push exactly that row's destination, and BACK re-seats focus on the same
//! row. A page that grew a private submenu, or a `Nav` row whose activation path differs from its
//! siblings', fails here rather than on the television.
//!
//! The last test is the grep gate: no screen outside the family surface owns a `RoutePush`.

use super::composed_tests::{consent_opened, frame, opened, path, settle_frames, walk_to, SurfaceRig};
use super::*;
use super::test_support::*;
use plx_plex::plex::account::{AudioPreferences, PreferenceRequest};
use super::super::family::PickerKind;
use crate::registry;
use plx_ui::dispatch::Dispatcher;
use plx_ui::fixture::{key, tick};
use plx_ui::form::{FormTable, RowKind};
use plx_machine::machine::{Edge, InputEvent, InputKind, Source, Tick};
use plx_ui::screen::{Activate, Hover, Stop};

#[derive(Clone, Copy, Debug)]
enum Method { Ok, Right, Click }

/// `(key, dest)` of each `Nav` item of `form`.
fn nav_items<Id: PartialEq + Clone, A: Clone>(form: &FormTable<Id, A, SettingsPage>) -> Vec<(u32, SettingsPage)> {
    (0..form.table.n_rows() as usize).filter_map(|i| match form.binding_at(i)?.kind.clone() {
        RowKind::Nav(dest) => Some((form.key_at(i)?.0, dest)),
        _ => None,
    }).collect()
}

/// The segment a page adds to the surface's stack path.
fn segment(dest: SettingsPage) -> String {
    let mut s = String::new();
    dest.probe(&mut s);
    format!("/{s}:")
}

/// Drive `method` on the row `key` of the page the dispatcher is on.
fn activate(d: &mut Dispatcher<InnerHost>, rig: &mut SurfaceRig, id: EntryId, key_elem: u32, method: Method, ms: u32) {
    let focused = FocusKey { entry: id, elem: key_elem };
    match method {
        Method::Ok => frame(d, rig, ms, vec![key(Key::Ok, tick(ms))]),
        Method::Right => frame(d, rig, ms, vec![key(Key::Right, tick(ms))]),
        Method::Click => {
            let c = cx(None);
            let screen = &d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen;
            let placed = screen.place(&key_elem, &c, At::Drawn).expect("the row has geometry");
            // No GL draw on the host: publish the row's real queried geometry to the real hit map.
            d.input.hit.fill(vec![Stop { key: focused, rect: placed.rect, rest_rect: placed.rest_rect,
                clip: placed.clip, hover: Hover::Focus, activate: Activate::Direct }]);
            d.input.hit.swap();
            d.input.hit.dpad_mode = false;
            let (x, y) = (placed.rect.cx(), placed.rect.cy());
            frame(d, rig, ms, vec![InputEvent { at: tick(ms), source: Source::Script, kind: InputKind::Pointer { x, y, hit: None } }]);
            frame(d, rig, ms + 16, vec![InputEvent { at: tick(ms + 16), source: Source::Script, kind: InputKind::Click { x, y, hit: None } }]);
        }
    }
}

/// Every `Nav` item of the page a fresh dispatcher (from `build`) stands on, by each method.
fn assert_every_nav_item_pushes_exactly_its_dest(label: &str, items: &[(u32, SettingsPage)],
    build: &dyn Fn() -> (Dispatcher<InnerHost>, SurfaceRig, EntryId))
{
    assert!(!items.is_empty(), "{label}: a form page with no Nav item proves nothing");
    for &(elem, dest) in items {
        for method in [Method::Ok, Method::Right, Method::Click] {
            let (mut d, mut rig, id) = build();
            let ms = settle_frames(&mut d, &mut rig, 16);
            // let the scroll spring bring the row on screen: a click needs its drawn geometry
            let ms = walk_to(&mut d, &mut rig, ms, elem);
            let mut ms = ms;
            for _ in 0..10 { ms = settle_frames(&mut d, &mut rig, ms); }
            let before = path(&d, id);
            activate(&mut d, &mut rig, id, elem, method, ms + 16);
            let after = path(&d, id);
            assert_eq!(after.matches(&segment(dest)).count(), 1,
                "{label}: {method:?} on key {elem} must push {dest:?}\n  before: {before}\n  after:  {after}");
            assert_eq!(after.matches('/').count(), before.matches('/').count() + 1,
                "{label}: {method:?} on key {elem} pushed exactly one page: {after}");
            let ms = settle_frames(&mut d, &mut rig, ms + 48);
            frame(&mut d, &mut rig, ms + 16, vec![key(Key::Back, tick(ms + 16))]);
            settle_frames(&mut d, &mut rig, ms + 16);
            let page_only = |p: String| p.split(" seats=").next().unwrap_or("").to_string();
            assert_eq!(page_only(path(&d, id)), page_only(before), "{label}: BACK from {dest:?} ({method:?}) pops to the same page");
            assert_eq!(d.focus(), Some(FocusKey { entry: id, elem }),
                "{label}: BACK from {dest:?} ({method:?}) re-seats the row that opened it");
        }
    }
}

fn root_items() -> Vec<(u32, SettingsPage)> {
    let page = RootPage::new(EntryId(0), plx_data::stores::browse::DirectoryView::empty_for_test());
    nav_items(&page.form)
}

#[test]
fn every_root_nav_item_pushes_exactly_its_dest_signed_out() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("nav-structure-root-out");
    let items = root_items();
    assert!(items.iter().any(|(_, d)| *d == SettingsPage::Playback), "{items:?}");
    assert_every_nav_item_pushes_exactly_its_dest("root (signed out)", &items, &opened);
}

#[test]
fn every_root_nav_item_pushes_exactly_its_dest_signed_in() {
    let _g = plx_base::testlock::serial();
    let _sess = multi_user_session("nav-structure-root-in");
    let items = root_items();
    assert!(items.iter().any(|(_, d)| *d == SettingsPage::AudioSubtitles), "{items:?}");
    assert_every_nav_item_pushes_exactly_its_dest("root (signed in)", &items, &opened);
}

#[test]
fn every_playback_field_pushes_its_picker() {
    let _g = plx_base::testlock::serial();
    let _sess = seated_scratch_session("nav-structure-playback");
    let items = super::super::preferences::nav_items_for_test(super::super::preferences::Kind::Playback, None);
    assert_eq!(items.iter().map(|(_, d)| *d).collect::<Vec<_>>(),
        [SettingsPage::Picker(PickerKind::Quality), SettingsPage::Picker(PickerKind::DirectPlay),
         SettingsPage::Picker(PickerKind::SubtitleSize), SettingsPage::Picker(PickerKind::SubtitlePosition),
         SettingsPage::Picker(PickerKind::HdrOverlay), SettingsPage::Picker(PickerKind::NextEpisode), SettingsPage::Picker(PickerKind::DeckPress),
         SettingsPage::Picker(PickerKind::SkipInterval), SettingsPage::Picker(PickerKind::InstantDpadSeeking), SettingsPage::Picker(PickerKind::DolbyVision)]);
    assert_every_nav_item_pushes_exactly_its_dest("playback", &items, &|| consent_opened(SettingsPage::Playback));
}

#[test]
fn every_audio_and_subtitles_field_pushes_its_picker() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("nav-structure-audio");
    let previous = plx_plex::plex::session::current_snapshot();
    struct Restore(std::sync::Arc<plx_plex::plex::session::CurrentProfile>);
    impl Drop for Restore {
        fn drop(&mut self) { plx_plex::plex::session::publish_profile_for_test(self.0.user.clone(), self.0.generation); }
    }
    let _restore = Restore(previous);
    let user = plx_plex::plex::session::UserRef { id: 7, uuid: "nav-structure-audio".into(), ..Default::default() };
    plx_plex::plex::session::publish_profile_for_test(Some(user.clone()), 81);
    let (request, snapshot) = PreferenceRequest::fixture_for_test(user, 81, AudioPreferences::default());
    let items = super::super::preferences::nav_items_for_test(super::super::preferences::Kind::AudioSubtitles, Some(&AudioPreferences::default()));
    assert_eq!(items.len(), 4, "{items:?}");
    assert_every_nav_item_pushes_exactly_its_dest("audio & subtitles", &items, &|| {
        let (mut d, mut rig, id) = consent_opened(SettingsPage::AudioSubtitles);
        frame(&mut d, &mut rig, 32, vec![]);
        let Some(registry::PreferenceCmd::Load { reply }) = rig.preference_commands.pop() else {
            panic!("the page asks its host for the load");
        };
        reply.send(registry::AccountPreferenceReply { request: Some(request.clone()), outcome: Ok(snapshot.clone()) }).unwrap();
        frame(&mut d, &mut rig, 48, vec![]);
        (d, rig, id)
    });
}

#[test]
fn the_under_posters_details_row_pushes_its_picker() {
    let _g = plx_base::testlock::serial();
    let _sess = seated_scratch_session("nav-structure-under-posters");
    let items = super::under_posters::nav_items_for_test();
    assert_eq!(items.iter().map(|(_, d)| *d).collect::<Vec<_>>(), [SettingsPage::UnderPostersDetails]);
    assert_every_nav_item_pushes_exactly_its_dest("under posters", &items, &|| consent_opened(SettingsPage::UnderPosters));
}

/// Holds the live Under posters cell on its default for the test and puts it back after it.
struct UnderPostersCell;
impl UnderPostersCell {
    fn reset() -> Self { Self::holding(plx_plex::plex::session::UnderPosters::DEFAULT) }
    fn holding(value: plx_plex::plex::session::UnderPosters) -> Self {
        plx_media::route::restore_under_posters(value);
        UnderPostersCell
    }
}
impl Drop for UnderPostersCell {
    fn drop(&mut self) { plx_media::route::restore_under_posters(plx_plex::plex::session::UnderPosters::DEFAULT); }
}

/// The Settings surface on a real dispatcher, driven by keys, with the app's write door played by
/// [`Self::answer`]: the page sends `Choose`, the app persists and publishes the value, then replies.
struct Walk {
    d: Dispatcher<InnerHost>,
    rig: SurfaceRig,
    id: EntryId,
    ms: u32,
}

impl Walk {
    fn press(&mut self, k: Key) {
        self.ms += 16;
        frame(&mut self.d, &mut self.rig, self.ms, vec![key(k, tick(self.ms))]);
        self.ms = settle_frames(&mut self.d, &mut self.rig, self.ms);
    }
    /// The writes the page asked for since the last call (in order), each one answered durable.
    fn answer(&mut self) -> Vec<plx_plex::plex::session::UnderPosters> {
        let mut asked = Vec::new();
        for command in std::mem::take(&mut self.rig.preference_commands) {
            let registry::PreferenceCmd::Choose(plx_media::route::Choice::UnderPosters(value), reply) = command else {
                panic!("the page asked for something other than an Under posters write");
            };
            plx_media::route::restore_under_posters(value);
            reply.send(true).unwrap();
            asked.push(value);
        }
        self.ms = settle_frames(&mut self.d, &mut self.rig, self.ms);
        self.ms = settle_frames(&mut self.d, &mut self.rig, self.ms);
        asked
    }
    fn elem(&self) -> Option<u32> { self.d.focus().map(|k| k.elem) }
    fn on(&self, page: &str) -> bool { path(&self.d, self.id).contains(page) }
}

/// **A picker pushed by OK opens with the cursor on the line that is checked**, whatever line that
/// is: the Details picker lists Nothing first, so a stored Genre must seat on the second row and a
/// stored Nothing on the first. (The page asks for that seat when it mounts; the surface's own
/// seat for a pushed page must not land after it and put the cursor back on the first row.)
#[test]
fn the_details_picker_opens_seated_on_the_stored_line_not_its_first_row() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
    let _g = plx_base::testlock::serial();
    let _sess = seated_scratch_session("nav-walk-under-posters-seat");
    for stored in [UnderPostersDetails::Genre, UnderPostersDetails::AgeRatingScore, UnderPostersDetails::None] {
        let _cell = UnderPostersCell::holding(UnderPosters { title: false, details: stored, sort_value: false });
        let (d, rig, id) = opened();
        let mut w = Walk { d, rig, id, ms: 0 };
        w.ms = settle_frames(&mut w.d, &mut w.rig, 16);
        let row = root_key(RootId::UnderPosters);
        w.ms = walk_to(&mut w.d, &mut w.rig, w.ms, row);
        w.press(Key::Ok);
        w.press(Key::Down);
        assert_eq!(w.elem(), Some(2), "Down from Title reaches Details");
        w.press(Key::Ok);
        assert!(w.on("under_posters_details"), "OK on Details pushed the picker: {}", path(&w.d, w.id));
        assert_eq!(w.elem(), Some(super::under_posters::details_elem_for_test(stored)), "{stored:?}: the picker opens on its checked line");
    }
}

/// Puts every Playback live setting back to what it was when the test began.
struct PlaybackCells {
    quality: plx_media::route::Quality,
    direct_play: plx_media::route::DirectPlayMode,
    size: plx_media::route::SubtitleSize,
    position: plx_media::route::SubtitlePosition,
    hdr: plx_media::route::HdrOverlayBrightness,
    next_episode: plx_media::route::NextEpisodeMode,
    instant_dpad_seeking: plx_plex::plex::session::InstantDpadSeeking,
    dolby_vision: plx_plex::plex::session::DolbyVisionMode,
    deck_press: plx_media::route::DeckPress,
    skip: plx_media::route::SkipInterval,
}
impl PlaybackCells {
    fn capture() -> Self {
        use plx_media::route as r;
        PlaybackCells { quality: r::quality(), direct_play: r::direct_play_mode(), size: r::subtitle_size(),
            position: r::subtitle_position(), hdr: r::hdr_overlay_brightness(), next_episode: r::next_episode_mode(), instant_dpad_seeking: r::instant_dpad_seeking(),
            dolby_vision: r::dolby_vision_mode(), deck_press: r::deck_press(), skip: r::skip_interval() }
    }
}
impl Drop for PlaybackCells {
    fn drop(&mut self) {
        use plx_media::route as r;
        r::restore_quality(self.quality); r::restore_direct_play_mode(self.direct_play);
        r::restore_subtitle_size(self.size); r::restore_subtitle_position(self.position);
        r::restore_hdr_overlay_brightness(self.hdr); r::restore_next_episode_mode(self.next_episode); r::restore_instant_dpad_seeking(self.instant_dpad_seeking);
        r::restore_dolby_vision_mode(self.dolby_vision); r::restore_deck_press(self.deck_press); r::restore_skip_interval(self.skip);
    }
}

/// Open the page `build` mounts, walk to the row `row` and press OK; the page it pushed must be
/// `dest`, seated on `line` (the checked line, never the first row); BACK pops to the row.
fn assert_a_pushed_picker_opens_on_its_checked_line_and_pops_to_its_row(
    label: &str, build: &dyn Fn() -> (Dispatcher<InnerHost>, SurfaceRig, EntryId), row: u32, dest: SettingsPage, line: u32)
{
    assert_ne!(line, 0, "{label}: a stored first row cannot tell a seated picker from an unseated one");
    let (d, rig, id) = build();
    let mut w = Walk { d, rig, id, ms: 0 };
    w.ms = settle_frames(&mut w.d, &mut w.rig, 16);
    w.ms = walk_to(&mut w.d, &mut w.rig, w.ms, row);
    w.press(Key::Ok);
    assert_eq!(path(&w.d, w.id).matches(&segment(dest)).count(), 1, "{label}: OK on key {row} pushed {dest:?}: {}", path(&w.d, w.id));
    assert_eq!(w.elem(), Some(line), "{label}: the pushed page opens on its checked line: {}", path(&w.d, w.id));
    w.press(Key::Back);
    assert_eq!(path(&w.d, w.id).matches(&segment(dest)).count(), 0, "{label}: BACK popped {dest:?}: {}", path(&w.d, w.id));
    assert_eq!(w.elem(), Some(row), "{label}: BACK re-seats the row that opened the page");
}

/// **Every Settings page that pushes a page with a checked line opens it on that line**, and BACK
/// re-seats the row it came from. The pushed page asks for its seat from `Mount`, and
/// `RouteSurface::request` queues the destination's default first-row seat BEFORE that mount runs;
/// queued after, it put every one of them back on the first row. Each row stores the LAST line of
/// its ladder, so a first-row seat is a red row:
/// Playback (Quality, Direct play, Subtitle size, Subtitle position, HDR overlay, Next episode,
/// Deck press, Skip interval, Instant D-pad seeking), Audio & Subtitles (Audio language, Subtitle mode, Subtitle language,
/// Forced subtitles), Under posters (Details), the Settings root (Language). The Interface
/// resolution picker is not in the table: its row exists only where the platform offers it.
#[test]
fn every_pushed_picker_opens_on_its_checked_line_and_pops_to_its_row() {
    use plx_media::route as r;
    use plx_plex::plex::session as ses;
    let _g = plx_base::testlock::serial();
    let _sess = seated_scratch_session("nav-structure-checked-lines");

    // Playback: the live setting each picker reads, set to the last line of its ladder.
    let _cells = PlaybackCells::capture();
    let quality = *r::available_quality_ladder().last().expect("a quality ladder");
    let ladder_last = |len: usize| (len - 1) as u32;
    r::restore_quality(quality);
    r::restore_direct_play_mode(r::DirectPlayMode::Disabled);
    r::restore_subtitle_size(*ses::SubtitleSize::LADDER.last().unwrap());
    r::restore_subtitle_position(*ses::SubtitlePosition::LADDER.last().unwrap());
    r::restore_hdr_overlay_brightness(*ses::HdrOverlayBrightness::LADDER.last().unwrap());
    r::restore_next_episode_mode(*ses::NextEpisodeMode::LADDER.last().unwrap());
    r::restore_deck_press(*ses::DeckPress::LADDER.last().unwrap());
    r::restore_skip_interval(*ses::SkipInterval::LADDER.last().unwrap());
    r::restore_instant_dpad_seeking(*ses::InstantDpadSeeking::LADDER.last().unwrap());
    r::restore_dolby_vision_mode(*ses::DolbyVisionMode::LADDER.last().unwrap());
    let lines = |kind: PickerKind| match kind {
        PickerKind::Quality => ladder_last(r::available_quality_ladder().len()),
        PickerKind::DirectPlay => 2,
        PickerKind::SubtitleSize => ladder_last(ses::SubtitleSize::LADDER.len()),
        PickerKind::SubtitlePosition => ladder_last(ses::SubtitlePosition::LADDER.len()),
        PickerKind::HdrOverlay => ladder_last(ses::HdrOverlayBrightness::LADDER.len()),
        PickerKind::NextEpisode => ladder_last(ses::NextEpisodeMode::LADDER.len()),
        PickerKind::DeckPress => ladder_last(ses::DeckPress::LADDER.len()),
        PickerKind::SkipInterval => ladder_last(ses::SkipInterval::LADDER.len()),
        PickerKind::InstantDpadSeeking => ladder_last(ses::InstantDpadSeeking::LADDER.len()),
        PickerKind::DolbyVision => ladder_last(ses::DolbyVisionMode::LADDER.len()),
        other => panic!("{other:?} is not a Playback picker"),
    };
    let playback = super::super::preferences::nav_items_for_test(super::super::preferences::Kind::Playback, None);
    assert_eq!(playback.len(), 10, "every Playback picker is in the table: {playback:?}");
    for &(row, dest) in &playback {
        let SettingsPage::Picker(kind) = dest else { panic!("a Playback row pushes a picker: {dest:?}") };
        assert_a_pushed_picker_opens_on_its_checked_line_and_pops_to_its_row(
            &format!("playback {kind:?}"), &|| consent_opened(SettingsPage::Playback), row, dest, lines(kind));
    }

    // Under posters: Details, the fourth ladder line.
    {
        use ses::{UnderPosters, UnderPostersDetails};
        let _cell = UnderPostersCell::holding(UnderPosters { title: false, details: UnderPostersDetails::AgeRatingScore, sort_value: false });
        let items = super::under_posters::nav_items_for_test();
        assert_eq!(items.len(), 1, "{items:?}");
        let (row, dest) = items[0];
        assert_a_pushed_picker_opens_on_its_checked_line_and_pops_to_its_row("under posters", &|| consent_opened(SettingsPage::UnderPosters), row, dest,
            super::under_posters::details_elem_for_test(UnderPostersDetails::AgeRatingScore));
    }

    // Audio & Subtitles: the account's stored answers.
    {
        use plx_plex::plex::account::AudioPreferences;
        let previous = ses::current_snapshot();
        struct Restore(std::sync::Arc<ses::CurrentProfile>);
        impl Drop for Restore {
            fn drop(&mut self) { ses::publish_profile_for_test(self.0.user.clone(), self.0.generation); }
        }
        let _restore = Restore(previous);
        let user = ses::UserRef { id: 7, uuid: "nav-structure-checked-lines".into(), ..Default::default() };
        ses::publish_profile_for_test(Some(user.clone()), 81);
        let language_line = |code: &str| 1 + plx_plex::plex::languages::picker().position(|l| l.code == code)
            .unwrap_or_else(|| panic!("{code} is in the language picker")) as u32;
        let prefs = AudioPreferences { stated_language: Some("fr".into()), subtitle_language: Some("de".into()),
            subtitle_mode: 2, subtitle_forced: 3, ..Default::default() };
        let (request, snapshot) = PreferenceRequest::fixture_for_test(user, 81, prefs.clone());
        let items = super::super::preferences::nav_items_for_test(super::super::preferences::Kind::AudioSubtitles, Some(&prefs));
        assert_eq!(items.len(), 4, "every Audio & Subtitles picker is in the table: {items:?}");
        for &(row, dest) in &items {
            let SettingsPage::Picker(kind) = dest else { panic!("an Audio row pushes a picker: {dest:?}") };
            let line = match kind {
                PickerKind::AudioLanguage => language_line("fr"),
                PickerKind::SubtitleLanguage => language_line("de"),
                PickerKind::SubtitleMode => 2,
                PickerKind::ForcedSubtitles => 3,
                other => panic!("{other:?} is not an Audio & Subtitles picker"),
            };
            assert_a_pushed_picker_opens_on_its_checked_line_and_pops_to_its_row(&format!("audio {kind:?}"), &|| {
                let (mut d, mut rig, id) = consent_opened(SettingsPage::AudioSubtitles);
                frame(&mut d, &mut rig, 32, vec![]);
                let Some(registry::PreferenceCmd::Load { reply }) = rig.preference_commands.pop() else {
                    panic!("the page asks its host for the load");
                };
                reply.send(registry::AccountPreferenceReply { request: Some(request.clone()), outcome: Ok(snapshot.clone()) }).unwrap();
                frame(&mut d, &mut rig, 48, vec![]);
                (d, rig, id)
            }, row, dest, line);
        }
    }

    // The Settings root: the Language page, on the saved language. (The Interface resolution picker
    // is pushed only where the platform offers a resolution choice, `offers_ui_resolution`: the host
    // root has no such row, so that one picker is the TV's to verify.)
    {
        let language = *LANGUAGES.last().expect("a language list");
        let _saved = SavedLanguageForTest::new(language);
        let root = root_items();
        let row_of = |dest: SettingsPage| root.iter().find(|(_, d)| *d == dest).map(|(row, _)| *row)
            .unwrap_or_else(|| panic!("the root has a row for {dest:?}: {root:?}"));
        assert_a_pushed_picker_opens_on_its_checked_line_and_pops_to_its_row("root language", &opened,
            row_of(SettingsPage::Language), SettingsPage::Language, LangId::Choice(language).key().0);
    }
}

struct SavedLanguageForTest(plx_platform::i18n::Preference);
impl SavedLanguageForTest {
    fn new(value: plx_platform::i18n::Preference) -> Self { Self(plx_platform::i18n::saved_preference_for_test(value)) }
}
impl Drop for SavedLanguageForTest {
    fn drop(&mut self) { plx_platform::i18n::saved_preference_for_test(self.0); }
}

/// **Settings > Libraries > Under posters, walked with the remote** (`routes.md` R13): from the
/// Settings root, Down to the row and OK; the Title switch toggles; Details opens the picker, BACK
/// from it re-seats Details with nothing written; a pick writes, pops when durable and the row reads
/// it; Reset restores both; BACK leaves to the root, on the row that opened the page, and BACK again
/// dismisses the surface.
#[test]
fn the_under_posters_page_walks_title_details_reset_and_back_with_keys() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
    let _g = plx_base::testlock::serial();
    let _sess = seated_scratch_session("nav-walk-under-posters");
    let _cell = UnderPostersCell::reset();
    let (d, rig, id) = opened();
    let mut w = Walk { d, rig, id, ms: 0 };
    w.ms = settle_frames(&mut w.d, &mut w.rig, 16);
    let row = root_key(RootId::UnderPosters);
    w.ms = walk_to(&mut w.d, &mut w.rig, w.ms, row);
    w.press(Key::Ok);
    assert!(w.on("under_posters sel=1 "), "OK on the root row opened the page, seated on the Title switch: {}", path(&w.d, w.id));
    assert_eq!(w.elem(), Some(1));

    // The Title switch.
    w.press(Key::Ok);
    assert_eq!(w.answer(), [UnderPosters { title: true, details: UnderPostersDetails::YearLength, sort_value: true }], "OK on Title asks for the switch on");
    assert!(plx_media::route::under_posters().title);
    assert!(w.on("title=true"), "the page reads the stored value: {}", path(&w.d, w.id));

    // Details: BACK from the picker writes nothing and re-seats the row.
    w.press(Key::Down);
    assert_eq!(w.elem(), Some(2), "Down from Title reaches Details");
    w.press(Key::Ok);
    assert!(w.on("under_posters_details"), "OK on Details pushed the picker: {}", path(&w.d, w.id));
    w.press(Key::Back);
    assert!(w.on("under_posters sel=2 "), "BACK from the picker re-seats the Details row: {}", path(&w.d, w.id));
    assert_eq!(w.elem(), Some(2));
    assert!(w.answer().is_empty(), "BACK from the picker wrote nothing");

    // A pick: Down from the checked line to the next, OK writes it and the picker pops once durable.
    w.press(Key::Ok);
    let year_length = super::under_posters::details_elem_for_test(UnderPostersDetails::YearLength);
    let genre = super::under_posters::details_elem_for_test(UnderPostersDetails::Genre);
    assert_eq!(w.elem(), Some(year_length), "the picker opens seated on the checked line (Year / Length), not its first row");
    w.press(Key::Down);
    assert_eq!(w.elem(), Some(genre), "Down moves to the next line (Genre)");
    w.press(Key::Ok);
    assert_eq!(w.answer(), [UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: true }], "the pick is one write, title kept");
    assert!(w.on("under_posters sel=2 "), "a durable pick pops the picker onto the Details row: {}", path(&w.d, w.id));
    assert_eq!(plx_media::route::under_posters().details, UnderPostersDetails::Genre);
    assert!(w.on("details=1"), "the Details row reads the pick (Genre, ladder index 1): {}", path(&w.d, w.id));

    // The page writes from what is stored, not from what it last wrote: a Title toggle after the
    // pick keeps the pick.
    w.press(Key::Up);
    assert_eq!(w.elem(), Some(1), "Up from Details reaches Title");
    w.press(Key::Ok);
    assert_eq!(w.answer(), [UnderPosters { title: false, details: UnderPostersDetails::Genre, sort_value: true }], "a Title toggle after the pick keeps the picked details");

    // Reset restores both.
    w.press(Key::Down);
    w.press(Key::Down);
    assert_eq!(w.elem(), Some(3), "Down from Details reaches Sort value");
    w.press(Key::Ok);
    assert!(!w.answer()[0].sort_value, "the default is On, so OK asks for Off");
    w.press(Key::Down);
    assert_eq!(w.elem(), Some(30), "Down from Sort value reaches Reset");
    w.press(Key::Ok);
    assert_eq!(w.answer(), [UnderPosters::DEFAULT], "Reset asks for the default of both");
    assert_eq!(plx_media::route::under_posters(), UnderPosters::DEFAULT);

    // BACK leaves the page onto the row that opened it, and BACK again dismisses the surface.
    w.press(Key::Back);
    assert!(w.on("root sel="), "BACK left the page: {}", path(&w.d, w.id));
    assert_eq!(w.elem(), Some(row), "BACK re-seats the root row that opened the page");
    w.press(Key::Back);
    assert_eq!(w.d.nav.modals.top().map(|s| s.phase), Some(plx_ui::containers::modal::Phase::Closing), "BACK at the root dismisses the surface");
}

/// The Language page's Nav rows, driven through the real `RouteSurface` by OK (`Activate`) and
/// RIGHT. Not through the dispatcher like the other pages: the pushed `Contribute` page draws a QR
/// code, which the host test build has no GL context for.
#[test]
fn every_language_nav_item_pushes_exactly_its_dest() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("nav-structure-language");
    let items = nav_items(&LanguagePage::new(EntryId(0)).form);
    assert_eq!(items.iter().map(|(_, d)| *d).collect::<Vec<_>>(), [SettingsPage::Contribute]);
    for &(elem, dest) in &items {
        for method in [Method::Ok, Method::Right] {
            let mut surface = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Language);
            step(&mut surface, ScreenEvent::Mount, None);
            let focus = FocusKey { entry: EntryId(0), elem };
            let ev = match method {
                Method::Ok => ScreenEvent::Activate(elem),
                _ => ScreenEvent::Input(InputEvent { at: Tick::default(), source: Source::Sdl,
                    kind: InputKind::Key { key: Key::Right, sym: 0, wcode: 0, edge: Edge::Down, at_edge: true } }),
            };
            assert_eq!(surface.inner.top().unwrap().arg, SettingsPage::Language);
            step(&mut surface, ev, Some(focus));
            assert_eq!(surface.inner.top().unwrap().arg, dest,
                "language: {method:?} on key {elem} must push exactly {dest:?}");
            assert_eq!(surface.inner.depth(), 2, "language: {method:?} pushed exactly one page");
        }
    }
}

#[test]
fn every_legal_index_row_pushes_its_own_document() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("nav-structure-legal");
    let items = super::super::legal::nav_items_for_test();
    assert_eq!(items.len(), 6, "{items:?}");
    assert!(items.iter().all(|(_, d)| matches!(d, SettingsPage::Document(_))), "{items:?}");
    assert_every_nav_item_pushes_exactly_its_dest("legal index", &items, &|| consent_opened(SettingsPage::Legal));
}

#[test]
fn every_consent_settings_preview_row_pushes_its_preview() {
    let _g = plx_base::testlock::serial();
    let _sess = scratch_session("nav-structure-consent");
    let items = super::super::consent::nav_items_for_test(None);
    assert_eq!(items.len(), 5, "{items:?}");
    assert!(items.iter().all(|(_, d)| matches!(d, SettingsPage::Preview(_))), "{items:?}");
    assert_every_nav_item_pushes_exactly_its_dest("consent (settings)", &items, &|| consent_opened(SettingsPage::Privacy));
}

/// **No page in the family owns a `RoutePush` but the family itself.** A picker (or any future
/// drill-down) is a stack page pushed through `Fx::Nav`; the surface's one push spring carries it.
/// A `RoutePush` field anywhere else in `screens/` is a private submenu coming back.
#[test]
fn only_the_family_surface_owns_a_route_push() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() { walk(&p, out); } else if p.extension().is_some_and(|e| e == "rs") { out.push(p); }
        }
    }
    let mut files = Vec::new();
    walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    assert!(files.len() > 20, "the walk found the screens tree");
    let mut offenders = Vec::new();
    for file in files {
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name == "settings.rs" || name.contains("_tests") || name == "tests.rs" { continue; }
        for (n, line) in std::fs::read_to_string(&file).unwrap().lines().enumerate() {
            if line.contains("RoutePush") && !line.trim_start().starts_with("//") {
                offenders.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(offenders.is_empty(), "a screen owns a RoutePush: {offenders:#?}");
}
