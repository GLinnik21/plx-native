//! **The application bundle's SCREEN-SIDE half** (restructure spec §3.1, phase 5b): the effects a
//! screen may ask for (`AppFx`), the messages a machine receives (`AppMsg`), and the requests an
//! owned screen makes of the legacy LOOP (`LoopReq`) while the two coexist (§14).
//!
//! **…and, since restructure phase 10, the concrete `ScreenArg` ([`AppArg`]) and the one `mount`
//! match ([`AppMounter`])** — which §2.1 always put here and which lived in `app/bridge.rs` until
//! then. The blocker was stated in this doc and is gone: the argument carried the legacy `Route`,
//! `Route` was `app`-private, and a screen may not name `app::`. `Route` moved here in phase 10
//! and was FOLDED INTO [`AppArg`] in phase 12 — its seven mountable values are flat variants and
//! the enum is gone (§15.2), so a page argument is one value rather than a value inside a value. What §0's criterion 5 buys
//! for that is the thing this module is for: **a new screen touches its own `screens/<name>.rs`,
//! this file, `dev/scenarios.rs` and `tests/manifest.json` and nothing else** — the variant, the id,
//! the `mount` arm and the recorded shape ([`SCREEN_SHAPES`]) are all here.
//!
//! What stays in `app/bridge.rs` is the concrete `Host` and the rig it lends the dispatcher
//! (`AppHost`, `AppViews`, `Bridge`), plus the two conversions that are about the LEGACY trail
//! rather than about the alphabet (`AppArg::from_node`/`node`). The mounter is generic over the
//! host for the same reason the screens are: the views it needs arrive through the `*Like`
//! accessors below, so it names no application type. The Settings family instantiates the same
//! screens a second time for the surface's own inner stack (`screens::settings::InnerHost`), which
//! is how one `OnboardScreen` mounts twice (§6.2).
//!
//! `LoopReq` is a DEBT with a phase number on each variant: a request the loop performs because
//! the machine that should own it (Session, Player, Navigation over the app's real stack) is not
//! on the dispatcher yet. The bridge drains them after every dispatcher frame.

use crate::screens::family::SettingsPage;
use crate::screens::settings::{Family, RouteSurface};
use crate::stores::{StoreCmd, StoreId, StoreWork};
use crate::ui::machine::{
    Canon, Chrome, Cx, Effects, EntryId, Host, InstanceId, LogicalState, ScreenId,
};
use crate::ui::screen::{Mounter, ReturnState, Screen};

/// The application's effects (spec §3.1). `Store` since phase 4; `Consent` and `Loop` since 5b.
pub(crate) enum AppFx {
    /// A store command, executed as a `Deliver` to the store machine in the same drain.
    Store(StoreId, StoreCmd),
    /// Poll only the store work this visible route owns, after its read-only step returns.
    StoreWork(StoreWork),
    /// The consent MACHINE's command (§2.2): it owns the two decisions and publishes them.
    Consent(ConsentCmd),
    /// A request of the legacy loop (§14) — see [`LoopReq`].
    Loop(LoopReq),
    /// Content-page requests, executed by the navigation bridge during coexistence (phase 7).
    Content(ContentReq),
    /// Home-page semantic requests. The bridge owns navigation/player/menu execution.
    Home(HomeReq),
    /// Library-page semantic requests. The bridge owns navigation/player/item-menu execution.
    Library(LibraryReq),
    Search(SearchReq),
    /// Player-route requests — what an overlay panel on the player's own `ModalStack` asks of the
    /// loop, because the thing being asked for needs the `MainThread` token, the route or the
    /// trail (§14). Executed by `app/playback.rs`'s drain.
    Player(PlayerReq),
    /// The item context menu's committed row (phase 10) — see [`ItemMenuReq`].
    ItemMenu(ItemMenuReq),
}

/// **What the item context menu asks of the loop**, once its own `step` has resolved the pressed
/// row to an [`crate::screens::item_menu::Action`].
///
/// The panel owns its rows, its cursor, its anchor and its dismissal; it owns none of what an
/// action DOES. Every arm of `app::input::apply_item_action` either navigates (a `Route` flip plus
/// a blocking metadata fetch), starts playback (the playback session's `&mut`) or reaches the
/// view-state store, and a screen may name none of the three (§2.1). So the panel decides and the
/// loop performs, exactly as `LibraryReq` and `PlayerReq` do for their screens.
///
/// The four payload fields beside the action are the whole of what `MenuHost` and two `static mut`s
/// were still deciding by the time the route that carried them was deleted:
///
/// * `sid` — WHICH SERVER the action's ratingKey is about, captured when the menu was presented.
///   Resolving it against `plex::current_server()` at the press is the reported bug itself: on a
///   Continue Watching shelf merged across servers, Play from Start on a friend's episode found
///   OUR row with the same key and played a different film under the friend's title.
/// * `item` — the catalog ROW the menu was opened on, for the one action a key cannot perform.
///   `route::request_play_movie` needs the part id, duration, resume offset and media flags, and
///   the only way back from a bare key used to be `pms::index_of_rk`, which walks the HOME hub
///   catalog alone — so on a Library, Search or person tile the press silently did nothing.
///   `None` for the detail page's filmstrip and season menus, which play through the loaded season.
/// * `loaded_episode` — `MenuHost::is_loaded_episode`, the one bit that changed what an action
///   means: only the filmstrip's rk is a leaf of the season the mounted page has loaded, so its
///   Play from Start goes through that page's own episode path and its scrobble makes the page
///   re-read itself. Every other entry point — including the detail page's own RELATED shelf,
///   which stands on that page while its tiles are OTHER items — is a card row.
/// * `from_home` — `menu_leave`'s only question (`matches!(host, MenuHost::Home)`): a navigation
///   out of a menu opened on a HOME shelf resets the trail, because Home is the trail's root and
///   the page being left is not one BACK can return to.
pub(crate) struct ItemMenuReq {
    pub(crate) act: crate::screens::item_menu::Action,
    pub(crate) sid: crate::plex::ServerId,
    pub(crate) item: Option<crate::pms::PmsMovie>,
    pub(crate) loaded_episode: bool,
    pub(crate) from_home: bool,
}

/// **What a player overlay asks for**, once its own `step` has decided.
///
/// A panel on the player's page-owned `ModalStack` owns its own state and its own input, but not
/// the playback: seeking, pausing, applying a quality rung and leaving for a detail page all need
/// the `MainThread` token and the container ops `app::bridge` owns, neither of which a screen may
/// name (§2.1).
/// So the panel decides and the loop performs, exactly as `LibraryReq` does for the Library.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PlayerReq {
    /// A transport key FELL THROUGH the panel (§ `overlay_transport_key_tests`): a viewer holding
    /// the track menu, the Info card or the Chapters strip open still expects PAUSE/PLAY to work,
    /// and the panel stays up. `true` = the key was PLAY, `false` = PAUSE; a PLAYPAUSE toggle is
    /// neither and is carried as `None`.
    Transport(Option<bool>),
    /// Seek to this position in ns and resume if paused — the Chapters strip's OK.
    SeekTo(i64),
    /// **Commit a SCRUB to this position, and leave a paused film paused** (restructure phase 12,
    /// PX-PLAYER) — `app::playback::commit_seek`, the other half of the pair above.
    ///
    /// The difference is the reason there are two. [`SeekTo`](PlayerReq::SeekTo) RESUMES, because
    /// a viewer who picked a chapter asked to watch it. A scrub does not: the transport's bar is
    /// how a PAUSED film is moved, and starting it there is a behaviour nobody asked for. Holding
    /// the pause across the seek needs three pieces of state that are the loop's and no screen's —
    /// `App::repause_at`, `TX.resume_pend` and the bounded seek-preroll feed override that lets
    /// the pipeline decode the landed frame without publishing a viewer Resume — which is why this
    /// is a request rather than something `PlayerScreen` performs.
    CommitSeek(i64),
    /// Apply the `…` popover's chosen row.
    More(crate::ui::more_menu::Action),
    /// Apply the Info card's focused action.
    Info(crate::ui::info_panel::InfoAction),
    /// The panel took a DOWN past its own bottom: drop the HUD's ring onto the tabs row.
    FocusTabs,
    /// Keep the transport alive while a panel is being read (`HUD_MENU_MS`), or hand it the
    /// ordinary linger as a panel closes (`HUD_LINGER_MS`).
    ExtendHud(u32),
    /// The Info card's OK landed on a control FACE, which has a press dip of its own: arm the
    /// tvOS press and commit on the spring-back rather than acting now.
    ArmInfoPress,
    /// The track menu picked a row. `route::commit_audio_selection`/`commit_subtitle_selection`
    /// take the playback session's `&mut`, which a screen never has (§2.2) — so the panel decides
    /// and the loop performs, exactly as every other request in this enum.
    CommitTrack(crate::ui::track_menu::TrackCommit),
    /// **Present one of the four overlays directly** (restructure phase 12) — the tabs row's OK
    /// (`OverlayKind::Info`/`::Chapters`, the old `key_ok`'s `focus == 2` arm) and the failure
    /// read-out's own recovery escape (`OverlayKind::More { quality: true }`, the old
    /// `key_player_failed`'s `ChooseQuality` arm). Both used to reach
    /// `super::bridge::open_player_overlay` straight from the loop's key ladder; `PlayerScreen`
    /// may not name that function itself (§2.2), so it asks instead.
    OpenOverlay(crate::screens::player::overlay::OverlayKind),
    /// **OK landed on the transport's own control row** — whichever disc, the Skip pill or the Up
    /// Next tile currently occupies it (restructure phase 12, the old `key_ok`'s `focus == 1`
    /// arm). Arms the same tvOS press dip [`ArmInfoPress`](PlayerReq::ArmInfoPress) does; the
    /// loop's existing commit-frame dispatch (`app/run.rs`'s `Route::Player =>
    /// activate_player_row(...)`) is unchanged and performs whatever the row decides once the
    /// spring-back has played.
    ArmControlRow,
    /// **Leave the player** — the one ritual the STOP key, a BACK with nothing else open, and the
    /// failure read-out's own BACK escape all perform (`exit_player`; restructure phase 12,
    /// replacing `app/run.rs`'s direct calls to it from the player's own key arms).
    Exit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SearchReq {
    Back,
    Detail { sid: crate::plex::ServerId, rk: String },
    Person { sid: crate::plex::ServerId, key: String, guid: String, name: String, thumb: String },
    ItemMenu { sid: crate::plex::ServerId, rk: String },
    Tab(HomeTab),
    Account,
}

/// Bounded actions emitted by the owned Home page. Item identity is always server-scoped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HomeReq {
    Play { sid: crate::plex::ServerId, rk: String, resume_ns: i64 },
    Detail { sid: crate::plex::ServerId, rk: String },
    ItemMenu { sid: crate::plex::ServerId, rk: String },
    /// BACK from the shelves: fold to the hero and seat the engine in its remembered hero group.
    FoldToHero,
    Account,
    Tab(HomeTab),
}

/// Stable top-strip destinations; availability changes presentation, never identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HomeTab {
    Home,
    Movies,
    Shows,
    Search,
}

/// Addressed bootstrap/diagnostic intentions. They use the same owned step and focus engine as
/// remote input; application scripts never write a Home cursor or carousel global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HomeCmd {
    FocusGrid { row: usize, col: usize },
    Hero,
    FocusStrip(HomeTab),
    Flip(i32),
    SelectHero(i32),
    ItemMenu,
}

/// Bounded actions emitted by an owned Library instance. Every media action carries its server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LibraryReq {
    /// Evaluated after dispatch by the application, where the input owner can report a live arm.
    PublishShelves { target: crate::stores::browse::SectionAddress, hidden_page: bool, at_head: bool },
    Menu { kind: LibraryMenuKind, anchor: [u32; 4], target: crate::stores::browse::SectionAddress },
    Play { sid: crate::plex::ServerId, rk: String, resume_ns: i64 },
    Detail { sid: crate::plex::ServerId, rk: String },
    ItemMenu { sid: crate::plex::ServerId, rk: String, from_deck: bool },
    Account,
    Tab(HomeTab),
    BackToHome { kind: crate::browse::SecKind },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryMenuKind { Sort, Filter, Genre, Sources }

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LibraryMenuArg {
    pub host: crate::ui::machine::InstanceId,
    pub target: crate::stores::browse::SectionAddress,
    pub kind: LibraryMenuKind,
    /// Bit-preserving rest rectangle; valid in canonical arguments without float equality.
    pub anchor: [u32; 4],
}

