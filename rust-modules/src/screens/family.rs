//! **The Settings family's shared plumbing** (restructure spec §6.2 `SettingsSurface`, phase 5b):
//! the surface's INNER host and its argument vocabulary ([`SettingsPage`]), the conversions a
//! surface makes when it steps a page of its own stack under the outer dispatcher's context, and
//! the two things every page in the family does the same way — seating a `TableView` on the
//! engine's focus and naming its element keys.
//!
//! Not a screen: `screens/mod.rs`'s rule that a screen never names a sibling is what this module
//! exists to satisfy — Legal pushes a document, Privacy pushes a preview, the root pushes all of
//! them, and each names the destination through this vocabulary rather than through the module
//! that implements it.

use crate::ui::form::{Activation, FormTable, RowKey};
use crate::ui::machine::{
    Canon, Chrome, Cx, Effects, Fx, GroupId, Host, LogicalState, NavOp, ScreenId,
};
use crate::ui::screen::ScreenArg;
use crate::ui::widgets::ControlPalette;

use super::registry::{AppFx, AppMsg, DirectoryLike};

/// A first-run background may mount before the persisted hero seed arrives. Keep the same
/// generation cursor as other session-derived views, while a live hub seed remains authoritative.
pub(crate) struct SessionGround {
    ground: crate::ui::route_screen::RouteGround,
    watch: crate::plex::session::VisibleSessionWatch,
    from_session: bool,
    seed: Option<[[f32; 3]; 4]>,
}
impl SessionGround {
    pub(crate) fn new() -> Self {
        Self { ground: crate::ui::route_screen::RouteGround::new(), watch: Default::default(),
            from_session: false, seed: None }
    }
    pub(crate) fn refresh(&mut self) -> bool {
        if !self.from_session || !self.watch.changed() { return false; }
        let Some(session) = crate::plex::session::peek_settled() else { return false; };
        if self.seed == session.last_hero_blur { return false; }
        self.seed = session.last_hero_blur;
        self.ground = crate::ui::route_screen::RouteGround::for_home(self.seed);
        true
    }
}
impl std::ops::Deref for SessionGround {
    type Target = crate::ui::route_screen::RouteGround;
    fn deref(&self) -> &Self::Target { &self.ground }
}
impl std::ops::DerefMut for SessionGround {
    fn deref_mut(&mut self) -> &mut Self::Target { &mut self.ground }
}

/// Prefer the already-published Home hero; otherwise observe the persisted seed as it lands.
pub(crate) fn pre_home_ground(hubs: crate::pms::HubsView<'_>) -> SessionGround {
    let live = hubs.hero(0).filter(|hero| hero.item.has_blur).map(|hero| hero.item.blur);
    if let Some(blur) = live {
        let _ = plx_base::storage_worker::submit_retained(move || crate::plex::session::record_last_hero(blur));
    }
    let seed = live.or_else(crate::plex::session::last_hero);
    SessionGround { ground: crate::ui::route_screen::RouteGround::for_home(seed),
        watch: Default::default(), from_session: live.is_none(), seed }
}

/// A page of the surface's own stack (§6.2: root → Privacy | Legal | Favourites → Document).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SettingsPage {
    /// The Settings root (`ui::settings`'s table).
    Root,
    /// Install-wide language choice, applied next launch.
    Language,
    /// Translation contribution guide with an offline QR link.
    Contribute,
    /// Install-wide playback defaults.
    Playback,
    /// Plex account audio and subtitle preferences.
    AudioSubtitles,
    /// Favorite libraries — the onboard screen in Settings mode.
    Favourites,
    /// Privacy & data — the consent screen in Settings mode.
    Privacy,
    /// The Legal index.
    Legal,
    /// About PlxNative, a document pushed straight from the root.
    About,
    /// One Legal document, by index into `screens::legal`'s page list.
    Document(u8),
    /// One Privacy preview document (`screens::consent::Preview`).
    Preview(u8),
    /// The FIRST-RUN consent question: one stage per page, the second pushed over the first.
    ConsentStage(u8),
    /// One playback / account preference's choice list, pushed from Playback or Audio & Subtitles.
    Picker(PickerKind),
}

