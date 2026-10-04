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
use crate::screens::registry;
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
    let _sess = scratch_session("nav-structure-playback");
    let items = super::super::preferences::nav_items_for_test(super::super::preferences::Kind::Playback, None);
    assert_eq!(items.iter().map(|(_, d)| *d).collect::<Vec<_>>(),
        [SettingsPage::Picker(PickerKind::Quality), SettingsPage::Picker(PickerKind::DirectPlay),
         SettingsPage::Picker(PickerKind::SubtitleSize), SettingsPage::Picker(PickerKind::SubtitlePosition),
         SettingsPage::Picker(PickerKind::NextEpisode), SettingsPage::Picker(PickerKind::SkipInterval)]);
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
            let mut surface = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Language,
                plx_data::pms::HubsSnapshot::empty_for_test().view());
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
    walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/screens"), &mut files);
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