impl crate::ui::machine::LogicalState for LibraryMenuArg {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        c.u32(self.host.0).u32(self.target.epoch).u32(u32::from(self.target.sid.raw()))
            .u64(self.target.section as u64).u32(match self.kind {
                LibraryMenuKind::Sort => 0, LibraryMenuKind::Filter => 1,
                LibraryMenuKind::Genre => 2, LibraryMenuKind::Sources => 3,
            });
        for value in self.anchor { c.u32(value); }
    }
    fn probe(&self, out: &mut String) { out.push_str("library_menu_arg"); }
}

/// **What the item context menu is about**, captured at the moment it is presented and never
/// re-resolved (`screens::item_menu`). [`LibraryMenuArg`] is the precedent: an anchored `Compact`
/// surface whose whole subject is decided by the press that opened it.
///
/// Six `static mut`s collapsed into this one value in restructure phase 10 — `ITEM` and `SID`
/// (what the menu is about), `OPENER` (where it hangs and what it lifts), and the `MenuHost` the
/// route carried, down to the two bits an action actually reads. Being an ARGUMENT rather than a
/// global is a stronger claim than it looks: a hub refetch cannot re-point the row under an open
/// panel, and two menus could not exist at once to fight over it.
#[derive(Clone)]
pub(crate) struct ItemMenuArg {
    /// The server every action's ratingKey names — see [`ItemMenuReq`].
    pub(crate) sid: crate::plex::ServerId,
    /// The item the menu is about. For a card it is the row's own key, repeated here so the
    /// identity questions (`same_instance`, the canonical hash) need not look inside the row.
    pub(crate) rk: String,
    pub(crate) kind: ItemMenuKind,
    /// The PAGE entry the menu hangs off: its focused element is the anchor the panel sits beside
    /// and the tile lifted back out of the modal dim (`app::bridge::redraw_opener`).
    pub(crate) host: crate::ui::machine::EntryId,
    /// …and which element that is, as the host page's own focus at the press frame. The surface
    /// takes input the moment it is presented, so the host's live cursor is not the answer.
    pub(crate) focus: Option<crate::ui::machine::FocusKey<u32>>,
    /// The focused tile's drawn rect, bit-preserving so a canonical argument needs no float
    /// equality — [`LibraryMenuArg::anchor`]'s rule. The presenter resolves the centred fallback
    /// (`item_menu::fallback_anchor`) before storing it, so this is always a real rect.
    pub(crate) anchor: [u32; 4],
    pub(crate) loaded_episode: bool,
    pub(crate) from_home: bool,
}

/// Which of the three menus this is, and the data its row set is built from.
///
/// No `Debug`, deliberately: `PmsMovie` is a wire DTO with none, and deriving one for it would put
/// a household's viewing on the far end of any `{:?}` — which `diag::scrub` cannot make safe,
/// because nothing distinguishes a title from an ordinary log word (`diag/scrub.rs`'s own rule, and
/// the tree-wide grep that enforces it).
#[derive(Clone)]
pub(crate) enum ItemMenuKind {
    /// A card row — a home shelf, the Library grid, a Search result shelf, a person's filmography,
    /// the detail page's Related shelf. `from_deck` is the SHELF's answer, not the item's: the
    /// deck-removal row exists only on Continue Watching.
    Card {
        /// Boxed because it is by far the largest thing an `AppArg` can carry, and every other
        /// variant of that enum would pay for it inline.
        row: Box<crate::pms::PmsMovie>,
        from_deck: bool,
    },
    /// The detail page's episode filmstrip. `mark` is resolved by the page through the same
    /// `ep_state` that draws the still's own state line, so the tile and the menu opened on it
    /// cannot describe one episode two ways.
    Episode { mark: crate::ui::widgets::PosterMark },
    /// The detail page's season tabs.
    Season { mark: crate::ui::widgets::PosterMark },
}

/// **Identity, not contents.** `PmsMovie` is a wire DTO with no `PartialEq` of its own, and one
/// derived over its forty-odd fields would be the wrong question anyway: two menus are the same
/// menu when they are about the same item on the same server in the same role, and a refreshed
/// copy of that row with a new `viewOffset` is still that menu. The container asks this through
/// `ScreenArg::same_instance`, which must never be able to think it is holding two of them.
impl PartialEq for ItemMenuKind {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Card { row: a, from_deck: da }, Self::Card { row: b, from_deck: db }) =>
                da == db && a.sid == b.sid && a.rk == b.rk && a.kind == b.kind,
            (Self::Episode { mark: a }, Self::Episode { mark: b }) => a == b,
            (Self::Season { mark: a }, Self::Season { mark: b }) => a == b,
            _ => false,
        }
    }
}
impl Eq for ItemMenuKind {}

impl PartialEq for ItemMenuArg {
    fn eq(&self, other: &Self) -> bool {
        self.sid == other.sid && self.rk == other.rk && self.kind == other.kind
            && self.host == other.host && self.focus == other.focus
            && self.anchor == other.anchor && self.loaded_episode == other.loaded_episode
            && self.from_home == other.from_home
    }
}
impl Eq for ItemMenuArg {}

impl crate::ui::machine::LogicalState for ItemMenuArg {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        c.u32(u32::from(self.sid.raw())).str(&self.rk);
        match &self.kind {
            ItemMenuKind::Card { row, from_deck } => { c.u32(0).bool(*from_deck).u32(row.kind as u32); }
            ItemMenuKind::Episode { mark } => { c.u32(1).u32(*mark as u32); }
            ItemMenuKind::Season { mark } => { c.u32(2).u32(*mark as u32); }
        }
        c.u32(self.host.0);
        c.option(self.focus, |c, key| { c.u32(key.entry.0).u32(key.elem); });
        for value in self.anchor { c.u32(value); }
        c.bool(self.loaded_episode).bool(self.from_home);
    }
    fn probe(&self, out: &mut String) { out.push_str("item_menu_arg"); }
}

/// Addressed simulator/harness intentions. They are resolved by the mounted instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryCmd {
    Enter(crate::browse::SecKind),
    #[cfg(test)]
    FocusGrid { row: usize, col: usize },
    Page(i32),
    Sweep,
    SwitchStep(u32),
    ItemMenu,
}

/// Stable section identity. PMS section keys are server-local, never globally unique.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct LibrarySectionIdentity {
    pub(crate) sid: crate::plex::ServerId,
    pub(crate) key: i64,
}

/// Stable identities for Library controls and repeated media. Removed identities remain in the
/// instance registry as tombstones so `KeyRegion` never guesses ownership from live membership.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LibraryIdentity {
    Library(LibrarySectionIdentity),
    Shelf {
        section: LibrarySectionIdentity,
        hub: String,
        sid: crate::plex::ServerId,
        rk: String,
    },
    ShelfSlot {
        section: LibrarySectionIdentity,
        hub: String,
        publication: u32,
        slot: u32,
    },
    Grid {
        section: LibrarySectionIdentity,
        sid: crate::plex::ServerId,
        rk: String,
    },
    GridSlot {
        section: LibrarySectionIdentity,
        query: u32,
        slot: u32,
    },
    Rail { section: LibrarySectionIdentity, label: String },
    Control { section: LibrarySectionIdentity, kind: String, key: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LibraryKey {
    pub(crate) identity: LibraryIdentity,
    pub(crate) elem: u32,
    /// Last published slot is recovery metadata, never an active cursor.
    pub(crate) last_group: u32,
    pub(crate) last_index: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LibraryMemory {
    pub(crate) epoch: Option<u32>,
    pub(crate) query: Option<u32>,
    pub(crate) grid_reset_pending: bool,
    pub(crate) viewports: Vec<LibraryViewport>,
    pub(crate) keys: Vec<LibraryKey>,
    pub(crate) next_elem: u32,
    pub(crate) section: Option<LibrarySectionIdentity>,
    pub(crate) scroll: f32,
    pub(crate) shelf_scroll: Vec<(String, f32)>,
}

/// Section-owned document geometry, deliberately without any current or remembered item.
#[derive(Clone, Debug)]
pub(crate) struct LibraryViewport {
    pub(crate) epoch: u32,
    pub(crate) section: LibrarySectionIdentity,
    pub(crate) scroll: f32,
    pub(crate) shelves: Vec<(String, f32)>,
}

impl crate::ui::machine::LogicalState for LibraryViewport {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        let Self { epoch, section, scroll, shelves } = self;
        c.u32(*epoch).u32(u32::from(section.sid.raw())).u64(section.key as u64).f32(*scroll);
        c.seq(shelves.len());
        for (id, x) in shelves { c.str(id).f32(*x); }
    }
    fn probe(&self, _: &mut String) {}
}

/// An item's or person's identity travels with the navigation entry, never in a screen global.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ContentArg {
    Detail { sid: crate::plex::ServerId, rk: String },
    Person { sid: crate::plex::ServerId, key: String, guid: String, name: String, thumb: String },
    Filmography { sid: crate::plex::ServerId, key: String },
}

impl crate::ui::machine::LogicalState for ContentArg {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        match self {
            Self::Detail { sid, rk } => { c.u32(0).u32(u32::from(sid.raw())).str(rk); }
            Self::Person { sid, key, guid, name, thumb } => { c.u32(1).u32(u32::from(sid.raw())).str(key).str(guid).str(name).str(thumb); }
            Self::Filmography { sid, key } => { c.u32(2).u32(u32::from(sid.raw())).str(key); }
        }
    }
    fn probe(&self, out: &mut String) { out.push_str("content_arg"); }
}

impl ContentArg {
    pub(crate) fn same_item(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Detail { sid: a, rk: x }, Self::Detail { sid: b, rk: y }) =>
                a == b && x == y,
            (Self::Person { sid: a, key: x, guid: g, .. },
             Self::Person { sid: b, key: y, guid: h, .. }) =>
                if !g.is_empty() && !h.is_empty() { g == h } else { a == b && x == y },
            (Self::Filmography { sid: a, key: x }, Self::Filmography { sid: b, key: y }) =>
                a == b && x == y,
            _ => false,
        }
    }
}

/// Application payload on the container's return state. Focus itself remains engine-owned.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DetailIdentity {
    /// A published placeholder with no server-side identity yet; never equal to a landed item.
    Slot(u32),
    Season { sid: crate::plex::ServerId, show: String, rk: String },
    Episode { sid: crate::plex::ServerId, rk: String, text: bool },
    Related { sid: crate::plex::ServerId, rk: String },
    Cast { sid: crate::plex::ServerId, key: String, guid: String, name: String, role: String },
}

