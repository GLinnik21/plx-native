//! **The application bundle's SCREEN-SIDE half** (restructure spec §3.1, phase 5b): the effects a
//! screen may ask for (`AppFx`), the messages a machine receives (`AppMsg`), and the requests an
//! owned screen makes of the legacy LOOP (`LoopReq`) while the two coexist (§14).
//!
//! The concrete `Host` impl — the `Arg` enum that names every screen, the mounter's one `match` —
//! lives in `app/bridge.rs` and not here, for one reason the layer rule cannot argue with: its
//! `Arg` still carries the legacy `Route` (§14: "`Route` survives only as its argument"), and
//! `Route` is `app`-private. So the screens are GENERIC over any host that carries this bundle
//! ([`AppLike`]), and the bridge instantiates them for its `AppHost`; the Settings family
//! instantiates the same screens a second time for the surface's own inner stack
//! (`screens::settings::InnerHost`), which is how one `OnboardScreen` mounts twice (§6.2).
//!
//! `LoopReq` is a DEBT with a phase number on each variant: a request the loop performs because
//! the machine that should own it (Session, Player, Navigation over the app's real stack) is not
//! on the dispatcher yet. The bridge drains them after every dispatcher frame.

use crate::stores::{StoreCmd, StoreId, StoreWork};
use crate::ui::machine::{Cx, Host};

/// The application's effects (spec §3.1). `Store` since phase 4; `Consent` and `Loop` since 5b.
pub(crate) enum AppFx {
    /// A store command, executed as a `Deliver` to the store machine in the same drain.
    Store(StoreId, StoreCmd),
    /// Poll only the store work this visible route owns, after its read-only step returns.
    #[cfg_attr(not(test), allow(dead_code))] // Remove when Home is mounted in Phase 8.
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

/// Addressed simulator/harness intentions. They are resolved by the mounted instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryCmd {
    FocusGrid { row: usize, col: usize },
    FocusToolbar,
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
    pub(crate) keys: Vec<LibraryKey>,
    pub(crate) next_elem: u32,
    pub(crate) section: Option<LibrarySectionIdentity>,
    pub(crate) scroll: f32,
    pub(crate) shelf_scroll: Vec<(String, f32)>,
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
            }
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

pub(crate) const PAGE_MEMORY_SHAPE: &str = "PageMemory{None,Detail:{spot:Spot{section:i32,col:i32,ep_text:bool,saved_col:[i32;6],season:Option<i64>},next_elem:u32,keys:[{identity:DetailIdentity{Season(sid:u32,show:str,rk:str),Episode(sid:u32,rk:str,text:bool),Related(sid:u32,rk:str),Cast(sid:u32,key:str,guid:str,name:str,role:str),Slot(u32)},elem:u32}]},Person:{next_card_elem:u32,header_marked:bool,card_keys:[{sid:ServerId,rk:String,elem:u32}]},Filmography:{next_elem:u32,department:String,keys:[{department:String,catalog_id:Option<String>,elem:u32}],preview:Option<(String,String)>},Home:{next_group:u32,next_elem:u32,groups:[{identity:HomeHubIdentity{ContinueWatching,Identifier{sid:ServerId,id:String},Key{sid:ServerId,key:String},Ephemeral{generation:u32,ordinal:u32}},group:u32}],items:[{identity:HomeItemIdentity{Item{hub:HomeHubIdentity,sid:ServerId,rk:String},Slot{hub:HomeHubIdentity,generation:u32,ordinal:u32}},elem:u32,last_row:u32,last_col:u32}],carousel:Option<(ServerId,String)>,strip_chosen:bool,scroll_y:f32,row_scroll:[(group:u32,scroll:f32)]},Library:{next_elem:u32,section:Option<{sid:ServerId,key:i64}>,scroll:f32,keys:[{identity:LibraryIdentity,elem:u32,last_group:u32,last_index:u32}],shelf_scroll:[(hub:String,scroll:f32)]}}";

/// Effects cross the screen/loop boundary; screens do not poll one another's pending latches.
pub(crate) enum ContentReq {
    Push(ContentArg),
    Present(ContentArg),
    Back,
    Play { resume_ns: i64 },
    ItemMenu,
}

pub(crate) trait ContentLike: AppLike<Memory = PageMemory> {}
impl<H: AppLike<Memory = PageMemory>> ContentLike for H {}

/// A host that publishes Home's retained catalog view. The view is borrowed from the rig-owned
/// snapshot and is therefore valid for the complete step/draw query without per-frame cloning.
pub(crate) trait HomeLike: AppLike<Memory = PageMemory> + Sized {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> crate::pms::HubsView<'a>;
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
/// — the QR sign-in and the who's-watching picker are app-stack PAGES (`AppArg::Legacy(Route::…)`),
/// never a surface on the `ModalStack`, exactly as first-run Favourites was in 5b. They MUST stay
/// the literal strings `"login"`/`"profiles"`: `app::route_word` prints the same two words for the
/// same two routes, and `bridge::frame`'s `debug_assert_eq!(word, route_word(route), …)` is what
/// would catch the two drifting apart — `tests/run.py` selects fps samples by these words
/// (`tests/manifest.json`'s `route` field), so a changed spelling silently disarms a scene rather
/// than failing anything visible.
pub(crate) mod word {
    pub(crate) const HOME: &str = "home";
    pub(crate) const PERSON: &str = "person";
    pub(crate) const SETTINGS: &str = "settings";
    pub(crate) const PRIVACY: &str = "privacy";
    pub(crate) const LEGAL: &str = "legal";
    pub(crate) const CONSENT: &str = "consent";
    pub(crate) const ONBOARD: &str = "onboard";
    /// The QR sign-in (`screens::login::LoginScreen`). Same spelling as `app::route_word`'s
    /// `Route::Login` arm — see this module's doc for why that equality is load-bearing.
    pub(crate) const LOGIN: &str = "login";
    /// The who's-watching picker (`screens::profiles::ProfilesScreen`). Same spelling as
    /// `app::route_word`'s `Route::Profiles` arm — see this module's doc.
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