/// Which preference a [`SettingsPage::Picker`] edits. The parent page (`Playback` for the six local ones,
/// `AudioSubtitles` for the four account ones) and the page's own title/options live in `screens::preferences`;
/// the NAME of the field lives here because the parent pushes it through this vocabulary rather
/// than through the module that implements it (the module doc's rule).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickerKind {
    Quality,
    DirectPlay,
    SubtitleSize,
    SubtitlePosition,
    NextEpisode,
    SkipInterval,
    AudioLanguage,
    SubtitleMode,
    SubtitleLanguage,
    ForcedSubtitles,
}

impl SettingsPage {
    /// The pages a boot target opens the surface over, root first: `plxnative-settings=picker-…`
    /// is reached as root → Playback → the picker, so BACK from it lands where an interactive
    /// visit would. Every other page is booted as the surface's root.
    pub(crate) fn boot_trail(self) -> Vec<SettingsPage> {
        match self {
            SettingsPage::Picker(kind) => vec![SettingsPage::Root, kind.parent(), self],
            page => vec![page],
        }
    }
}

impl PickerKind {
    /// The page that lists this field (its picker is pushed from there and returns there).
    pub(crate) fn parent(self) -> SettingsPage {
        match self {
            PickerKind::Quality | PickerKind::DirectPlay | PickerKind::SubtitleSize | PickerKind::SubtitlePosition | PickerKind::NextEpisode | PickerKind::SkipInterval => SettingsPage::Playback,
            PickerKind::AudioLanguage | PickerKind::SubtitleMode | PickerKind::SubtitleLanguage | PickerKind::ForcedSubtitles => SettingsPage::AudioSubtitles,
        }
    }
}

impl ScreenArg for SettingsPage {
    fn chrome(&self) -> Chrome {
        Chrome::None
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            SettingsPage::Root => 100,
            SettingsPage::Playback => 108,
            SettingsPage::AudioSubtitles => 109,
            SettingsPage::Language => 110,
            SettingsPage::Contribute => 111,
            SettingsPage::Favourites => 101,
            SettingsPage::Privacy => 102,
            SettingsPage::Legal => 103,
            SettingsPage::About => 104,
            SettingsPage::Document(_) => 105,
            SettingsPage::Preview(_) => 106,
            SettingsPage::ConsentStage(_) => 107,
            SettingsPage::Picker(_) => 112,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

/// **A page's canonical encoding, for the surface's own logical state** (§5.4).
///
/// `screens::settings`'s `RouteSurface` hashes its inner stack entry by entry, and one entry is
/// distinguished from its neighbour by WHICH page it is — so both halves have to be written, and
/// they are written HERE rather than at the hash site so that a variant added to this enum is
/// answered by a census `match` beside its own declaration. A `_ =>` arm at a distant call site
/// would silently hash two different documents as one, which is exactly the class of miss the
/// recorder exists to catch.
///
/// The kind is [`ScreenArg::id`]'s number rather than a second numbering of the same question —
/// that is already the id the container keys page KIND on. The index is written for EVERY variant,
/// `0` where there is none, so a payload-less variant that later grows one cannot keep hashing
/// identically to its old self.
impl LogicalState for SettingsPage {
    fn write(&self, w: &mut Canon) {
        w.discriminant(ScreenArg::id(self).0);
        w.u8(match self {
            SettingsPage::Root
            | SettingsPage::Language
            | SettingsPage::Contribute
            | SettingsPage::Playback
            | SettingsPage::AudioSubtitles
            | SettingsPage::Favourites
            | SettingsPage::Privacy
            | SettingsPage::Legal
            | SettingsPage::About => 0,
            SettingsPage::Document(i) | SettingsPage::Preview(i) | SettingsPage::ConsentStage(i) => *i,
            SettingsPage::Picker(kind) => *kind as u8,
        });
    }
    fn probe(&self, out: &mut String) {
        out.push_str(match self {
            SettingsPage::Root => "root",
            SettingsPage::Language => "language",
            SettingsPage::Contribute => "contribute",
            SettingsPage::Playback => "playback",
            SettingsPage::AudioSubtitles => "audio-subtitles",
            SettingsPage::Favourites => "favourites",
            SettingsPage::Privacy => "privacy",
            SettingsPage::Legal => "legal",
            SettingsPage::About => "about",
            SettingsPage::Document(_) => "document",
            SettingsPage::Preview(_) => "preview",
            SettingsPage::ConsentStage(_) => "stage",
            SettingsPage::Picker(_) => "picker",
        });
        if let SettingsPage::Document(i) | SettingsPage::Preview(i) | SettingsPage::ConsentStage(i) = self {
            out.push_str(&format!("[{i}]"));
        }
        if let SettingsPage::Picker(kind) = self {
            out.push_str(&format!("[{}]", *kind as u8));
        }
    }
}

/// The inner host: the same bundle, the family's own argument, and the retained Browse directory
/// its Onboard child reads. It has no initial conditions of its own (the surface's `LogicalState`
/// covers its pages).
pub(crate) struct InnerHost;

#[derive(Default)]
pub(crate) struct NoInit;

impl LogicalState for NoInit {
    fn write(&self, _w: &mut Canon) {}
    fn probe(&self, _out: &mut String) {}
}

impl Host for InnerHost {
    type Arg = SettingsPage;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = crate::stores::browse::DirectoryView<'a>;
    type Init = NoInit;
    // No family page remembers anything on its own `ReturnState` today (`ui/machine.rs`'s
    // `Host::Memory` doc) — the surface's own `NavStack<InnerHost>` restores its child pages by
    // focus alone.
    type Memory = ();
}

impl DirectoryLike for InnerHost {
    fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a> {
        cx.views
    }
}

/// The outer context as the inner pages see it, carrying the same retained Browse directory the
/// outer host published for this frame.
/// The output lifetime is FREE (`'o`, bounded by the measure's): `Cx` is invariant in its
/// lifetime through the `Views` projection, so a caller must be able to shape one to a local
/// borrow to hand it to a `DrawFrame`.
pub(crate) fn inner_cx<'o, 'a: 'o, H: DirectoryLike>(cx: &Cx<'a, H>) -> Cx<'o, InnerHost> {
    Cx {
        views: H::directory(cx),
        tick: cx.tick,
        measure: cx.measure,
        press: cx.press,
        focus: cx.focus.clone(),
        owner: cx.owner,
    }
}