#[derive(Clone, Debug)]
pub(crate) struct DetailKey {
    pub(crate) identity: DetailIdentity,
    pub(crate) elem: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DetailMemory {
    pub(crate) spot: crate::metadata::Spot,
    pub(crate) keys: Vec<DetailKey>,
    pub(crate) next_elem: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardIdentity {
    pub(crate) sid: crate::plex::ServerId,
    pub(crate) rk: String,
    pub(crate) elem: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PersonMemory {
    pub(crate) card_keys: Vec<CardIdentity>,
    pub(crate) next_card_elem: u32,
    pub(crate) header_marked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilmographyKey {
    pub(crate) department: String,
    /// None identifies the department tab; Some identifies a provider credit within it.
    pub(crate) catalog_id: Option<String>,
    pub(crate) elem: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FilmographyMemory {
    pub(crate) keys: Vec<FilmographyKey>,
    pub(crate) next_elem: u32,
    pub(crate) department: String,
    pub(crate) preview: Option<(String, String)>,
}

/// Owned provider identity used by Home's stable group and element registries.
///
/// A provider which publishes no identity receives an explicitly ephemeral identity scoped to
/// that publication generation. Neither its title nor its position is claimed as stable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HomeHubIdentity {
    ContinueWatching,
    Identifier { sid: crate::plex::ServerId, id: String },
    Key { sid: crate::plex::ServerId, key: String },
    Ephemeral { generation: u32, ordinal: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HomeGroupKey {
    pub(crate) identity: HomeHubIdentity,
    pub(crate) group: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HomeItemIdentity {
    Item { hub: HomeHubIdentity, sid: crate::plex::ServerId, rk: String },
    Slot { hub: HomeHubIdentity, generation: u32, ordinal: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HomeItemKey {
    pub(crate) identity: HomeItemIdentity,
    pub(crate) elem: u32,
    /// Last published slot for this item, used only if its identity disappears. This is
    /// per-item recovery metadata, not the engine's active or remembered focus.
    pub(crate) last_row: u32,
    pub(crate) last_col: u32,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct HomeMemory {
    pub(crate) groups: Vec<HomeGroupKey>,
    pub(crate) items: Vec<HomeItemKey>,
    pub(crate) next_group: u32,
    pub(crate) next_elem: u32,
    pub(crate) carousel: Option<(crate::plex::ServerId, String)>,
    pub(crate) strip_chosen: bool,
    pub(crate) scroll_y: f32,
    /// Stable group keys, not row ordinals: provider reorder must not transfer a viewport.
    pub(crate) row_scroll: Vec<(u32, f32)>,
}

#[derive(Clone, Debug, Default)]
pub(crate) enum PageMemory {
    #[default]
    None,
    Detail(DetailMemory),
    Person(PersonMemory),
    Filmography(FilmographyMemory),
    Home(HomeMemory),
    Library(LibraryMemory),
    Search(crate::screens::search::Memory),
}

impl crate::ui::machine::LogicalState for DetailIdentity {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        match self {
            Self::Slot(local) => { c.u32(4).u32(*local); }
            Self::Season { sid, show, rk } => { c.u32(0).u32(u32::from(sid.raw())).str(show).str(rk); }
            Self::Episode { sid, rk, text } => { c.u32(1).u32(u32::from(sid.raw())).str(rk).bool(*text); }
            Self::Related { sid, rk } => { c.u32(2).u32(u32::from(sid.raw())).str(rk); }
            Self::Cast { sid, key, guid, name, role } => { c.u32(3).u32(u32::from(sid.raw())).str(key).str(guid).str(name).str(role); }
        }
    }
    fn probe(&self, out: &mut String) { out.push_str("detail_identity"); }
}

impl crate::ui::machine::LogicalState for PageMemory {
    fn write(&self, c: &mut crate::ui::machine::Canon) {
        match self {
            Self::None => { c.u32(0); }
            Self::Detail(memory) => {
                c.u32(1).u32(memory.spot.section as u32).u32(memory.spot.col as u32).bool(memory.spot.ep_text);
                for col in memory.spot.saved_col { c.u32(col as u32); }
                c.option(memory.spot.season, |c, season| { c.u64(season as u64); });
                c.u32(memory.next_elem).seq(memory.keys.len());
                for key in &memory.keys { key.identity.write(c); c.u32(key.elem); }
            }
            Self::Person(memory) => {
                c.u32(2).u32(memory.next_card_elem).bool(memory.header_marked).seq(memory.card_keys.len());
                for key in &memory.card_keys { c.u32(u32::from(key.sid.raw())).str(&key.rk).u32(key.elem); }
            }
            Self::Filmography(memory) => {
                c.u32(3).u32(memory.next_elem).str(&memory.department).seq(memory.keys.len());
                for key in &memory.keys {
                    c.str(&key.department).option(key.catalog_id.as_ref(), |c, id| { c.str(id); }).u32(key.elem);
                }
                c.option(memory.preview.as_ref(), |c, (department, id)| { c.str(department).str(id); });
            }
            Self::Home(memory) => {
                c.u32(4).u32(memory.next_group).u32(memory.next_elem).seq(memory.groups.len());
                for key in &memory.groups {
                    write_home_hub(&key.identity, c);
                    c.u32(key.group);
                }
                c.seq(memory.items.len());
                for key in &memory.items {
                    match &key.identity {
                        HomeItemIdentity::Item { hub, sid, rk } => {
                            c.u32(0);
                            write_home_hub(hub, c);
                            c.u32(u32::from(sid.raw())).str(rk);
                        }
                        HomeItemIdentity::Slot { hub, generation, ordinal } => {
                            c.u32(1);
                            write_home_hub(hub, c);
                            c.u32(*generation).u32(*ordinal);
                        }
                    }
                    c.u32(key.elem).u32(key.last_row).u32(key.last_col);
                }
                c.option(memory.carousel.as_ref(), |c, (sid, rk)| {
                    c.u32(u32::from(sid.raw())).str(rk);
                });
                c.bool(memory.strip_chosen).f32(memory.scroll_y).seq(memory.row_scroll.len());
                for &(group, scroll) in &memory.row_scroll { c.u32(group).f32(scroll); }
            }
            Self::Library(memory) => {
                c.u32(5).u32(memory.next_elem).f32(memory.scroll);
                c.option(memory.section.as_ref(), |c, section| {
                    c.u32(u32::from(section.sid.raw())).u64(section.key as u64);
                });
                c.seq(memory.keys.len());
                for key in &memory.keys {
                    write_library_identity(&key.identity, c);
                    c.u32(key.elem).u32(key.last_group).u32(key.last_index);
                }
                c.seq(memory.shelf_scroll.len());
                for (hub, scroll) in &memory.shelf_scroll { c.str(hub).f32(*scroll); }
                c.option(memory.epoch, |c, epoch| { c.u32(epoch); });
                c.option(memory.query, |c, query| { c.u32(query); });
                c.bool(memory.grid_reset_pending);
                c.seq(memory.viewports.len());
                for viewport in &memory.viewports { viewport.write(c); }
            }
            Self::Search(memory) => { c.u32(6); memory.write(c); }
        }
    }
    fn probe(&self, out: &mut String) { out.push_str("page_memory"); }
}

fn write_library_section(section: &LibrarySectionIdentity, c: &mut crate::ui::machine::Canon) {
    c.u32(u32::from(section.sid.raw())).u64(section.key as u64);
}

fn write_library_identity(identity: &LibraryIdentity, c: &mut crate::ui::machine::Canon) {
    match identity {
        LibraryIdentity::Library(section) => { c.u32(0); write_library_section(section, c); }
        LibraryIdentity::Shelf { section, hub, sid, rk } => {
            c.u32(1); write_library_section(section, c); c.str(hub).u32(u32::from(sid.raw())).str(rk);
        }
        LibraryIdentity::ShelfSlot { section, hub, publication, slot } => {
            c.u32(2); write_library_section(section, c); c.str(hub).u32(*publication).u32(*slot);
        }
        LibraryIdentity::Grid { section, sid, rk } => {
            c.u32(3); write_library_section(section, c); c.u32(u32::from(sid.raw())).str(rk);
        }
        LibraryIdentity::GridSlot { section, query, slot } => {
            c.u32(4); write_library_section(section, c); c.u32(*query).u32(*slot);
        }
        LibraryIdentity::Rail { section, label } => {
            c.u32(5); write_library_section(section, c); c.str(label);
        }
        LibraryIdentity::Control { section, kind, key } => {
            c.u32(6); write_library_section(section, c); c.str(kind).str(key);
        }
    }
}

fn write_home_hub(hub: &HomeHubIdentity, c: &mut crate::ui::machine::Canon) {
    match hub {
        HomeHubIdentity::ContinueWatching => { c.u32(0); }
        HomeHubIdentity::Identifier { sid, id } => { c.u32(1).u32(u32::from(sid.raw())).str(id); }
        HomeHubIdentity::Key { sid, key } => { c.u32(2).u32(u32::from(sid.raw())).str(key); }
        HomeHubIdentity::Ephemeral { generation, ordinal } => { c.u32(3).u32(*generation).u32(*ordinal); }
    }
}

pub(crate) const PAGE_MEMORY_SHAPE: &str = "PageMemory{None,Detail:{spot:Spot{section:i32,col:i32,ep_text:bool,saved_col:[i32;6],season:Option<i64>},next_elem:u32,keys:[{identity:DetailIdentity{Season(sid:u32,show:str,rk:str),Episode(sid:u32,rk:str,text:bool),Related(sid:u32,rk:str),Cast(sid:u32,key:str,guid:str,name:str,role:str),Slot(u32)},elem:u32}]},Person:{next_card_elem:u32,header_marked:bool,card_keys:[{sid:ServerId,rk:String,elem:u32}]},Filmography:{next_elem:u32,department:String,keys:[{department:String,catalog_id:Option<String>,elem:u32}],preview:Option<(String,String)>},Home:{next_group:u32,next_elem:u32,groups:[{identity:HomeHubIdentity{ContinueWatching,Identifier{sid:ServerId,id:String},Key{sid:ServerId,key:String},Ephemeral{generation:u32,ordinal:u32}},group:u32}],items:[{identity:HomeItemIdentity{Item{hub:HomeHubIdentity,sid:ServerId,rk:String},Slot{hub:HomeHubIdentity,generation:u32,ordinal:u32}},elem:u32,last_row:u32,last_col:u32}],carousel:Option<(ServerId,String)>,strip_chosen:bool,scroll_y:f32,row_scroll:[(group:u32,scroll:f32)]},Library:{next_elem:u32,section:Option<{sid:ServerId,key:i64}>,scroll:f32,keys:[{identity:LibraryIdentity,elem:u32,last_group:u32,last_index:u32}],shelf_scroll:[(hub:String,scroll:f32)],epoch:Option<u32>,query:Option<u32>,grid_reset_pending:bool,viewports:[LibraryViewport{epoch:u32,section:{sid:u32,key:u64},scroll:f32,shelves:[(id:str,x:f32)]}]}}";

/// Effects cross the screen/loop boundary; screens do not poll one another's pending latches.
pub(crate) enum ContentReq {
    Push(ContentArg),
    Present(ContentArg),
    Back,
    Play { play: PlayIntent, resume_ns: i64 },
    ItemMenu,
    /// **Present one of the Detail page's own panels** on the container tree (spec §6.2's
    /// "page-owned panels"). The page names WHICH and supplies whatever the panel needs to place
    /// itself; the loop knows the style and the page's identity, so neither is on this request.
    Panel(ContentPanel),
}

/// **Which page-owned panel [`ContentReq::Panel`] asks for** — any content page's, not one
/// screen's: the Detail page's three and the Person page's biography sheet.
///
/// One variant per panel rather than one screen with an inner kind — unlike the player's four
/// overlays, which share a key ladder and a transport rule. These share nothing: two are read-only
/// `Style::Alert` sheets (one with a page cursor, one with none), one is a `Style::Compact` menu
/// anchored to a button whose OK navigates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ContentPanel {
    /// *Also available* (`screens::alt_sources`), anchored to the drawn rect of the pill that
    /// opened it — bit-preserving, so a canonical argument holds it without float equality.
    AltSources { anchor: [u32; 4] },
    /// *Track information* (`screens::tracks_panel`), opened at 1-based `page`. Every interactive
    /// opening passes 1; `/tmp/plxnative-tracks=<n>` is the only caller that does not, and it is
    /// what makes a headless capture of page 2 possible at all.
    Tracks { page: i32 },
    /// *About* (`screens::about_panel`), the footer card's synopsis read in full. It carries
    /// nothing: the sheet has no cursor to open at and describes the item that landed.
    About,
    /// The **Person** page's biography, in full (`screens::person_bio`) — the alert behind that
    /// page's `MORE` mark. It carries nothing and takes no subject: the sheet is about the person
    /// the person store holds, and this page has no `(sid, rk)` to give.
    Bio,
}

impl ContentPanel {
    /// **The surface a panel IS**: its container style and its argument, which is the whole of what
    /// presenting one needs beyond the host the loop already holds.
    ///
    /// Here rather than in `app/bridge::open_content_panel`, which is where the same `match` stood
    /// until the About panel joined it. The reason is §0's criterion 5 and nothing subtler: with the
    /// map in the bridge, adding a page-owned panel edited `app/bridge.rs` — so the "and nothing
    /// else" the criterion claims would have been false on its first proof, for a reason that has
    /// nothing to do with the loop. The bridge still owns the PRESENTING (the duplicate check, the
    /// `next_style` handshake, the `NavOp`); this owns which surface the page asked for.
    ///
    /// `subject` is the ITEM the host page is standing on, and it is an `Option` because a content
    /// page need not have one: the Person page is about a person, so it has no `(sid, rk)` to give
    /// and every panel that needs one is refused rather than presented against a neighbour's item.
    /// `None` back means "this page cannot offer that panel", which the caller drops.
    pub(crate) fn surface(
        self,
        host: crate::ui::machine::InstanceId,
        subject: Option<(crate::plex::ServerId, &str)>,
    ) -> Option<(crate::ui::containers::modal::Style, AppArg)> {
        use crate::ui::containers::modal::Style;
        Some(match self {
            Self::AltSources { anchor } => {
                let (sid, rk) = subject?;
                (
                    Style::Compact,
                    AppArg::AltSources(crate::screens::alt_sources::AltSourcesArg {
                        host,
                        sid,
                        rk: rk.to_string(),
                        anchor,
                    }),
                )
            }
            Self::Tracks { page } => (
                Style::Alert,
                AppArg::TracksPanel(crate::screens::tracks_panel::TracksPanelArg { page }),
            ),
            Self::About => (Style::Alert, AppArg::AboutPanel),
            Self::Bio => (Style::Alert, AppArg::PersonBio),
        })
    }
}

/// **The play a content page decided on, for the loop to start** (spec §2.2, phase 9).
///
/// `route::request_play` takes the playback session's `&mut`, and an owned screen is only ever
/// shown the frame's publication (`AppViews::session`) — so Detail names the item and the loop
/// performs the request, exactly as Home's [`HomeReq::Play`] already did. The loop also owns what
/// happens when the request is REFUSED (a PMS/native route transition still owns the reducer):
/// the page's navigation is skipped, which is what the page's own discarded `started` bool used
/// to decide.
pub(crate) enum PlayIntent {
    /// An item off this page's own metadata.
    Item {
        sid: crate::plex::ServerId,
        rk: String,
        part: String,
        vcodec: String,
        acodec: String,
        title: String,
        context: String,
    },
    /// The alternative source the page had selected (`route::request_play_movie`).
    Movie(&'static crate::pms::PmsMovie),
}

pub(crate) trait ContentLike: AppLike<Memory = PageMemory> {}
impl<H: AppLike<Memory = PageMemory>> ContentLike for H {}

/// A host that publishes Home's retained catalog view. The view is borrowed from the rig-owned
/// snapshot and is therefore valid for the complete step/draw query without per-frame cloning.
pub(crate) trait HomeLike: AppLike<Memory = PageMemory> + Sized {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> crate::pms::HubsView<'a>;
}

/// A host that publishes this frame's playback session (spec §2.3). The player's owned screens
/// read the Player machine's decisions through it; a change is asked for as an effect
/// (`AppFx::Player`, `ContentReq::Play`), because the publication is a copy and there is no `&mut`
/// on this path by construction.
pub(crate) trait PlayerLike: AppLike<Memory = PageMemory> + Sized {
    fn session<'a>(cx: &Cx<'a, Self>) -> &'a crate::route::PlaybackSession;
}

pub(crate) trait SearchLike: AppLike<Memory = PageMemory> + Sized {
    fn search<'a>(cx: &Cx<'a, Self>) -> crate::search::view::SearchView<'a>;
}

/// A host that publishes all three retained Library views captured at the frame split.
pub(crate) trait LibraryLike: AppLike<Memory = PageMemory> + Sized {
    fn listing<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::ListingView<'a>;
    fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a>;
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::HubsView<'a>;
}

/// The application's messages (spec §3.1).
pub(crate) enum AppMsg {
    Store(StoreCmd),
    StoreWork(StoreWork),
    HubsResult(crate::stores::hubs::HubsResult),
    Home(HomeCmd),
    Library(LibraryCmd),
    LibraryEdit { target: crate::stores::browse::SectionAddress, edit: crate::stores::browse::QueryEdit },
    LibrarySelect(crate::stores::browse::SectionAddress),
    DetailRestore { spot: crate::metadata::Spot, episode: Option<String> },
    /// The *Also available* surface committed a row: open that copy's own page. The SURFACE names
    /// the destination and the PAGE navigates, which is `LibraryMenu`'s shape (`LibrarySelect`) and
    /// what keeps "what a press means on the Detail page" in one place instead of two.
    AltSourceOpen(ContentArg),
}

/// What the consent machine is told (§2.3): a person's answer to both questions at once.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConsentCmd {
    Record { errors: bool, usage: bool },
}

/// What an owned screen asks the LEGACY LOOP to do, because the owner of that decision is not on
/// the dispatcher yet. Each names the phase that retires it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LoopReq {
    /// BACK at a root the platform owns (Home, the FIRST consent question): hand the screen to
    /// the television. Retires with the Navigation root rule (phase 12).
    BackAtRoot,
    /// Privacy & data → Delete all local data, confirmed: erase, sign out, land on sign-in.
    /// Retires when Session owns the sign-in (phase 6).
    DeleteAllLocalData,
    /// The first-run Favourites screen (`Route::Onboard`) finished: enter Home. Retires with the
    /// route enum (phase 12, after 6 puts Login/Profiles/Onboard on one stack).
    OnboardDone,
    /// The first-run Favourites screen's BACK: the profile picker. Same retirement.
    OnboardBack,
    /// **Phase 6.** BACK at the ROOT of the QR sign-in or the who's-watching picker — nothing of
    /// this app is behind either, exactly the case [`BackAtRoot`] names, but this one is NOT that
    /// variant, because it is not merely "hand the screen to the television": `auth::cancel()`
    /// gets to decide FIRST whether there is a stored session to fall back to (issues #16-#18's
    /// rule, carried forward verbatim from the legacy `key_onboarding`/`onboarding_back` ladder
    /// this replaces — see `app::input::login_or_profiles_root_back`, which the loop's request
    /// drain calls). The SCREEN answers the other half of the old ladder's job — `onboarding_back`
    /// used to take a `pin_pad_open` bool so the loop could tell a picker's own PIN keypad BACK
    /// apart from a real root press; since phase 6 that state lives on the screen's own focus
    /// engine, not a legacy static the loop can still read, so the screen now decides that part
    /// itself (closing its own pad and answering `Handled::Yes` with NO request, exactly as
    /// `screens::consent`'s Settings-mode BACK declines rather than asking the loop) and pushes
    /// this request only when it has decided the press really is its root. No payload: the two
    /// screens' root-press handling is identical bar one log word, which the loop derives from
    /// its own `Route` at the point it drains this.
    AuthBackAtRoot,
    /// **Phase 10, the profile menu's five rows.** `screens::account_menu` is a surface on the
    /// shared `ModalStack` and owns its own rows, cursor and dismissal — but not one of the five
    /// things a row DOES. Three call `crate::auth` and then flip `app.route` (a screen may not
    /// name `Route` at all, §2.1); one presents another surface, whose `Style` is the
    /// application's to choose and not a screen's (`Navigation::next_style`); and one reaches
    /// `crate::lab`. Each is therefore a request the loop performs, exactly as `LibraryReq` and
    /// `PlayerReq` are for their screens. They retire when Session owns the sign-in and the
    /// registry owns the style (phases 6/12's remainder).
    ///
    /// Every one of them ALSO gets a `Fx::Nav(NavOp::Dismiss)` from the screen in the same drain,
    /// which is the legacy `on_ok`'s own "close, then return the action" in the order a container
    /// can express it.
    AccountChangeProfile,
    AccountSignIn,
    AccountSignOut,
    /// PRESENTS the Settings surface over the same page, so the host route does not move.
    /// Reachable signed OUT as well: a person who cannot sign in has still received a copy of this
    /// software, and LG requires the privacy notice to be readable in the app rather than only on
    /// the store listing.
    AccountSettings,
    /// Lab builds only, and it changes no route: the tester stays where they were and the toast
    /// says what happened.
    AccountSendDiagnostics,
}

/// Any host that carries this bundle. The screens under `screens/` are written against it, so the
/// bridge's `AppHost` and the Settings surface's inner host both mount them unchanged.
pub(crate) trait AppLike: Host<Elem = u32, Fx = AppFx, Msg = AppMsg> {}
impl<H: Host<Elem = u32, Fx = AppFx, Msg = AppMsg>> AppLike for H {}

/// The heartbeat words an owned screen can name (§15.3's word table) — the same alphabet
/// `app::route_word`/`overlay_word` print, so the fps tier's `overlay=` selection cannot drift
/// from the screen that owns the frame.
///
/// **`LOGIN`/`PROFILES` are phase 6's addition, and they are `route=` words, not `overlay=` ones**
/// — the QR sign-in and the who's-watching picker are app-stack PAGES (`AppArg::Login` /
/// `AppArg::Profiles`, flat variants since phase 12 folded the page alphabet in here), never a
/// surface on the `ModalStack`, exactly as first-run Favourites was in 5b. They MUST stay the
/// literal strings `"login"`/`"profiles"`, and the reason changed shape in D1 rather than going
/// away: `app::words::route_word` prints the same two words for the same two arguments, and the
/// `debug_assert_eq!(word, route_word(route), …)` in `bridge::frame` that used to catch the two
/// drifting is GONE — with one route authority there is nothing to compare a mirror against, so
/// the heartbeat word simply IS the top screen's own `Screen::name`. The equality is now the
/// tables' to keep, and `app::words::heartbeat_word_tests` derives both rather than transcribing
/// either. `tests/run.py` selects fps samples by these words (`tests/manifest.json`'s `route`
/// field), so a changed spelling silently disarms a scene rather than failing anything visible.
pub(crate) mod word {
    pub(crate) const HOME: &str = "home";
    /// The profile menu (`screens::account_menu`). An `overlay=` word since phase 10 — it was a
    /// `route=` word (`Route::Account`) while the menu was a legacy popover with a route of its
    /// own, and `tests/manifest.json`'s `home-acct-glass` scene was re-keyed with it.
    pub(crate) const ACCOUNT: &str = "account";
    /// The item context menu (`screens::item_menu`). An `overlay=` word since phase 10 — it was a
    /// `route=` word (`Route::ItemMenu { over }`) while the menu was a legacy popover with a route
    /// of its own, and `tests/manifest.json`'s `item-menu` scene was re-keyed with it. The
    /// SPELLING is unchanged (`itemmenu`, one word, no separator), so a reader's grammar and every
    /// tool that greps for it are unchanged too.
    pub(crate) const ITEM_MENU: &str = "itemmenu";
    pub(crate) const PERSON: &str = "person";
    pub(crate) const SETTINGS: &str = "settings";
    pub(crate) const PRIVACY: &str = "privacy";
    pub(crate) const LEGAL: &str = "legal";
    pub(crate) const CONSENT: &str = "consent";
    pub(crate) const ONBOARD: &str = "onboard";
    /// The QR sign-in (`screens::login::LoginScreen`). Same spelling as `app::words::route_word`'s
    /// `AppArg::Login` arm — see this module's doc for why that equality is load-bearing.
    pub(crate) const LOGIN: &str = "login";
    /// The who's-watching picker (`screens::profiles::ProfilesScreen`). Same spelling as
    /// `app::words::route_word`'s `AppArg::Profiles` arm — see this module's doc.
    pub(crate) const PROFILES: &str = "profiles";
}

/// An element key for a route-family screen: table rows are their index; the action band's
/// controls sit above [`BAND`], so one `u32` namespace serves both groups of a screen.
///
/// **This number is repeated, not shared, and the repeat is `ui::table_screen::BAND_BASE`.**
/// `table_screen.rs` is a LIBRARY module and cannot name `screens::registry` (the layer rule:
/// `ui/` never names `screens/`), so the one place that actually MINTS a band element
/// (`BandPart::key`) carries its own copy of this literal with a comment pointing back here. The
/// assertion below is what keeps that a documented duplication rather than a silent one: if a
/// future edit moves this constant without moving its twin, `band_index`/`alert_index` would
/// misresolve every control in the family's action row (Privacy & data's Share/Don't Share, every
/// screen's Done/Try again) the next time anyone TYPED the mismatch, rather than the next time
/// anyone ran the app on a television.
pub(crate) const BAND: u32 = 0x4000_0000;
/// The decision alert's two answers (Cancel, Delete), above the band.
pub(crate) const ALERT: u32 = 0x4000_0100;

const _: () = assert!(
    BAND == crate::ui::table_screen::BAND_BASE,
    "screens::registry::BAND and ui::table_screen::BAND_BASE are the same address in two crates \
     that cannot import from each other; keep them numerically identical"
);

/// The inverse of [`band_index`]: where a family screen's Nth band control lives in the shared
/// `u32` namespace. Currently unused by any screen — every band element in the tree today is
/// minted by `table_screen::BandPart::key` (which carries its own copy of the same arithmetic,
/// for the layer reason on [`BAND`]'s doc) — kept here as the one place that STATES the forward
/// direction, and pinned by a round-trip test against `band_index` so the two cannot drift apart
/// silently if a future screen starts calling it directly instead of `BandPart`.
///
/// **`#[allow(dead_code)]` because its only callers are test modules** — this one's round-trip
/// test and `screens::onboard`'s, which mints the band key it presses through here rather than
/// writing `BAND + 0` out by hand. A `cfg(test)`-only caller is invisible to `dead_code` in the
/// build that ships, so `-D warnings` fails `--no-default-features` on it; the same reason
/// `screens::onboard::probe_fields` carries one. Deleting the function instead would delete the
/// only STATEMENT of the forward direction in this crate and leave `table_screen::BandPart::key`'s
/// copy of the arithmetic unpaired, which is the drift the const assert above exists to prevent.
#[allow(dead_code)]
pub(crate) fn band_elem(i: usize) -> u32 {
    BAND + i as u32
}
/// **`then`, not `then_some`, and the difference is a panic.** `bool::then_some` takes its value
/// by VALUE, so the subtraction is evaluated whatever the condition says — and every ordinary
/// table row is an `elem` far BELOW `BAND`, so `elem - BAND` underflows. The dev profile has
/// overflow checks on, so that is an outright panic on the commonest input this function has
/// ("attempt to subtract with overflow"), reached from any screen in the family that asks whether
/// the focused row is a band control. `then` takes a closure and so runs the arithmetic only on
/// the branch that already proved it cannot underflow.
///
/// A release build would not have panicked, which is what makes this worth a comment rather than
/// a silent edit: overflow wraps there, the guard is still `false`, and the function still answers
/// `None`. So the bug was invisible on the television and fatal in `make check` — the reverse of
/// the usual direction, and not something to re-derive from the diff.
pub(crate) fn band_index(elem: u32) -> Option<usize> {
    (elem >= BAND && elem < ALERT).then(|| (elem - BAND) as usize)
}
pub(crate) fn alert_index(elem: u32) -> Option<usize> {
    (elem >= ALERT && elem < ALERT + 2).then(|| (elem - ALERT) as usize)
}

// ── The modal REPEAT CADENCE ─────────────────────────────────────────────────────────────────
//
// **Here, and not in `screens/player/input.rs`, because it is not the player's** (phase 10 merge).
// It lived there while the player's four overlay panels were the only screens that paced their own
// `Edge::Repeat`; the item context menu became a surface in the same phase and needs the identical
// cadence, and a screen may not reach into a sibling family for a shared word (§2.1, and
// `ci/check-deps.sh`'s `sibling` gate). The `app/` side — `App::modal_repeat`, seeded in
// `app/boot.rs` — has always used it too, so "the player's input module" was never an honest home:
// the callers are one surface family, one standalone surface and the loop.

/// Rate-limits a REPEAT-DRIVEN discrete step — a forwarded hardware auto-repeat, or one tick of a
/// scroll-wheel gesture — to a couch-comfortable cadence, independent of the SOURCE's own cadence.
/// A held hardware key repeats roughly every 50ms; a wheel gesture can deliver several ticks in one
/// pass. Settings, Consent and Legal move a whole table row — or, inside a document, a full page of
/// reading text — per step, so letting either source drive `on_updown` at its own rate reads as a
/// blur rather than a scroll: item 13's whole ask.
///
/// Pure and host-testable — no `SDL_GetTicks` inside; `now` is threaded in by the caller, the same
/// shape `HeldKey`'s own `wrapping_sub` timing takes, so it survives the tick wrap the same way.
pub(crate) struct RepeatGate {
    /// The tick of the last step this gate admitted; `None` before the first one.
    pub(crate) last: Option<u32>,
}
impl RepeatGate {
    /// Minimum time between two repeat-driven steps this gate allows. Slower than the discrete
    /// focus-list repeat (110ms, [`PANEL_REPEAT_MS`]) on purpose — a home-grid card is a glance, a
    /// settings row or a line of reading text is not.
    pub(crate) const STEP_MS: u32 = 160;
    pub(crate) const IDLE: RepeatGate = RepeatGate { last: None };
    /// True at most once per [`Self::STEP_MS`]; always true the first call, or after a gap at
    /// least that long (which is also what makes a long-idle gate behave like a fresh one).
    pub(crate) fn ready(&mut self, now: u32) -> bool {
        self.ready_every(now, Self::STEP_MS)
    }
    /// The same gate at a caller-chosen cadence. The player's four overlay surfaces take
    /// [`PANEL_REPEAT_MS`] through this, which is what preserves the exact hold-to-move feel the
    /// loop's own client-side repeat timer gave them before phase 9 moved their input onto the
    /// dispatcher: the remote streams `Edge::Repeat` at roughly 50 ms, and a list walked at that
    /// rate reads as a blur (see [`PANEL_REPEAT_MS`]).
    pub(crate) fn ready_every(&mut self, now: u32, step_ms: u32) -> bool {
        let due = match self.last {
            None => true,
            Some(last) => now.wrapping_sub(last) >= step_ms,
        };
        if due {
            self.last = Some(now);
        }
        due
    }
    /// **A fresh press restarts the cadence from itself.** The press is acted on unconditionally
    /// by its own arm — a fresh press is never swallowed by the cadence of the press before it —
    /// and this records it as the step it is, so the FIRST hardware repeat of the new hold waits a
    /// full [`PANEL_REPEAT_MS`] rather than landing on top of it.
    ///
    /// It cleared `last` instead until the surface tests were written, which was wrong in exactly
    /// the direction that is invisible on a fresh gate: the remote streams `Edge::Repeat` at ~50 ms
    /// and the first of them arrives with `last: None`, so a held key stepped TWICE before the
    /// cadence engaged, once for the press and once for the repeat behind it.
    pub(crate) fn rearm(&mut self, now: u32) {
        self.last = Some(now);
    }
}

/// **The cadence a held direction walks a player panel's list at**, in ms.
///
/// 110 ms, which is the number `app/run.rs`'s client-side repeat timer used for exactly these four
/// panels (the track menu, the `…` popover, the Info card and the Chapters strip) while their keys
/// went through the loop's own ladder. That timer existed because the Magic Remote streams
/// hardware auto-repeat at ~50 ms and a list walked at that rate is unusable; phase 9 put the
/// panels' input on the dispatcher, where `Edge::Repeat` arrives at the hardware's rate, so the
/// cadence has to be applied by the surface that receives it. Same number, same feel, one owner.
pub(crate) const PANEL_REPEAT_MS: u32 = 110;

#[cfg(test)]
mod repeat_gate_tests {
    use super::{RepeatGate, PANEL_REPEAT_MS};

    #[test]
    fn a_gate_admits_the_first_step_then_holds_the_cadence() {
        let mut gate = RepeatGate::IDLE;
        assert!(gate.ready(1_000), "nothing has fired yet");
        assert!(!gate.ready(1_050), "too soon");
        assert!(!gate.ready(1_159), "still short of the step");
        assert!(gate.ready(1_160), "exactly one step later");
        assert!(!gate.ready(1_161));
    }

    /// SDL ticks wrap at 2^32ms; the same arithmetic `HeldKey`'s lost-keyup net and client-side
    /// repeat already rely on, so this gate must survive it the same way.
    #[test]
    fn the_gate_survives_the_tick_wrap() {
        let mut gate = RepeatGate::IDLE;
        let at = u32::MAX - 50;
        assert!(gate.ready(at));
        assert!(!gate.ready(at.wrapping_add(100)));
        assert!(gate.ready(at.wrapping_add(160)));
    }

    /// The player panels' own cadence, and the reason `ready_every` exists: the remote's ~50 ms
    /// hardware repeat is admitted at 110 ms, not at 160 (a settings row) and not at 50.
    #[test]
    fn a_player_panel_walks_a_held_direction_at_its_own_cadence() {
        let mut gate = RepeatGate::IDLE;
        assert!(gate.ready_every(1_000, PANEL_REPEAT_MS));
        assert!(!gate.ready_every(1_050, PANEL_REPEAT_MS), "the hardware's own rate is too fast");
        assert!(gate.ready_every(1_110, PANEL_REPEAT_MS));
        // …and a FRESH press restarts the cadence FROM ITSELF. The press is acted on by its own
        // arm without consulting the gate, so it is never swallowed; what this pins is the other
        // half, which was wrong until the surface tests caught it — the first hardware repeat of
        // the new hold must wait a full cadence rather than landing on top of the press.
        gate.rearm(1_115);
        assert!(!gate.ready_every(1_165, PANEL_REPEAT_MS), "the repeat behind a fresh press waits");
        assert!(gate.ready_every(1_225, PANEL_REPEAT_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row index stays itself under `band_elem`/`band_index` for the whole practical range of
    /// an action row (never more than two controls in this family today, but the round trip is
    /// asserted well past that so a widened band does not silently wrap into the alert's space).
    #[test]
    fn band_elem_and_band_index_round_trip() {
        for i in 0..64usize {
            let e = band_elem(i);
            assert!(e >= BAND && e < ALERT, "band_elem({i}) = {e:#x} left the band's own range");
            assert_eq!(band_index(e), Some(i));
        }
    }

    /// A raw table-row index (always far below [`BAND`]) is never mistaken for a band or alert
    /// control — the three ranges the family's `u32` namespace is carved into must not overlap.
    #[test]
    fn a_table_row_index_is_neither_a_band_nor_an_alert_element() {
        for row in [0u32, 1, 2, 41, 4095] {
            assert_eq!(band_index(row), None, "row {row} must not resolve as a band control");
            assert_eq!(alert_index(row), None, "row {row} must not resolve as an alert answer");
        }
    }

    /// The alert's two answers (Cancel, Delete) are the only two elements in its range, and the
    /// band's own top control does not spill into it.
    #[test]
    fn the_alert_range_holds_exactly_two_answers_just_above_the_band() {
        assert_eq!(alert_index(ALERT), Some(0));
        assert_eq!(alert_index(ALERT + 1), Some(1));
        assert_eq!(alert_index(ALERT + 2), None, "the alert's range is exactly two elements wide");
        assert_eq!(band_index(ALERT - 1), Some((ALERT - 1 - BAND) as usize), "the band's range runs right up to the alert's");
        assert_eq!(band_index(ALERT), None, "…and stops there — the two ranges must not overlap");
    }
}

// ---------------------------------------------------------------------------------------------
// the page alphabet, the screen argument and the one mount match
// ---------------------------------------------------------------------------------------------

// (The nine-value page alphabet `Route`, plus `route_wears_tab_bar` and `page_word`, stood here — the PAGE alphabet, a second
// enum whose nine values `AppArg::Legacy` wrapped. **Phase 12 folded it in rather than renaming
// it** (spec §14: "`Route` survives only as its argument"; §15.2: the enum is gone): the seven
// values that could actually mount are flat `AppArg` variants below, and the two that could not
// (`Detail`, `Person`) are gone entirely, because a page with an ITEM IDENTITY has always mounted
// from `AppArg::Content` and the `Legacy(Detail | Person)` arm of the mounter was a
// `debug_assert!(false)` nothing constructed.
//
// What the fold removed, beyond one enum: the two-level `match` every chrome, identity and mount
// question was written over; `route_wears_tab_bar`, whose only in-registry caller was
// [`AppArg::chrome`] and which is that function's body now; `page_word`, a second nine-word table
// beside `Screen::name`; and `AppArg::route()`, the "which of the two spellings is this page"
// resolution that every reader in `app/` had to perform first.)

/// **A screen argument: everything the container needs to mount one screen, and nothing else.**
///
/// Three families in one enum, distinguished by nothing but which variant it is: the seven flat
/// PAGES the application stack can hold, the CONTENT pages that carry an item identity, and the
/// surfaces a `ModalStack` presents. `AppArg::Legacy(Route)` and the nine-value page alphabet it
/// wrapped were folded in here in phase 12 (§15.2: the enum is gone) — see the note above.
///
/// **Both owned variants carry the page their inner stack is ROOTED at**, which is there for the
/// dev boot targets and for nothing else. `/tmp/plxnative-settings=privacy` has to put a headless
/// run on a page that is normally two presses inside the surface, and the loop cannot press them:
/// the Settings root's row indices are `RootPage`'s private business (the Favourites row is absent
/// signed out), so a loop that reached the child by delivering `Activate(<row>)` would be encoding
/// a table it does not own and would rot the first time a row is added. Rooting the stack at the
/// target instead needs nothing from the page. The one thing it costs is that BACK at a
/// dev-booted child DISMISSES the surface rather than revealing the root — a difference that
/// exists only under a trigger, and that the fps scenes it serves (`legal-document`,
/// `decision-alert`, `settings-*`) never press.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum AppArg {
    LibraryMenu(LibraryMenuArg),
    /// The profile menu (`screens::account_menu`), presented on the shared `ModalStack` over
    /// whichever bar-wearing page the chip was pressed on. **It carries nothing**, and that is the
    /// whole of what `Route::Account { over: BarHost }` was for: the host is the page the surface
    /// was presented over, which the container already knows and does not replace, so there is
    /// neither a host to name nor a destination to "close back to".
    AccountMenu,
    /// The item context menu (`screens::item_menu`), presented on the shared `ModalStack` over
    /// whichever card surface the hold happened on. **Everything the popover is about travels on
    /// the argument** — the row, its server, the anchor it hangs beside, and the two bits an
    /// action reads — which is what six `static mut`s and `Route::ItemMenu { over: MenuHost }`
    /// were between them doing. There is no host to NAME because the host is the top page, which
    /// the container already knows and does not replace.
    ItemMenu(ItemMenuArg),
    /// One of the player's four panels, presented on the PLAYER PAGE's own `ModalStack` (§6.2).
    /// It carries only which panel: the host is the page it is presented over, which the container
    /// already knows, and the panel's own state is the instance's.
    PlayerOverlay(crate::screens::player::overlay::PlayerOverlayArg),
    /// The Detail page's *Also available* picker (§6.2's page-owned panels). Its ANCHOR — the drawn
    /// rect of the pill it hangs off — is on the argument, `LibraryMenuArg`'s shape and for the
    /// same reason: the entry outlives any one frame's idea of where that pill was.
    AltSources(crate::screens::alt_sources::AltSourcesArg),
    /// The Detail page's *Track information* sheet (§6.2's page-owned panels). It carries only the
    /// 1-based PAGE the sheet opens at — a boot address like `Settings`'s root, not an identity —
    /// because the sheet describes `metadata::current()` and the host it is presented over is the
    /// container's own knowledge.
    TracksPanel(crate::screens::tracks_panel::TracksPanelArg),
    /// The Detail page's *About* sheet (§6.2's page-owned panels). **It carries nothing**, like
    /// [`Self::AccountMenu`] and for the same reason twice over: the sheet describes
    /// `metadata::current()`, it has no cursor to be opened at, and the host it is presented over
    /// is the container's own knowledge.
    AboutPanel,
    /// The Person page's biography sheet (§6.2's page-owned panels). **It carries nothing**, for
    /// `AboutPanel`'s reasons: the sheet describes `person::current()`, it opens at the top of the
    /// prose every time, and the host it is presented over is the container's own knowledge.
    PersonBio,
    /// plex.tv sign-in (QR) — shown when there is no usable session.
    Login,
    /// The "who's watching" Plex Home picker.
    Profiles,
    /// **"Which libraries do you want?"** — the *Favorite libraries* screen, the third and last
    /// onboarding page and the only one that is not about credentials: which of the granted
    /// libraries this profile wants, asked once PER PROFILE and only when the roster holds more
    /// than one. Favourites fill Home's shelves, decide which type pills the top strip draws at
    /// all, and scope the Library's own Sources picker; the grant is untouched, and Search still
    /// reaches every granted library.
    Onboard,
    /// The Home shelves — the app's ROOT page, and the one page that is always there.
    Home,
    /// The Library browse grid. Its sort/filter/source panels are [`Self::LibraryMenu`] entries;
    /// WHICH library is the `browse` store's business, not this argument's, which is why it
    /// carries nothing (the grid is re-ENTERED, never re-queried).
    Library,
    /// The Search screen. A PEER of Home and the Library, not a stacking page: it is reached from
    /// the strip's last pill and BACK from it returns to Home. What it OPENS stacks; it does not.
    Search,
    /// Playback. Its four panels are NOT here — they are entries on the player page's own
    /// `ModalStack` ([`Self::PlayerOverlay`]), and the container owns which one is up.
    Player,
    /// A page with an ITEM IDENTITY — a detail page, a person page, a filmography. Two of these
    /// are two entries (`person → detail → person` is three), which is the whole reason the
    /// identity rides on the argument instead of being a variant of a page alphabet.
    Content(ContentArg),
    /// The Settings family, rooted at this page (`SettingsPage::Root` for every real opening).
    Settings(SettingsPage),
    /// The first-run consent question, rooted at this stage byte (0 for every real opening;
    /// `screens::consent`'s `STAGE_PRODUCT` for `/tmp/plxnative-consent=product`).
    FirstRunConsent(u8),
}

pub(crate) const ARG_SHAPE: &str = "AppArg{Login,Profiles,Onboard,Home,Library,Search,Player,Content:{Detail{sid:u32,rk:str},Person{sid:u32,key:str,guid:str,name:str,thumb:str},Filmography{sid:u32,key:str}},Settings:SettingsPage{Root,Favourites,Privacy,Legal,About,Document(u8),Preview(u8),ConsentStage(u8)},FirstRunConsent(u8),LibraryMenu{host:u32,target:{epoch:u32,sid:u32,section:u64},kind:u32,anchor:[u32;4]},\
     PlayerOverlay{Tracks(tab:i32),Info,Chapters,More(quality:bool)},\
     AltSources{host:u32,sid:u32,rk:str,anchor:[u32;4]},\
     TracksPanel{page:i32},AboutPanel,PersonBio,AccountMenu,\
     ItemMenu{sid:u32,rk:str,kind:{Card{from_deck:bool,type:u32},Episode{mark:u32},Season{mark:u32}},\
     host:u32,focus:Option<{entry:u32,elem:u32}>,anchor:[u32;4],loaded_episode:bool,from_home:bool}}";

impl LogicalState for AppArg {
    fn write(&self, c: &mut Canon) {
        match self {
            Self::LibraryMenu(arg) => { c.u32(4); arg.write(c); }
            Self::PlayerOverlay(arg) => { c.u32(5); arg.write(c); }
            Self::AltSources(arg) => { c.u32(6); arg.write(c); }
            Self::TracksPanel(arg) => { c.u32(7); arg.write(c); }
            // 10, after the two menus' 8 and 9: a canon tag is a surface's identity in a recorded
            // state, so a retired or reallocated one would make two surfaces indistinguishable in
            // a replay. They are allocated forward, exactly as `ScreenId` is.
            Self::AboutPanel => { c.u32(10); }
            Self::PersonBio => { c.u32(11); }
            // 8 and 9 rather than the 6/7 the profile and card menus carried on their own
            // branch: this canon tag is the surface's identity in a recorded state, so two
            // surfaces merged from two lanes may not share one. 4/5 are the library and player
            // menus; 6/7 are the two Detail panels above.
            Self::AccountMenu => { c.u32(8); }
            Self::ItemMenu(arg) => { c.u32(9); arg.write(c); }
            // The seven PAGE variants keep the canon bytes `Legacy(Route)` wrote — a `0` tag and
            // then the route's own — so a recording taken before the fold and one taken after are
            // byte-comparable at every page frame. Only the SHAPE STRING moved, which is what the
            // fixtures are re-recorded for.
            Self::Login => { c.u32(0).u32(0); }
            Self::Profiles => { c.u32(0).u32(1); }
            Self::Onboard => { c.u32(0).u32(2); }
            Self::Home => { c.u32(0).u32(3); }
            Self::Library => { c.u32(0).u32(6); }
            Self::Search => { c.u32(0).u32(9); }
            Self::Player => { c.u32(0).u32(10); }
            Self::Content(arg) => { c.u32(1); arg.write(c); }
            Self::Settings(page) => { c.u32(2); page.write(c); }
            Self::FirstRunConsent(stage) => { c.u32(3).u8(*stage); }
        }
    }
    fn probe(&self, out: &mut String) { out.push_str("app_arg"); }
}

impl crate::ui::screen::ScreenArg for AppArg {
    /// **Which pages draw the shared top tab bar** — `route_wears_tab_bar`'s body, in the one
    /// place that ever asked it. Exhaustive on purpose: a new screen must not be able to answer
    /// this by accident, because the page each surface stands on is what answers for the chrome
    /// under it. Detail and Person have no bar, which is what makes every transition to or from
    /// them fade the bar with the page.
    fn chrome(&self) -> Chrome {
        match self {
            AppArg::Home | AppArg::Library | AppArg::Search => Chrome::TabBar,
            AppArg::Login
            | AppArg::Profiles
            | AppArg::Onboard
            | AppArg::Player
            | AppArg::Content(_)
            | AppArg::Settings(_)
            | AppArg::FirstRunConsent(_)
            | AppArg::LibraryMenu(_)
            | AppArg::AccountMenu
            | AppArg::ItemMenu(_)
            | AppArg::PlayerOverlay(_)
            | AppArg::AltSources(_)
            | AppArg::TracksPanel(_)
            | AppArg::AboutPanel
            | AppArg::PersonBio => Chrome::None,
        }
    }
    fn id(&self) -> ScreenId {
        ScreenId(match self {
            AppArg::LibraryMenu(_) => 15,
            // One id for all four panels, exactly as `Settings(_)` collapses its root payload:
            // they are four kinds of ONE screen, and `same_instance` must never let the container
            // think it is holding two of them.
            AppArg::PlayerOverlay(_) => 16,
            AppArg::AltSources(_) => 17,
            AppArg::TracksPanel(_) => 18,
            // 21, not a vacated id: allocated forward, so a retired identity is never handed to
            // the screen that replaced it.
            AppArg::AboutPanel => 21,
            AppArg::PersonBio => 22,
            // 19 and 20, not the 5 and 6 `Route::Account`/`Route::ItemMenu` vacated when this
            // phase deleted them, and not the 17/18 these two carried on their own branch: ids
            // are allocated forward here so a retired identity is never handed to the screen
            // that replaced it, and so two lanes' surfaces cannot collide on one.
            AppArg::AccountMenu => 19,
            // One id for all three entry points (a card, an episode still, a season tab): they are
            // three row sets of ONE screen, and `same_instance` must never let the container think
            // it is holding two of them.
            AppArg::ItemMenu(_) => 20,
            // The seven page ids are the ones `Legacy(Route)` computed, unchanged: an id is a
            // screen's identity in a recorded state and in `same_instance`, so they are allocated
            // forward and a fold may not renumber them. 5 and 6 are vacant (the two popover routes
            // phase 10 deleted); 8 and 9 belong to Detail and Person, which mount from `Content`.
            AppArg::Login => 1,
            AppArg::Profiles => 2,
            AppArg::Onboard => 3,
            AppArg::Home => 4,
            AppArg::Library => 7,
            AppArg::Search => 10,
            AppArg::Player => 11,
            // The ROOT payload is a boot address, not an identity: one Settings surface and one
            // consent question, whichever page each happens to have been rooted at.
            AppArg::Settings(_) => 12,
            AppArg::FirstRunConsent(_) => 13,
            AppArg::Content(ContentArg::Detail { .. }) => 8,
            AppArg::Content(ContentArg::Person { .. }) => 9,
            AppArg::Content(ContentArg::Filmography { .. }) => 14,
        })
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        if let (Self::Content(a), Self::Content(b)) = (self, other) {
            return a.same_item(b);
        }
        if matches!(self, Self::Content(_)) || matches!(other, Self::Content(_)) {
            return false;
        }
        // **A player overlay's identity is its KIND, never the playback under it** (§16.9): the
        // panel is a surface on the player's own stack, so the page beneath it is not part of what
        // "the same instance" means here — and, the other way round, the PLAYER's own argument
        // carries no overlay at all any more, which is what makes a BACK out of the track menu
        // dismiss a surface instead of remounting the page (the reuse-vs-remount risk this rule
        // exists for).
        if let (Self::PlayerOverlay(a), Self::PlayerOverlay(b)) = (self, other) {
            return a.kind.slot() == b.kind.slot();
        }
        // …and the same reason `id` collapses the payload: `Settings(Root)` and
        // `Settings(Legal)` are the same SCREEN, so a container must never be able to think it
        // is holding two of them.
        <Self as crate::ui::screen::ScreenArg>::id(self) == <Self as crate::ui::screen::ScreenArg>::id(other)
    }
}

// ---------------------------------------------------------------------------------------------
// the mounter: the one match
// ---------------------------------------------------------------------------------------------

/// **What a detail page that is about to mount should be RESTORED to** — the one payload the
/// container's own `ReturnState` cannot supply, because the page has never been on this stack.
///
/// Its two users are the `/tmp/plxnative-detail` boot trigger (a hard cut onto a page nobody
/// navigated from) and a show opened ON A PARTICULAR SEASON, which is the mount a page argument
/// cannot express: an argument names a PAGE, and a season is a tab inside one. It was a
/// `ui::trail::Node` until phase 12, i.e. a whole history entry used as a carrier for its `Spot`.
#[derive(Clone)]
pub(crate) struct DetailSeed {
    pub(crate) sid: crate::plex::ServerId,
    pub(crate) rk: String,
    pub(crate) spot: crate::metadata::Spot,
}

#[derive(Default)]
pub(crate) struct AppMounter {
    pub(crate) seed: Option<DetailSeed>,
    /// **Where the NEXT player instance returns to** — the entry that was on top when the push was
    /// asked for, stamped at the press and consumed by the mount exactly as `player_hud_ms` is.
    ///
    /// A seed rather than something the mounter derives, for the reason the auto-advance rule
    /// needs: `play_up_next` starts a new item while the player is ALREADY mounted, so nothing is
    /// seeded and nothing is consumed — the origin the user actually came from survives however
    /// many episodes the chain runs for. That was `Origin::Unchanged` and a `set_origin` call;
    /// it is now the absence of a write.
    pub(crate) player_origin: Option<crate::screens::player::Origin>,
    pub(crate) library_kind: Option<crate::browse::SecKind>,
    /// How long the NEXT player instance pins its transport for, in ms — `HUD_LINGER_MS` for an
    /// ordinary start and `HUD_HEADLESS_MS` for a capture run. It is a seed rather than a constant
    /// because `start_playback` is what knows which, and because the deadline must be stamped from
    /// the instant the page MOUNTS: callers used to pass `last_input + HUD_LINGER_MS`, a timestamp
    /// taken before a blocking resolve, so a load longer than the 4.5 s linger expired the HUD
    /// before it was ever drawn and the user got a blank screen instead of a transport.
    pub(crate) player_hud_ms: Option<u32>,
}

/// **The one `mount` match** (spec §2.1) — the whole of what "add a screen" costs, in the module
/// that owns the alphabet it matches on.
///
/// **Generic over the host rather than written against `app::bridge::AppHost`**, for the layer
/// rule's sake and for one practical consequence of it: the screens are generic already, the
/// views they need arrive through the `*Like` accessors above, and nothing in this match wants a
/// concrete application type. So the mounter names no `app::` module and the bridge instantiates
/// it for its own host exactly as the dispatcher instantiates everything else.
impl<H> Mounter<H> for AppMounter
where
    H: crate::ui::machine::Host<Arg = AppArg> + HomeLike + LibraryLike + SearchLike + PlayerLike,
{
    fn mount(
        &mut self,
        id: InstanceId,
        arg: &AppArg,
        ret: &ReturnState<u32, PageMemory>,
        cx: &Cx<'_, H>,
        _fx: &mut Effects<'_, H>,
    ) -> Box<dyn Screen<H>> {
        let entry = match cx.owner {
            crate::ui::machine::InputOwner::Entry(e) => e,
            _ => EntryId(0),
        };
        match arg {
            AppArg::LibraryMenu(arg) => Box::new(crate::screens::library::menu::LibraryMenu::new(entry, arg.clone())),
            AppArg::AccountMenu => Box::new(crate::screens::account_menu::AccountMenuScreen::new(entry)),
            AppArg::ItemMenu(arg) => Box::new(crate::screens::item_menu::ItemMenuScreen::new(entry, arg.clone())),
            AppArg::PlayerOverlay(arg) => Box::new(
                crate::screens::player::overlay::PlayerOverlayScreen::new(H::session(cx), entry, arg.kind),
            ),
            AppArg::AltSources(arg) => Box::new(
                crate::screens::alt_sources::AltSourcesScreen::new(entry, arg.clone()),
            ),
            AppArg::TracksPanel(arg) => Box::new(
                crate::screens::tracks_panel::TracksPanelScreen::new(entry, *arg),
            ),
            AppArg::AboutPanel => Box::new(
                crate::screens::about_panel::AboutPanelScreen::new(entry),
            ),
            AppArg::PersonBio => Box::new(
                crate::screens::person_bio::PersonBioScreen::new(entry),
            ),
            AppArg::Content(ContentArg::Detail { sid, rk }) => {
                let mut page = crate::screens::detail::DetailScreen::new(entry, *sid, rk.clone());
                if let PageMemory::Detail(spot) = &ret.memory {
                    page.restore_memory(spot);
                } else if let Some(seed) = self.seed.take() {
                    if seed.sid == *sid && seed.rk == *rk { page.restore(&seed.spot); }
                }
                Box::new(page)
            }
            AppArg::Content(ContentArg::Person { sid, key, guid, name, thumb }) => {
                let mut page = crate::screens::person::PersonScreen::new(entry, *sid, key.clone(), guid.clone(), name.clone(), thumb.clone());
                if let PageMemory::Person(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            AppArg::Content(ContentArg::Filmography { sid, key }) => {
                let mut page = crate::screens::filmography::FilmographyScreen::new(entry, *sid, key.clone());
                if let PageMemory::Filmography(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            // the first-run Favourites screen is OWNED (§14: "retirement 5b Onboard"); the route
            // word stays the loop's while the loop still names the page
            AppArg::Onboard => Box::new(crate::screens::onboard::OnboardScreen::first_run(entry)),
            // Phase 6: the QR sign-in and the who's-watching picker are OWNED screens too, mounted
            // exactly the same way — the route word is still the loop's (`route_word`), and
            // naming the route is the whole of (re)mounting either: a fresh instance is built
            // every time `bridge::frame` follows a `Replace` onto one of them, which is what lets
            // every remaining `app::input`/`app::run` call site drop its own `enter()`-equivalent
            // reset (see `input::enter_profiles_from_onboard`'s doc for the same argument made
            // about `screens::onboard` in 5b).
            AppArg::Login => Box::new(crate::screens::login::LoginScreen::new(entry)),
            AppArg::Profiles => Box::new(crate::screens::profiles::ProfilesScreen::new(entry)),
            AppArg::Home => {
                let mut page = crate::screens::home::HomeScreen::new(entry, id);
                if let PageMemory::Home(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            AppArg::Library => {
                let kind = self.library_kind.or_else(|| H::directory(cx).current().map(|i| H::directory(cx).sections()[i].kind))
                    .unwrap_or(crate::browse::SecKind::Movie);
                let mut page = crate::screens::library::LibraryScreen::new(entry, id, kind);
                if let PageMemory::Library(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            AppArg::Search => {
                let mut page = crate::screens::search::SearchScreen::new(entry, id);
                if let PageMemory::Search(memory) = &ret.memory { page.restore(memory); }
                Box::new(page)
            }
            // Phase 9: the player is an OWNED screen — the instance that holds the HUD, the scrub
            // gesture, the held-key timer, the control row's springs and the Up Next countdown,
            // and that answers `RenderStrategy::VideoPlane`. Its transport is pinned from the
            // instant it mounts (`AppMounter::player_hud_ms`), never from the keypress that asked
            // for the playback.
            AppArg::Player => {
                let mut page = crate::screens::player::PlayerScreen::new(entry);
                // **Where BACK, Stop and EOS land** (§5.1). Captured at the MOUNT, from the page
                // that was on top when the push was asked for — never read live, because an
                // overlay opening on this page's own stack changes the input owner and must not
                // be mistaken for a new origin.
                page.origin = self.player_origin.take();
                page.hud.extend(cx.tick.ms, self.player_hud_ms.take().unwrap_or(crate::screens::player::input::HUD_LINGER_MS));
                page.publish();
                Box::new(page)
            }
            // **The `Legacy(Detail | Person)` arm stood here** — a `debug_assert!(false)` and a
            // fall back to Home for two `Route` values nothing could construct, because a page
            // with an ITEM IDENTITY has no room for one in a bare page name and mounts from
            // `AppArg::Content`. Phase 12's fold deleted the two values with the enum, so the
            // match is exhaustive over pages that can really mount and there is no unreachable
            // arm left to keep honest.
            AppArg::Settings(root) => Box::new(RouteSurface::new(entry, id, Family::Settings, *root)),
            AppArg::FirstRunConsent(stage) => Box::new(RouteSurface::new(
                entry,
                id,
                Family::FirstRunConsent,
                SettingsPage::ConsentStage(*stage),
            )),
        }
    }
}

/// **Every `AppArg` a SURFACE can be presented with, one per variant** — the domain the heartbeat's
/// ` overlay=` alphabet is derived over (`app::overlay_words`, restructure phase 10 item 4).
///
/// Exhaustive by the COMPILER, which is the whole reason it is written this way: the `match` below
/// names every variant of the enum, so adding one and forgetting it here does not compile. A word
/// that silently never joins the table is the failure this guards — `tests/manifest.json` selects
/// fps samples by these strings, and a scene keyed on a word the app cannot print fails on the
/// television as "only 0 post-warmup samples", which is indistinguishable from a real regression.
///
/// The PAGE variants (the seven flat pages and `Content`) are excluded on purpose and by name
/// rather than by a wildcard: a page's word is `route=`, and it is the top page's own
/// `Screen::name`. `Settings` and `FirstRunConsent` appear once each because their payload is
/// a boot ADDRESS rather than an identity (`ScreenArg::id` collapses it the same way), and the
/// family's inner stack is what decides its word at any moment — `RouteSurface::top_word`.
#[cfg(test)]
pub(crate) fn every_surface_arg() -> Vec<AppArg> {
    use crate::screens::player::overlay::{OverlayKind, PlayerOverlayArg};
    let args = vec![
        AppArg::LibraryMenu(LibraryMenuArg {
            host: crate::ui::machine::InstanceId(1),
            target: crate::stores::browse::SectionAddress {
                epoch: 0, sid: crate::plex::ServerId::UNSET, section: 0,
            },
            kind: LibraryMenuKind::Sort,
            anchor: [0; 4],
        }),
        AppArg::AccountMenu,
        AppArg::ItemMenu(ItemMenuArg {
            sid: crate::plex::ServerId::UNSET,
            rk: "1".into(),
            kind: ItemMenuKind::Card { row: Box::new(crate::pms::PmsMovie::default()), from_deck: false },
            host: EntryId(0),
            focus: None,
            anchor: [0; 4],
            loaded_episode: false,
            from_home: false,
        }),
        // The Detail page's own two panels. Presented over Home here like every other
        // non-player surface: `Screen::name` is a constant of the screen, so the word it
        // answers does not depend on which page it was opened from — and the alternative,
        // standing a real Detail page up first, would make this derivation depend on the
        // metadata store landing.
        AppArg::AltSources(crate::screens::alt_sources::AltSourcesArg {
            host: crate::ui::machine::InstanceId(1),
            sid: crate::plex::ServerId::UNSET,
            rk: "1".into(),
            anchor: [0; 4],
        }),
        AppArg::TracksPanel(crate::screens::tracks_panel::TracksPanelArg { page: 1 }),
        AppArg::AboutPanel,
        AppArg::PersonBio,
        // The four panels are ONE screen with four kinds, and each answers a different
        // `Screen::name` — so every kind is listed, not one representative.
        AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Tracks { tab: 0 } }),
        AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Info }),
        AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::Chapters }),
        AppArg::PlayerOverlay(PlayerOverlayArg { kind: OverlayKind::More { quality: false } }),
        AppArg::Settings(SettingsPage::Root),
        AppArg::FirstRunConsent(0),
    ];
    for a in &args {
        match a {
            AppArg::LibraryMenu(_)
            | AppArg::AccountMenu
            | AppArg::ItemMenu(_)
            | AppArg::AltSources(_)
            | AppArg::TracksPanel(_)
            | AppArg::AboutPanel
            | AppArg::PersonBio
            | AppArg::PlayerOverlay(_)
            | AppArg::Settings(_)
            | AppArg::FirstRunConsent(_) => {}
            // The eight PAGE variants. Named rather than swept into a `_`, so the exhaustiveness
            // above is real and a new SURFACE variant cannot land in a catch-all.
            AppArg::Login
            | AppArg::Profiles
            | AppArg::Onboard
            | AppArg::Home
            | AppArg::Library
            | AppArg::Search
            | AppArg::Player
            | AppArg::Content(_) => {
                panic!("a page argument is not a surface: its word is `route=`")
            }
        }
    }
    args
}
// ---------------------------------------------------------------------------------------------
// the shape inventory
// ---------------------------------------------------------------------------------------------

/// **Every SCREEN-side shape the recorder's `state_fp` hashes** (spec §5.4), as one array in the
/// module a new screen is added to.
///
/// It was a hand-written list inside `app/recorder.rs::state_fp` until restructure phase 10, which
/// made `app/recorder.rs` a file every new screen had to touch — and §0's criterion 5 is that a
/// conversion touches its own `screens/<name>.rs`, this module, `dev/scenarios.rs` and
/// `tests/manifest.json` and nothing else. The recorder still owns the shapes that are the LOOP's
/// (the press machine, the input state, the frame line, the container tree, the return state, the
/// PMS fixtures, the app's own init) and folds them in FRONT of these; what belongs to a screen is
/// declared here, beside the argument that mounts it.
///
/// **Order is part of the hash** (`ui::rec::state_fp` writes the sequence), so an entry is
/// APPENDED, or [`SCREEN_SHAPES_PIN`] moves for that reason alone.
pub(crate) const SCREEN_SHAPES: &[&str] = &[
    ARG_SHAPE,
    PAGE_MEMORY_SHAPE,
    crate::screens::search::Memory::SHAPE,
    crate::screens::search::SHAPE,
    crate::screens::home::SHAPE,
    crate::screens::library::SHAPE[0],
    crate::screens::library::SHAPE[1],
    crate::screens::library::SHAPE[2],
    crate::screens::library::SHAPE[3],
    crate::screens::library::SHAPE[4],
    crate::screens::library::SHAPE[5],
    crate::screens::library::SHAPE[6],
    crate::screens::library::SHAPE[7],
    crate::screens::library::menu::SHAPE[0],
    crate::screens::library::menu::SHAPE[1],
    crate::screens::account_menu::SHAPE,
    crate::screens::item_menu::SHAPE,
    crate::screens::detail::SHAPE,
    crate::screens::person::PersonScreen::SHAPE,
    crate::screens::filmography::FilmographyScreen::SHAPE,
    crate::screens::player::SHAPE,
    crate::screens::player::overlay::SHAPE,
    crate::screens::alt_sources::SHAPE[0],
    crate::screens::alt_sources::SHAPE[1],
    crate::screens::tracks_panel::SHAPE,
    crate::screens::about_panel::SHAPE,
    crate::screens::person_bio::SHAPE,
];

/// The pin over [`SCREEN_SHAPES`] — bump it in the same edit that adds an entry, and say why.
///
/// A committed replay fixture recorded against a different value cannot be LOADED at all, which is
/// the cost this exists to make visible rather than silent; `tools/plxnative-rec rerecord` is the
/// verb (`tests/fixtures/replay/README.md`). The APP-side half is pinned separately in
/// `app/recorder.rs`, over the shapes that are the loop's, so neither pin moves for the other's
/// reason.
///
/// **Phase 10, the Detail page's *About* sheet** (0x844a_099e_5f0e_46d6 → this): `ARG_SHAPE` gains
/// `AboutPanel`, `screens::about_panel::SHAPE` joins the array, and `screens::detail::SHAPE` loses
/// `about_panel_open:u8` in the same commit — which panel is up is the CONTAINER's record now
/// (`Navigation::write` writes every surface's argument, phase and instance hash) and a second
/// copy on the page would be two producers of one fact. A schema transition rather than a
/// rebaseline: a recording taken before it hashed the sheet as ONE BYTE on the page, so a replay
/// could not tell an About sheet from a Track sheet, nor either from the page's own byte going
/// stale.
///
/// **Phase 10, the Person page's biography sheet** (0x4e7f_3aa3_b666_b4b0 → this): `ARG_SHAPE`
/// gains `PersonBio` and `screens::person_bio::SHAPE` joins the array. Its PAGE is in that shape
/// deliberately — UP/DOWN there moves nothing else in the app, so a recording taken before this
/// graded the sheet opening and closing with a hole between, and its page cursor was a `static mut`
/// no `LogicalState` could see.
///
/// **Phase 12, the player's pointer drag** (0x2ba1_a831_5580_d0b1 → this): `screens::player::SHAPE`
/// gains `scrub.drag:bool`. It is not a new piece of state — it was `app::input::Pointer::drag`, a
/// field of the LOOP's pointer machine, which is precisely why a recording taken before this
/// could not see it: the recorder hashes screens and the loop's own shapes, and a scrub gesture
/// half-owned by each hashed as neither. PX-PLAYER moves the whole gesture onto `PlayerScreen`, so
/// "a pointer is dragging the bar" is now a field of the page whose preview it moves, and a replay
/// that diverges on it says so instead of showing a preview nobody recorded.
///
/// **Phase 12, the fold of `Route` into `AppArg`** (0xd7b2_a9a4_39f9_706f → this): `ARG_SHAPE`
/// loses its nested `Legacy:Route{…}` and gains the seven page names flat. **No recorded byte
/// moved** — `LogicalState::write` still emits the `0` tag and the same per-page tag it wrote as
/// `Legacy(Route)`, deliberately, so a page frame hashes identically before and after. What
/// changed is the SHAPE STRING, which is what a shape pin is for: the fixtures are re-recorded
/// because their header names the shape, not because their frames disagree.
///
/// `#[cfg(test)]` because the pin is an ASSERTION about the array above and never a value the
/// app reads — `state_fp()` hashes [`SCREEN_SHAPES`] itself.
#[cfg(test)]
const SCREEN_SHAPES_PIN: u64 = 0xb7cc_e355_9fd5_f0f9;

#[cfg(test)]
mod arg_tests {
    use super::*;
    use crate::ui::screen::ScreenArg as _;

    /// **The screen half of the recorder's shape pin** (§5.4), asserted here rather than in
    /// `app/recorder.rs` because this is the array a new screen joins — so the bump lands in the
    /// same file, and the same commit, as the entry that caused it.
    #[test]
    fn the_screen_shape_inventory_is_pinned() {
        assert_eq!(crate::ui::rec::state_fp(SCREEN_SHAPES), SCREEN_SHAPES_PIN);
    }

    /// **One `ScreenId` per surface VARIANT.** Ids are allocated forward here precisely so a
    /// retired identity is never handed to the screen that replaced it, and `same_instance` is
    /// derived from `id` — so two different variants sharing one would let the container hold a
    /// mounted instance of the wrong screen and believe it was reusing the right one.
    ///
    /// Two arguments of ONE variant sharing an id is the DELIBERATE case (the four player panels,
    /// the three item-menu entry points, both Settings roots), which is why the assertion is over
    /// `mem::discriminant` rather than over equality of the arguments themselves.
    #[test]
    fn two_different_surface_variants_never_share_a_screen_id() {
        use crate::ui::screen::ScreenArg;
        let args = every_surface_arg();
        for (i, a) in args.iter().enumerate() {
            for b in args.iter().skip(i + 1) {
                if a.id() == b.id() {
                    assert_eq!(
                        std::mem::discriminant(a),
                        std::mem::discriminant(b),
                        "two different surfaces share ScreenId({})",
                        a.id().0
                    );
                }
            }
        }
    }

    // ---- the bar-wearing alphabet -------------------------------------------------------------

    /// **The profile chip is offered on exactly the pages that wear the shared top bar.**
    ///
    /// This test used to be `the_profile_popover_stands_on_the_page_it_was_opened_from`, and it
    /// graded `Route::Account { over: BarHost }` through `page_of`: the chip is a stop on all
    /// three bar screens, so the route had to CARRY the page underneath or a press on the
    /// Library's chip would cut to Home under the panel and strand the user there on dismissal.
    ///
    /// The menu is a `ModalStack` surface since phase 10, so that whole class of bug is gone by
    /// construction — a surface is presented OVER the top page and never replaces it, which is
    /// why `Route::Account` and `BarHost` are both deleted. What survives of the old rule is the
    /// half `BarHost::of` answered: WHICH pages have a chip to press at all. It is derived from
    /// `ScreenArg::chrome` now (the chip is a control on that bar), so the two cannot drift —
    /// which is exactly what `BarHost::of`'s own hand-written three-route list could do.
    /// `app::bridge`'s `the_profile_menu_is_a_surface_over_the_page_whose_chip_was_pressed`
    /// grades the other half, on a real tree.
    ///
    /// It is asked of `ScreenArg::chrome` DIRECTLY, here, rather than of its one-line consumer
    /// (`app::input::wears_the_chip`, which is `chrome() == Chrome::TabBar` and nothing else):
    /// this file is where a page argument joins the alphabet, so the bar-wearing set and the
    /// screen that joins it land in the same commit. A `screens/` module may not name
    /// `crate::app::` at all (`ci/check-deps.sh`'s `layer` gate), which is the same boundary
    /// stated as a rule.
    #[test]
    fn the_profile_chip_is_offered_on_exactly_the_bar_wearing_pages() {
        for r in [AppArg::Home, AppArg::Library, AppArg::Search] {
            assert!((r.chrome() == Chrome::TabBar), "a bar-wearing page carries the chip");
        }
        for r in [
            AppArg::Content(crate::screens::registry::ContentArg::Detail {
                sid: crate::plex::ServerId::UNSET, rk: String::new(),
            }),
            AppArg::Content(crate::screens::registry::ContentArg::Person {
                sid: crate::plex::ServerId::UNSET, key: String::new(), guid: String::new(),
                name: String::new(), thumb: String::new(),
            }),
            AppArg::Login,
            AppArg::Profiles,
            AppArg::Onboard,
            AppArg::Player,
        ] {
            assert!(
                !(r.chrome() == Chrome::TabBar),
                "only the bar-wearing screens carry the profile chip"
            );
        }
    }
}