/// Seat a form-backed page's table on the engine's focus: a row key parks the selection on that row
/// and lights the list; the elem is a row's [`RowKey`] number, not its index. A key the form does
/// not know (the band, the alert, a row that left) dims the list. Every page answers `FocusMoved`
/// with this, so the drawn selection and the engine never disagree.
pub(crate) fn form_focus<Id, A, Dest>(form: &mut FormTable<Id, A, Dest>, elem: u32)
where
    Id: PartialEq + Clone,
    A: Clone,
    Dest: Clone,
{
    let on_table = form.index_of_key(RowKey(elem)).filter(|_| elem < super::registry::BAND);
    form.note_engine_key(on_table.map(|_| RowKey(elem)));
    match on_table {
        Some(i) => {
            form.table.sel = i as i32;
            form.table.list_focused = true;
        }
        None => form.table.list_focused = false,
    }
}

/// The band's group in every page of the family; the table is `GroupId(0)`, an alert `GroupId(2)`.
pub(crate) const TABLE_GROUP: GroupId = GroupId(0);
pub(crate) const BAND_GROUP: GroupId = GroupId(1);
pub(crate) const ALERT_GROUP: GroupId = GroupId(2);

/// **The ONE activation path of every form page in the family** (docs/settings-form.md,
/// "Navigation"): resolve the focus key to its row and either push the row's destination — a
/// `Nav` item emits `Fx::Nav(NavOp::Push(dest))` itself and answers `None` — or hand back the
/// row's action for the page's own `match`. No page pushes through a private path, and no page
/// owns a `RoutePush`: the surface is the only stack executor.
pub(crate) fn form_activate<Id, A>(
    form: &FormTable<Id, A, SettingsPage>,
    key: u32,
    fx: &mut Effects<'_, InnerHost>,
) -> Option<A>
where
    Id: PartialEq + Clone,
    A: Clone,
{
    let index = form.index_of_key(RowKey(key))?;
    match form.activate(index)? {
        Activation::Push(dest) => {
            fx.push(Fx::Nav(NavOp::Push(dest)));
            None
        }
        Activation::Action(action) => Some(action),
    }
}

/// Rule 8 (RIGHT on a row that opens nested content enters it, exactly as OK does): the focus key
/// of the row RIGHT should activate, if the focused row opens something.
pub(crate) fn form_right_target<Id, A, Dest>(form: &FormTable<Id, A, Dest>, key: u32) -> Option<u32>
where
    Id: PartialEq + Clone,
    A: Clone,
    Dest: Clone,
{
    let i = form.index_of_key(RowKey(key))?;
    form.table.row_opens(i as i32).then_some(key)
}

thread_local! {
    /// The surface's ground palette, published for the frame so the pages' controls are keyed to
    /// the ground they sit on (what `settings::control_palette` answered). RENDER state: set by
    /// the surface at draw, read by the pages' draws, never hashed.
    static PALETTE: std::cell::Cell<Option<ControlPalette>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn set_palette(p: ControlPalette) {
    PALETTE.with(|c| c.set(Some(p)));
}

pub(crate) fn palette() -> ControlPalette {
    PALETTE.with(|c| c.get()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// [`super::SettingsPage`]'s `ScreenId`s are what a `NavStack` uses to decide whether two
    /// requests name "the same instance" (`same_instance` is bare equality here, but `NavOp::Root`
    /// elsewhere in the library also keys eviction bookkeeping off `id()`) — two variants sharing
    /// one id by a copy-paste slip would let the container conflate two different pages.
    #[test]
    fn every_settings_page_variant_has_its_own_screen_id() {
        let pages = [
            SettingsPage::Root,
            SettingsPage::Language,
            SettingsPage::Contribute,
            SettingsPage::Playback,
            SettingsPage::AudioSubtitles,
            SettingsPage::Favourites,
            SettingsPage::Privacy,
            SettingsPage::Legal,
            SettingsPage::About,
            SettingsPage::Document(0),
            SettingsPage::Preview(0),
            SettingsPage::ConsentStage(0),
            SettingsPage::Picker(PickerKind::Quality),
        ];
        let mut ids: Vec<u32> = pages.iter().map(|p| crate::ui::screen::ScreenArg::id(p).0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), pages.len(), "two SettingsPage variants must not share a ScreenId");
    }

    /// `Document`/`Preview`/`ConsentStage` carry an index that does not change WHICH kind of page
    /// they are — the id is the variant's, not the index's — so two different documents still
    /// name the same `ScreenId` (which is exactly right: `NavStack::apply`'s `NavOp::Root` arm
    /// compares `same_instance`, not `id()`, for identity, and `id()` alone is only ever a KIND).
    #[test]
    fn an_indexed_variant_s_id_does_not_vary_with_its_index() {
        use crate::ui::screen::ScreenArg;
        assert_eq!(SettingsPage::Document(0).id(), SettingsPage::Document(5).id());
        assert_eq!(SettingsPage::Preview(0).id(), SettingsPage::Preview(3).id());
        assert_eq!(SettingsPage::ConsentStage(0).id(), SettingsPage::ConsentStage(1).id());
        // …but `same_instance` still tells them apart, since it is bare equality here:
        assert!(!SettingsPage::Document(0).same_instance(&SettingsPage::Document(1)));
    }
}

#[cfg(test)]
mod session_tests {
    #[test]
    fn session_refresh_restores_first_run_ground() {
        let _serial = plx_base::testlock::serial();
        let _session = crate::plex::session::TempSession::new("first-run-seed-refresh");
        let mut saved = (*crate::plex::session::peek()).clone();
        saved.last_hero_blur = Some([[0.2, 0.3, 0.4]; 4]);
        crate::plex::session::install_transient_for_test(true);
        let mut ground = super::pre_home_ground(crate::pms::HubsSnapshot::empty_for_test().view());
        assert!(ground.seed.is_none());
        crate::plex::session::save(&saved);
        assert!(ground.refresh());
        assert_eq!(ground.seed, saved.last_hero_blur);
        assert!(!ground.refresh());
    }
}
