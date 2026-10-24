//! Owned Home page for restructure phase 8.
//!
//! The instance owns carousel, snap and render motion. The input engine owns the only focus and
//! every group cursor; this module reads them through `Cx::focus` and never mirrors them. Catalog
//! reads come exclusively from the rig-retained `HubsView`, and stable provider/item identities
//! are interned into `HomeMemory` so reorder and eviction preserve meaning rather than position.
//! Shared top chrome is container-owned: Home declares the strip doors and handles semantic strip
//! activation, but neither draws the bar nor stores a bar cursor.
//! The container's first-`Cover`/last-`Uncover` lifecycle pauses hero auto-rotation beneath every
//! modal style and restarts its countdown when the page becomes active again.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::sync::Arc;

use plx_data::pms::{HeroRef, HubIdentity, HubRef, HubsView, PmsMovie};
use plx_data::stores::hubs::HubsCmd;
use plx_data::stores::{StoreCmd, StoreId, StoreWork};
use plx_ui::cards::{self as ui_cards, RowStyle};
use plx_ui::cards::{CardSource, SectionFrame, Shelf};
use plx_ui::consts::*;
use plx_ui::hstack::{Align, HStack, Item};
use plx_ui::frame::Budget;
use plx_ui::hero_logo::{self, HeroLogo, LogoRung};
use plx_ui::hold_hint::{HintInput, HoldHint};
use plx_ui::icons::Icon;
use plx_ui::label::{Label, VAlign};
use plx_ui::linked_heading::{self, LinkedHeading};
use plx_ui::landing_hero::{
    base_scrim_ramp, hero_stack, COL_W as HERO_COL_W,
    TEXT_BOTTOM as HERO_TEXT_BOTTOM,
};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputEvent,
    InputKind, InstanceId, Key, LogicalState, Machine, MachineId, Measure, Tick,
};
use plx_machine::present::{PresentEvent, Provenance};
use plx_ui::screen::{
    Activate, At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, Focusable,
    GroupKind, GroupSpec, Hover, Link, Placed, RenderStrategy, Screen, ScreenEvent, Seat, Step,
    Stop,
};
use plx_ui::text_view::TextView;
use plx_ui::theme;
use plx_ui::widgets::{
    AmbientWash, Art, Button, CircleButton, ControlPalette, CtlPop, PageDots, PageGround,
    StatusKind, StatusOverlay,
};
use plx_ui::{hero_alpha, on_axis, Env, Painter, Rect, Spring, View};

use super::clock_readout::ClockWatch;

/// A paging command for a hub, when the hub has a listing identity to page by.
fn hub_page_cmd(hub: &HubRef<'_>,
    make: impl FnOnce(plx_plex::plex::ServerId, String, String) -> HubsCmd) -> Option<HubsCmd> {
    match hub.identity {
        Some(HubIdentity::Identifier { sid, id, key }) => Some(make(sid, id.into(), key.into())),
        // The merged deck is every server's lane, so the ask names no server of its own: the store
        // reads whichever lanes the move needs. Any card's server fills the slot the command has.
        Some(HubIdentity::ContinueWatching) => hub.items.first()
            .map(|m| make(m.sid, "home.continue".into(), String::new())),
        _ => None,
    }
}
use super::plaintext_question::{self, AlertStep, Near, OfferWatch, PlaintextAlert};
use super::registry::{
    key_cap, tile_facts, unlive_excess, Compaction, EvictOrder, KEY_PRUNE_BATCH, AppFx, AppMsg, ContentArg, ContentReq, HomeCmd, HomeGroupKey, HomeHubIdentity, HomeItemIdentity, HomeItemKey, HomeLike,
    HomeMemory, HomeReq, HomeTab, LoopReq, PageMemory, RowDiff, RowSeats,
};

const HERO_GROUP: GroupId = GroupId(0);
const FIRST_HUB_GROUP: u32 = 0x100;
const HERO_PLAY_ELEM: u32 = 0;
const HERO_INFO_ELEM: u32 = 1;
/// The Watchlist bookmark disc (`plx_appkit::watchlist_disc`): present only for a hero with a
/// Watchlist-takeable title ([`PmsMovie::watchlist_subject`]), second in the row, after Play.
const HERO_WATCHLIST_ELEM: u32 = 2;
const FIRST_ITEM_ELEM: u32 = 0x1000;
/// A collection shelf's linked heading (`ui::linked_heading`) is keyed off its shelf's group: the
/// heading's group AND its one element are `HEADING_BASE | shelf group`. Both derive from the
/// interned hub group, so they survive reorder and page memory without a registry of their own;
/// item elements and hub groups stay below this bit.
const HEADING_BASE: u32 = 0x8000_0000;
/// The hero's control slots: Play, Info and the Watchlist disc. A SLOT is a control's identity
/// (`Located::Hero(slot)`, its pop spring); its POSITION in the row is [`HomeScreen::hero_slots`].
const HERO_NBTN: usize = 3;
/// "Connect without encryption?" over the failure read-out (`screens::plaintext_question`): its
/// group sits between the hero's and the first hub's, its two answers below the first item key.
const PLAINTEXT_GROUP: GroupId = GroupId(0xF0);
const PLAINTEXT_CANCEL_ELEM: u32 = 0x0F00;
const PLAINTEXT_CONNECT_ELEM: u32 = 0x0F01;

/// Parent/container semantic strip keys. Positions are deliberately not encoded here.
pub const STRIP_HOME_ELEM: u32 = plx_ui::dispatch::STRIP_BASE;
pub const STRIP_MOVIES_ELEM: u32 = plx_ui::dispatch::STRIP_BASE + 1;
pub const STRIP_SHOWS_ELEM: u32 = plx_ui::dispatch::STRIP_BASE + 2;
pub const STRIP_SEARCH_ELEM: u32 = plx_ui::dispatch::STRIP_BASE + 3;
pub const STRIP_ACCOUNT_ELEM: u32 = plx_ui::dispatch::STRIP_BASE + 4;
pub const STRIP_WATCHLIST_ELEM: u32 = plx_ui::dispatch::STRIP_BASE + 5;
pub const STRIP_LIVETV_ELEM: u32 = plx_ui::widgets::TAB_KEY_LIVETV;

const MAX_ITEMS: usize = plx_data::pms::MAX_SHELF_ITEMS;
/// **How much of a resting shelf's posters must stand above the bottom margin for the pointer to take
/// focus onto them**, from the poster's top (0..1). The page rests the focused shelf at one fixed
/// line (`rest_scroll`), so the shelf below it stands cut by the bottom margin by a few tens of px
/// and the shelf above it is off the top: a rule that wanted a shelf WHOLE on screen
/// left the pointer nothing it could take focus onto (a shelf cut by the TOP still never takes it,
/// so one passing under the pointer on its way off does not steal focus). The wheel moves the
/// focus shelf by shelf either way (`bridge::wheel_input`, an Up or Down key).
const POINTER_REACH: f32 = 0.5;

const HERO_FLIP_CD: f32 = 0.35;
const HERO_AUTO_S: f32 = 8.0;
/// How near its target (px) a shelf's horizontal glide and the page's vertical reveal have to be
/// before the focused tile counts as RESTED for the hold hint's dwell.
const HINT_REST_PX: f32 = 0.5;
const K_SLIDE: f32 = 130.0;
const HERO_SLIDE_REST_PX: f32 = 0.5;
/// How near its target the snap dive has to be, in snap units (0 = hero, 1 = grid), before this
/// page calls the dive over — **together with [`SNAP_REST_VEL`], never alone.** `K_SNAP` is
/// critically damped, so the dive spends its last dozen frames inside any position threshold worth
/// picking while the picture is still visibly moving. Read by the present gate.
const SNAP_REST_POS: f32 = 0.002;
/// The velocity half of [`SNAP_REST_POS`], in snap units per second — the term that sees the
/// critically damped tail the position test is blind to.
const SNAP_REST_VEL: f32 = 0.01;
const HERO_PREFETCH: usize = 1;
const HERO_ART_CULL: f32 = 0.996;
const HERO_WASH_W: [f32; 4] = [0.55, 0.55, 0.40, 0.40];

const HERO_ROW_Y: f32 = HERO_TEXT_BOTTOM + theme::space::MD;
const HERO_CTRL_D: f32 = StatusOverlay::CTRL_H;
const HERO_CTRL_GAP: f32 = plx_ui::widgets::CONTROL_GAP;
const HERO_PAGER_D: f32 = HERO_CTRL_D * plx_ui::widgets::DISC_ICON_RATIO;
const HERO_PAGER_BEARING: f32 =
    HERO_PAGER_D * plx_ui::icons::ink_x(plx_ui::icons::Icon::Chevron).0;
const HERO_PAGER_PAD: f32 = HERO_CTRL_GAP - HERO_PAGER_BEARING;

const HERO_META_R: f32 = 0.60 * SCR_W;
const META_FLOW_W: f32 = HERO_META_R - MARGIN_X;
const SOURCE_PAD: f32 = theme::space::XS;

/// Each row's `motion` bytes are its `Shelf`'s (`Shelf::write`), the L0 row's motion write unchanged
/// (`base_y` included), so the text, `SCREEN_SHAPES_PIN` and the replay anchors stay what they were;
/// the primitive's name is split only so the `cards` gate does not read a shape string as a use of it.
pub const SHAPE: &str = concat!("HomeScreen{snap_target:f32,snap:{pos:f32,vel:f32},hero_flip_cd:f32,hero_auto:f32,covered:bool,visible_activation:Option<u32>,cta_available:bool,strip_chosen:bool,next_group:u32,next_elem:u32,groups:[HomeGroupKey{identity:HomeHubIdentity,group:u32}],items:[HomeItemKey{identity:HomeItemIdentity,elem:u32,last_row:u32,last_col:u32}],carousel:Option<(sid:u32,rk:str)>,outgoing:Option<(sid:u32,rk:str)>,hero_slide:{pos:f32,vel:f32},hero_dir:f32,hero_pop:{sp:[Spring{pos:f32,vel:f32};3],focused:Option<u32>},projected_generation:Option<u32>,grid:{scroll_y:{pos:f32,vel:f32},scroll_target:f32,rows:[{identity:HomeHubIdentity,group:u32,elems:[u32],motion:Card", "Row{scale:[Spring;24],overflow:Spring,scroll_x:Spring,lift:Spring,band:Spring,focus:i32,base_y:f32}}]},restored_scroll:[(group:u32,scroll:f32)],restore_reveal:bool}");

/// Work a sync did, by kind: index entries inserted, whole-index rebuilds, `elem_at` writes, and
/// sweeps of the item key table.
#[cfg(test)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct SyncCost {
    index_inserts: usize,
    index_rebuilds: usize,
    elem_at_writes: usize,
    sweeps: usize,
}

#[derive(Clone)]
struct HubProjection {
    identity: HomeHubIdentity,
    group: GroupId,
    elems: Vec<u32>,
    /// The row is a descriptor: its `elems` are placeholder slots, minted when the ring released it.
    placeholders: bool,
    /// Where the row's linked heading leads: the collection a promoted `custom.collection.*` hub
    /// lists, classified once per publication. Every other row is unlinked and keeps its plain
    /// heading.
    link: Option<plx_plex::plex::collections::CollectionRef>,
    /// The row's title, which the link above was classified from.
    title: String,
    /// The cards the projection was made from (an `Arc` clone each, none for a descriptor): the row
    /// is the same row, and needs no projecting again, while the next publication holds the very
    /// same cards (`Arc::ptr_eq`) in the same order. A pointer, not a rating key: a card the
    /// server answered again is a new card even when it reads alike.
    cards: Vec<Arc<PmsMovie>>,
    /// The key table was edited from outside a projection (a restore, an adopted show) since this
    /// row was projected, so its `elems` and the places its keys remember may no longer be the
    /// table's: the row is projected again, whatever the next publication holds. Only
    /// [`HomeScreen::invalidate_projection`] sets it, and a projection is always built unset.
    stale: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Located {
    Hero(usize),
    Item(usize, usize),
    /// A collection shelf's linked heading, by row.
    Heading(usize),
}

impl Located {
    /// On the shelves (a card or a linked heading) rather than the hero.
    fn on_grid(self) -> bool {
        matches!(self, Self::Item(..) | Self::Heading(_))
    }
}

fn heading_group(shelf: GroupId) -> GroupId {
    GroupId(HEADING_BASE | shelf.0)
}

fn heading_elem(shelf: GroupId) -> u32 {
    HEADING_BASE | shelf.0
}

struct Grid {
    /// One shelf per published row: `shelves.len() == rows.len()` once `sync_catalog` returns.
    shelves: Vec<Shelf>,
    scroll_y: Spring,
    scroll_target: f32,
}

impl Grid {
    fn new() -> Self {
        Self {
            shelves: Vec::new(),
            scroll_y: Spring::at(0.0),
            scroll_target: 0.0,
        }
    }

    /// The y the layout gave row `row` (a stale row index reads a shelf at rest).
    fn base_y(&self, row: usize) -> f32 {
        self.shelves.get(row).map_or(0.0, Shelf::base_y)
    }

    /// How far the row's heading has risen to clear the focused tile.
    fn lift(&self, row: usize) -> f32 {
        self.shelves.get(row).map_or(0.0, Shelf::heading_lift)
    }

    fn under_band(&self, row: usize) -> f32 {
        self.shelves.get(row).map_or(0.0, Shelf::under_band)
    }

    fn scroll_x(&self, row: usize) -> f32 {
        self.shelves.get(row).map_or(0.0, Shelf::scroll)
    }
}

struct Backdrop {
    wash: AmbientWash,
    grid_target: [[f32; 4]; 4],
    art: Spring,
    outgoing_art: Spring,
    tex: (u32, f32, f32),
    outgoing_tex: (u32, f32, f32),
    keyed: Option<(plx_plex::plex::ServerId, String)>,
    /// The art `tex` was resolved from — `(server, path)` of the hero the page names now.
    bound_art: Option<(u16, String)>,
    /// The art that was showing when the billboard's hero was REPLACED rather than flipped to
    /// (the hero left the pool: a finished film, a refetched catalog — `reconcile_carousel`).
    /// It stays up as the `outgoing_*` layer, resolved by its path every frame (a draw probe, so
    /// it keeps its texture), while the new art is fetched and dissolves in over it, and is let go
    /// once that art is up. Without it the replacement re-keyed `art` over nothing: the bare wash,
    /// then the new picture fading in — the owner's "hero reload" (2026-10-16).
    held: Option<(u16, String)>,
    /// The hero art path the page names NOW when that art is wanted on screen and has no texture
    /// yet (`bind` sets it, `draw` counts it — `placeholder.rs`: a count belongs at the DRAW,
    /// never at the resolve, and `prefetch` resolves neighbours it never draws). Empty otherwise.
    pending_art: String,
    /// The tick (ms) of the last direction-key `Repeat` the page saw, `None` once that key is
    /// released. While it is fresh a key is being HELD along the page, and the wash keeps the
    /// colours it has instead of re-keying to every card the focus crosses ([`Self::held`]).
    repeat_ms: Option<u32>,
}

/// How long after the last direction-key `Repeat` the wash still counts the key as held, for the
/// case where the key-up never arrives (the dropped-key-up net `ui::dispatch` keeps for OK). The
/// panel repeats about every 100 ms, so a live hold refreshes this several times over.
const WASH_REPEAT_GRACE_MS: u32 = 300;

impl Backdrop {
    fn new() -> Self {
        Self {
            wash: AmbientWash::flat(theme::SURFACE_APP),
            grid_target: [theme::SURFACE_APP; 4],
            art: Spring::at(0.0),
            outgoing_art: Spring::at(0.0),
            tex: (0, 0.0, 0.0),
            outgoing_tex: (0, 0.0, 0.0),
            keyed: None,
            bound_art: None,
            held: None,
            pending_art: String::new(),
            repeat_ms: None,
        }
    }

    /// Read a direction key's edge off the input stream (every key reaches the page, handled or
    /// not): a `Repeat` marks the key as held, an `Up` releases it. A first `Down` marks nothing,
    /// so a single press still re-keys the wash at once.
    fn note_key(&mut self, key: Key, edge: Edge, ms: u32) {
        if !matches!(key, Key::Left | Key::Right | Key::Up | Key::Down) {
            return;
        }
        match edge {
            Edge::Repeat => self.repeat_ms = Some(ms),
            Edge::Up => self.repeat_ms = None,
            Edge::Down => {}
        }
    }

    /// Is a direction key being held right now? The chrome glass blurs the page under it and
    /// reuses that blur only while the draw arguments beneath it are unchanged; a wash re-keyed
    /// to every card of a held run changes them every frame wherever the wash is not the plain
    /// ground under the band (hero art or a ramp knee under it: about 6 ms of GPU a late frame on
    /// the TV; over a plain wash the glass takes the ease at composite instead,
    /// `backdrop::Ground`). So the wash holds its target for the run and re-keys once the key
    /// rests.
    fn held(&self, ms: u32) -> bool {
        self.repeat_ms.is_some_and(|r| ms.wrapping_sub(r) <= WASH_REPEAT_GRACE_MS)
    }

    /// **Bind both art layers to the heroes the page names NOW**: resolve the selected and the
    /// outgoing backdrop, and re-key the reveal springs when the selection changed. Idempotent —
    /// a second call with the same heroes changes nothing — so it runs twice a frame: from
    /// [`Self::update`] on the `Tick`, and again from `draw_page` immediately before the draw.
    ///
    /// The second call is the correctness half. A MANUAL flip is the hero's at-edge Left/Right,
    /// which the dispatcher re-delivers to the page only once the focus engine reports the edge —
    /// BEHIND the frame's `Tick` (`ui::dispatch`, `Outcome::Edge(EdgeRule::Screen)`). Layers bound
    /// only on the tick were therefore the PREVIOUS pair on the press frame, drawn at the new
    /// pair's slide offsets: the x = 0 slot showed whatever the last tick had called outgoing —
    /// the hero two flips back while the previous slide was still settling, nothing (the bare
    /// wash) on a settled billboard — for exactly one frame (owner report 2026-09-30). The
    /// auto-advance flips inside `tick`, before `update`, and never showed it. Binding at the draw
    /// makes the layers a function of the state the draw reads, whatever event changed it.
    fn bind(
        &mut self,
        hero: Option<HeroRef<'_>>,
        outgoing: Option<HeroRef<'_>>,
        selected: Option<&(plx_plex::plex::ServerId, String)>,
        snap: f32,
    ) {
        let resolve = |h: Option<HeroRef<'_>>| {
            h.map(|h| plx_ui::widgets::resolve_tex_wh_on(h.item.sid.raw(), &h.item.art, 1280, 720, 0))
                .unwrap_or((0, 0.0, 0.0))
        };
        let changed = match (self.keyed.as_ref(), selected) {
            (Some((a, x)), Some((b, y))) => a != b || x != y,
            (None, None) => false,
            _ => true,
        };
        let art_of = |h: Option<HeroRef<'_>>| h.map(|h| (h.item.sid.raw(), h.item.art.clone()));
        if changed {
            // A replacement in place (no slide carries an outgoing hero) holds the picture that was
            // showing beneath the new one; a flip, or a first selection, holds nothing.
            let showing = self.bound_art.take().filter(|_| reveal(self.tex.0, &self.art) > 0.01);
            self.held = showing.filter(|art| {
                outgoing.is_none() && self.keyed.is_some() && Some(art) != art_of(hero).as_ref()
            });
        }
        self.bound_art = art_of(hero);
        if snap < HERO_ART_CULL {
            self.tex = resolve(hero);
            self.outgoing_tex = match (outgoing, self.held.as_ref()) {
                (Some(_), _) | (None, None) => resolve(outgoing),
                (None, Some((sid, art))) => plx_ui::widgets::resolve_tex_wh_on(*sid, art, 1280, 720, 0),
            };
        } else {
            self.tex = (0, 0.0, 0.0);
            self.outgoing_tex = (0, 0.0, 0.0);
        }
        self.pending_art.clear();
        if let Some(h) = hero.filter(|h| snap < HERO_ART_CULL && self.tex.0 == 0 && !h.item.art.is_empty()) {
            self.pending_art.push_str(&h.item.art);
        }

        if changed {
            plx_gfx::gfx::control_ground_invalidate();
            self.outgoing_art.jump(self.art.pos);
            // Over held art the new picture dissolves in, resident or not; over nothing it is up
            // at once when it can be.
            self.art.jump(f32::from(self.tex.0 != 0 && self.held.is_none()));
            if self.keyed.is_none() {
                self.wash
                    .jump(wash_corners(hero.map(|h| h.item), self.grid_target, snap));
            }
            self.keyed = selected.cloned();
        }
    }

    fn update(
        &mut self,
        hero: Option<HeroRef<'_>>,
        outgoing: Option<HeroRef<'_>>,
        grid_item: Option<&PmsMovie>,
        selected: Option<&(plx_plex::plex::ServerId, String)>,
        snap: f32,
        dt: f32,
        ms: u32,
    ) {
        self.bind(hero, outgoing, selected, snap);
        // decorative hero-art reveal dissolve (same reason as the wash below)
        plx_machine::idle::decorative(|| {
            if self.tex.0 != 0 {
                self.art.step(1.0, AmbientWash::K, dt);
            }
            if self.outgoing_tex.0 != 0 {
                self.outgoing_art.step(1.0, AmbientWash::K, dt);
            }
        });
        // The held art is let go once the picture over it is up — or once it is no longer
        // resident, when there is nothing left to hold.
        if self.held.is_some()
            && (reveal(self.tex.0, &self.art) > 0.999 || (snap < HERO_ART_CULL && self.outgoing_tex.0 == 0))
        {
            self.held = None;
            self.outgoing_tex = (0, 0.0, 0.0);
        }
        if let Some(item) = grid_item.filter(|m| m.has_blur).filter(|_| !self.held(ms)) {
            self.grid_target = AmbientWash::keyed(item.blur, PageGround::CARD_W);
        }
        // decorative colour dissolve under the page — must not hold the page-freeze snapshot
        plx_machine::idle::decorative(|| {
            self.wash.step(
                wash_corners(hero.map(|h| h.item), self.grid_target, snap),
                AmbientWash::K,
                dt,
            )
        });
    }

    fn draw(&self, p: Painter, env: &Env, slide: Option<(f32, f32)>) {
        if !self.pending_art.is_empty() && env.hero_a > 0.01 && env.sp < HERO_ART_CULL {
            plx_ui::placeholder::note(p, plx_ui::placeholder::Reason::HomeBackdrop, &self.pending_art);
        }
        let incoming_a = reveal(self.tex.0, &self.art);
        let outgoing_a = reveal(self.outgoing_tex.0, &self.outgoing_art);
        // THE GROUND: the wash, the one art layer dissolving over it (the snap dive, or the art
        // fading in) and the atmospheric ramp over both, as ONE pass per pixel
        // (`AmbientWash::draw_ground`). A hero FLIP slides two art layers, which one pass cannot
        // carry, so there the art and the ramp stay layers of their own.
        // Art held beneath a replaced hero (`held`) is the un-sliding outgoing layer: the new art
        // dissolves in over it, so the two cannot ride the ground's single art layer.
        let held = slide.is_none() && self.outgoing_tex.0 != 0;
        let art_a = if held { incoming_a.max(outgoing_a) } else { incoming_a };
        let wash = !wash_hidden(env.sp, art_a, slide.map(|_| outgoing_a))
            && !self.wash.is_flat(theme::SURFACE_APP, AmbientWash::FLAT_EPS);
        let art_in_ground = wash && slide.is_none() && !held && env.sp < HERO_ART_CULL;
        let ramp = (env.hero_a > 0.01 && slide.is_none()).then(|| hero_ramp(env.hero_a));
        let ramp_in_ground = wash
            && self.wash.draw_ground(
                p,
                Rect::FULL,
                art_in_ground.then(|| hero_art(self.tex, env.sp, incoming_a)),
                ramp,
            );

        let folded = !wash
            && env.hero_a > 0.01
            && env.sp < 0.001
            && slide.is_none()
            && !held
            && self.tex.0 != 0
            && incoming_a > 0.01
            && plx_ui::widgets::hero_ground_armed();
        if folded {
            plx_ui::widgets::hero_ground(
                p,
                self.tex.0,
                art_rect(self.tex, env.sp, 0.0),
                incoming_a * (1.0 - env.sp),
                base_scrim_ramp(env.hero_a),
                env.hero_a,
            );
        }
        if !folded && !art_in_ground && env.sp < HERO_ART_CULL {
            if let Some((out_x, in_x)) = slide {
                backdrop_art(p, self.outgoing_tex, env.sp, out_x, outgoing_a);
                backdrop_art(p, self.tex, env.sp, in_x, incoming_a);
            } else {
                if held {
                    backdrop_art(p, self.outgoing_tex, env.sp, 0.0, outgoing_a);
                }
                backdrop_art(p, self.tex, env.sp, 0.0, incoming_a);
            }
        }
        if !folded && env.hero_a > 0.01 {
            if !ramp_in_ground {
                let [y0, knee, mid, foot] = base_scrim_ramp(env.hero_a);
                p.rect(
                    Rect::new(0.0, y0, SCR_W, knee - y0),
                    0.0,
                    theme::scrim(0.0),
                    theme::scrim(mid),
                    0.0,
                );
                p.rect(
                    Rect::new(0.0, knee, SCR_W, SCR_H - knee),
                    0.0,
                    theme::scrim(mid),
                    theme::scrim(foot),
                    0.0,
                );
            }
            plx_ui::widgets::hero_scrim(p, env.hero_a, false);
        }
    }
}

/// [`HomeScreen::carousel_anchor`]: the billboard hero's index in the pool and, for an episode, its
/// show on its server.
#[derive(Clone, PartialEq, Debug)]
struct CarouselAnchor {
    index: usize,
    show: Option<(plx_plex::plex::ServerId, String)>,
}

/// A card named by borrowed parts, so a lookup in the key table clones nothing.
#[derive(Clone, Copy)]
enum ItemRef<'a> {
    Item { sid: plx_plex::plex::ServerId, rk: &'a str },
    Slot { generation: u32, ordinal: u32 },
}

/// One hub's entries in the key table: positions in `HomeScreen::items`.
#[derive(Default)]
struct HubKeys {
    by_rk: HashMap<Arc<str>, Vec<(plx_plex::plex::ServerId, usize)>>,
    slots: HashMap<(u32, u32), usize>,
}

impl HubKeys {
    fn is_empty(&self) -> bool {
        self.by_rk.is_empty() && self.slots.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_rk.values().map(Vec::len).sum::<usize>() + self.slots.len()
    }
}

pub struct HomeScreen {
    entry: EntryId,
    instance: InstanceId,

    groups: Vec<HomeGroupKey>,
    /// Shared with `memory()`: an unchanged republication leaves it shared, and only a real
    /// write (a new key, a moved card, a prune) copies it. Mutate through `Arc::make_mut`.
    items: Arc<Vec<HomeItemKey>>,
    next_group: u32,
    next_elem: u32,
    rows: Vec<HubProjection>,
    /// `elem` -> `(row, col)` for every card in `rows`, so [`HomeScreen::locate`] is a lookup
    /// rather than a scan of every row. Derived in `sync_catalog`; not canon.
    elem_at: HashMap<u32, (u32, u32)>,
    /// `items` / `groups` looked up by identity, so keying a card is a hash probe rather than a
    /// scan of the whole table. Positions into the two `Vec`s; kept in step by `push_item` /
    /// `push_group`, the only places either grows, and followed IN PLACE when a prune compacts the
    /// tables (`registry::Compaction`: no key is cloned or re-hashed for the entries that stay).
    /// `rebuild_indexes` rebuilds both whole, for the rare edit that rewrites identities in place.
    /// Derived, not canon.
    item_index: HashMap<HomeHubIdentity, HubKeys>,
    group_index: HashMap<HomeHubIdentity, usize>,
    projected_generation: Option<u32>,
    /// The Continue Watching row's cards by element as the latest projection showed them (an
    /// `Arc` clone each, not the catalog), so the NEXT publication can fade out the ones it
    /// removes. Not canon.
    cw_cards: HashMap<u32, Arc<PmsMovie>>,
    /// The server offset of the window `cw_cards` was taken from: a rebind (`rebind_shows`) is a
    /// replacement inside one window, never a window that slid. Not canon.
    cw_offset: usize,
    /// Where the billboard's hero stood in the pool the page last projected, and its show — what
    /// [`HomeScreen::reconcile_carousel`] hands the billboard to when that hero leaves the pool.
    /// A note about the previous projection (as `cw_cards` is). Not canon.
    carousel_anchor: Option<CarouselAnchor>,
    /// Elements `rebind_shows` / `adopt_shows` handed on to the card that replaced their episode.
    /// The one reason a restored key may find its element already held (`restore`), spent there.
    /// Not canon.
    rebound: HashSet<u32>,
    /// The cards the Continue Watching row removed, by [`ui_cards::elem_key`], kept while its
    /// landing is pending (a later publication adds to them and cannot change one) and let go
    /// once nothing fades. Not canon.
    ghost_cards: HashMap<u64, (u32, Arc<PmsMovie>)>,
    restored_scroll: Vec<(u32, f32)>,
    restore_reveal: bool,
    /// The rows held in the store (`HubsCmd::Hold`): the command goes out only when the rows in
    /// view outgrow the band last sent (`ui_cards::HoldBand`). Derived from what is on screen; not
    /// canon.
    hold: ui_cards::HoldBand,
    /// Keys that have left the page since the key table was last swept (`prune_keys`). Derived; not
    /// canon.
    since_prune: usize,
    /// What the syncs have cost, counted so the tests can pin the cost to the CHANGE and not to the
    /// catalog. Not canon.
    #[cfg(test)]
    cost: SyncCost,

    /// Selected hero ITEM identity. It is data, not focus.
    carousel: Option<(plx_plex::plex::ServerId, String)>,
    outgoing: Option<(plx_plex::plex::ServerId, String)>,
    hero_flip_cd: f32,
    hero_slide: Spring,
    hero_dir: f32,
    /// Seconds until the carousel auto-advances, hashed as logical state (`SHAPE`'s
    /// `hero_auto:f32`). Deliberately still a raw per-frame decrement (phase 12 D4 did NOT move
    /// this onto `motion::Ramp`): it is hashed across three committed replay fixtures, and a
    /// `Ramp`'s absolute-`Tick.ms` math computes the same real quantity through a different float
    /// operation sequence that measurably diverges the hash. `tick`'s own comment explains why
    /// the timer deliberately reports no `Motion` while it counts.
    hero_auto: f32,
    /// `HomeCmd::PinHero` holds the billboard: the countdown above stops for the life of the page.
    /// A screenshot pin only, never set by the product, and not logical state (no replay sets it).
    hero_pinned: bool,
    /// The container says this page is covered. For modals, `Navigation` emits `Cover` for the
    /// first presentation and `Uncover` only when the last surface is dismissed, independent of
    /// whether that surface's host-update policy is Live or Frozen.
    covered: bool,

    snap_target: f32,
    /// Stable element still visible while the engine has already crossed the hero/grid door.
    /// This is a press landing policy, not current focus, and retires at the visual midpoint.
    visible_activation: Option<u32>,
    cta_available: bool,
    strip_chosen: bool,
    snap: Spring,
    status_ms: f32,
    /// The plaintext-only server the failure read-out speaks about, when discovery offers the
    /// question for one (`plex::grant::offers`) — not logical state: it is the grant table's.
    plaintext: OfferWatch,
    /// Why key mode cannot help (`net::keypin::blocked`), held between ticks so a frame's draw and
    /// its hit rects read ONE value — the reason line moves the action row.
    clock: ClockWatch,
    /// The question, asked from the read-out's *Connect*.
    plaintext_alert: PlaintextAlert,
    hero_pop: CtlPop<HERO_NBTN>,
    /// The bookmark disc's membership, flip and the title it belongs to: input state fed by the
    /// application, never hashed (Detail's disc is the same).
    disc: HeroDisc,
    /// The standing "Hold OK for options" capsule and its dwell/life schedule — a timer advanced by
    /// `Tick`, not logical state (it is not in `SHAPE`, the way `status_ms` is not).
    hold_hint: HoldHint,
    backdrop: Backdrop,
    grid: Grid,
}

impl HomeScreen {
    pub fn new(entry: EntryId, id: InstanceId) -> Self {
        Self {
            entry,
            instance: id,
            groups: Vec::new(),
            items: Arc::default(),
            next_group: FIRST_HUB_GROUP,
            next_elem: FIRST_ITEM_ELEM,
            rows: Vec::new(),
            elem_at: HashMap::new(),
            item_index: HashMap::new(),
            group_index: HashMap::new(),
            projected_generation: None,
            cw_cards: HashMap::new(),
            cw_offset: 0,
            rebound: HashSet::new(),
            ghost_cards: HashMap::new(),
            restored_scroll: Vec::new(),
            restore_reveal: false,
            hold: ui_cards::HoldBand::default(),
            since_prune: 0,
            #[cfg(test)]
            cost: SyncCost::default(),
            carousel: None,
            carousel_anchor: None,
            outgoing: None,
            hero_flip_cd: 0.0,
            hero_slide: Spring::at(1.0),
            hero_dir: 1.0,
            hero_auto: HERO_AUTO_S,
            hero_pinned: false,
            covered: false,
            snap_target: 0.0,
            visible_activation: None,
            cta_available: false,
            strip_chosen: false,
            snap: Spring::at(0.0),
            status_ms: 0.0,
            plaintext: OfferWatch::default(),
            clock: ClockWatch::default(),
            plaintext_alert: PlaintextAlert::new(PLAINTEXT_GROUP, PLAINTEXT_CANCEL_ELEM, PLAINTEXT_CONNECT_ELEM),
            hero_pop: CtlPop::new(),
            disc: HeroDisc::default(),
            hold_hint: HoldHint::new(),
            backdrop: Backdrop::new(),
            grid: Grid::new(),
        }
    }

    pub fn restore(&mut self, memory: &HomeMemory) {
        let mut taken_groups: HashSet<u32> = self.groups.iter().map(|k| k.group).collect();
        for saved in &memory.groups {
            if self.find_group(&saved.identity).is_none() {
                assert!(taken_groups.insert(saved.group), "restored Home group collision");
                self.push_group(saved.clone());
            }
        }
        let mut taken_elems: HashSet<u32> = self.items.iter().map(|k| k.elem).collect();
        for saved in memory.items.iter() {
            if let Some(index) = self.find_item(&saved.identity) {
                let existing = &self.items[index];
                if (existing.last_row, existing.last_col) != (saved.last_row, saved.last_col) {
                    let existing = &mut Arc::make_mut(&mut self.items)[index];
                    existing.last_row = saved.last_row;
                    existing.last_col = saved.last_col;
                }
            } else {
                if taken_elems.insert(saved.elem) {
                    self.push_item(saved.clone());
                } else if !self.rebound.contains(&saved.elem) {
                    // Not one a rebind handed on: the number is held by a fresh key for another
                    // card (this page numbered its cards from the current catalog, and the
                    // remembered number belongs to a card that has since left). The remembered
                    // cursor recovers through `reconcile`; the key itself is not kept.
                    plx_base::eventlog::log(&format!(
                        "home: restored element {} is held by another card; its key is dropped", saved.elem));
                }
            }
        }
        self.rebound.clear();
        self.next_group = self.next_group.max(memory.next_group).max(FIRST_HUB_GROUP);
        self.next_elem = self.next_elem.max(memory.next_elem).max(FIRST_ITEM_ELEM);
        self.carousel = memory.carousel.clone();
        self.strip_chosen = memory.strip_chosen;
        self.grid.scroll_y.jump(memory.scroll_y);
        self.grid.scroll_target = memory.scroll_y;
        self.restored_scroll = memory.row_scroll.clone();
        self.restore_reveal = true;
        // The saved places overwrote the ones the fresh page's keys remember, and `adopt_shows`
        // may have handed elements on: every row is projected again from the table as it stands.
        self.invalidate_projection();
    }

    /// Make the next [`sync_catalog`](Self::sync_catalog) project EVERY row again, whatever the
    /// publication holds. A row whose identity and cards are unchanged is otherwise kept whole,
    /// and a key table edited from outside a projection (a restore, an adopted show) would then
    /// disagree with the rows kept over it: a tile listing an element that now names another
    /// card, a key remembering the place of the instance it came from.
    fn invalidate_projection(&mut self) {
        self.projected_generation = None;
        for row in &mut self.rows {
            row.stale = true;
        }
    }

    fn hub_identity(view: HubsView<'_>, row: usize, hub: HubRef<'_>) -> HomeHubIdentity {
        match hub.identity {
            Some(HubIdentity::ContinueWatching) => HomeHubIdentity::ContinueWatching,
            Some(HubIdentity::Identifier { sid, id, key }) => HomeHubIdentity::Identifier {
                sid,
                id: id.into(),
                key: key.into(),
            },
            Some(HubIdentity::Key { sid, key }) => HomeHubIdentity::Key {
                sid,
                key: key.into(),
            },
            None => HomeHubIdentity::Ephemeral {
                generation: view.generation,
                ordinal: row as u32,
            },
        }
    }

    fn find_group(&self, identity: &HomeHubIdentity) -> Option<usize> {
        self.group_index.get(identity).copied()
    }

    fn push_group(&mut self, key: HomeGroupKey) {
        #[cfg(test)]
        { self.cost.index_inserts += 1; }
        Self::index_group(&mut self.group_index, &key, self.groups.len());
        self.groups.push(key);
    }

    /// First wins, as the scan this replaced did.
    fn index_group(index: &mut HashMap<HomeHubIdentity, usize>, key: &HomeGroupKey, at: usize) {
        index.entry(key.identity.clone()).or_insert(at);
    }

    fn group_for(&mut self, identity: &HomeHubIdentity) -> GroupId {
        if let Some(index) = self.find_group(identity) {
            return GroupId(self.groups[index].group);
        }
        let group = self.next_group.max(FIRST_HUB_GROUP);
        self.next_group = group
            .checked_add(1)
            .expect("Home group-key space exhausted");
        assert!(group < HEADING_BASE, "Home groups overlap the linked headings");
        self.push_group(HomeGroupKey {
            identity: identity.clone(),
            group,
        });
        GroupId(group)
    }

    /// Where `identity` sits in `items`. A probe by borrowed parts: no `String` is cloned.
    fn find_item(&self, identity: &HomeItemIdentity) -> Option<usize> {
        match identity {
            HomeItemIdentity::Item { hub, sid, rk } => self.item_slot(hub, ItemRef::Item { sid: *sid, rk }),
            HomeItemIdentity::Slot { hub, generation, ordinal } => {
                self.item_slot(hub, ItemRef::Slot { generation: *generation, ordinal: *ordinal })
            }
        }
    }

    fn item_slot(&self, hub: &HomeHubIdentity, item: ItemRef<'_>) -> Option<usize> {
        let keys = self.item_index.get(hub)?;
        match item {
            ItemRef::Item { sid, rk } => keys.by_rk.get(rk)?.iter().find(|(s, _)| *s == sid).map(|&(_, i)| i),
            ItemRef::Slot { generation, ordinal } => keys.slots.get(&(generation, ordinal)).copied(),
        }
    }

    fn push_item(&mut self, key: HomeItemKey) {
        #[cfg(test)]
        { self.cost.index_inserts += 1; }
        Self::index_item(&mut self.item_index, &key, self.items.len());
        Arc::make_mut(&mut self.items).push(key);
    }

    /// First wins, as the scan this replaced did.
    fn index_item(index: &mut HashMap<HomeHubIdentity, HubKeys>, key: &HomeItemKey, at: usize) {
        match &key.identity {
            HomeItemIdentity::Item { hub, sid, rk } => {
                let sids = index.entry(hub.clone()).or_default().by_rk.entry(rk.clone()).or_default();
                if !sids.iter().any(|(s, _)| s == sid) {
                    sids.push((*sid, at));
                }
            }
            HomeItemIdentity::Slot { hub, generation, ordinal } => {
                index.entry(hub.clone()).or_default().slots.entry((*generation, *ordinal)).or_insert(at);
            }
        }
    }

    /// Both indexes from the tables, after an edit that rewrote identities in place (a rebind of a
    /// finished episode's key). A prune does not come here: it follows the positions it moved.
    fn rebuild_indexes(&mut self) {
        #[cfg(test)]
        { self.cost.index_rebuilds += 1; self.cost.index_inserts += self.items.len() + self.groups.len(); }
        self.item_index.clear();
        self.group_index.clear();
        for (at, key) in self.items.iter().enumerate() {
            Self::index_item(&mut self.item_index, key, at);
        }
        for (at, key) in self.groups.iter().enumerate() {
            Self::index_group(&mut self.group_index, key, at);
        }
    }

    /// The position of `item`'s key, minting one if the card has none.
    fn item_registration(&mut self, hub: &HomeHubIdentity, item: ItemRef<'_>) -> usize {
        if let Some(index) = self.item_slot(hub, item) {
            return index;
        }
        let elem = self.next_elem.max(FIRST_ITEM_ELEM);
        self.next_elem = elem
            .checked_add(1)
            .expect("Home element-key space exhausted");
        assert!(elem < HEADING_BASE, "Home elements overlap the linked headings");
        let identity = match item {
            ItemRef::Item { sid, rk } => HomeItemIdentity::Item { hub: hub.clone(), sid, rk: rk.into() },
            ItemRef::Slot { generation, ordinal } => HomeItemIdentity::Slot { hub: hub.clone(), generation, ordinal },
        };
        self.push_item(HomeItemKey { identity, elem, last_row: 0, last_col: 0 });
        self.items.len() - 1
    }

    /// Record where `index`'s card sits and return its elem. The table is written only when the
    /// slot moved, so an unchanged republication leaves it shared with `memory()`.
    fn place_item(&mut self, index: usize, row: u32, col: u32) -> u32 {
        let key = &self.items[index];
        if (key.last_row, key.last_col) != (row, col) {
            let key = &mut Arc::make_mut(&mut self.items)[index];
            key.last_row = row;
            key.last_col = col;
        }
        self.items[index].elem
    }

    #[cfg(test)]
    fn elem_for(&mut self, identity: HomeItemIdentity) -> u32 {
        let (hub, item) = match &identity {
            HomeItemIdentity::Item { hub, sid, rk } => (hub, ItemRef::Item { sid: *sid, rk }),
            HomeItemIdentity::Slot { hub, generation, ordinal } => {
                (hub, ItemRef::Slot { generation: *generation, ordinal: *ordinal })
            }
        };
        let index = self.item_registration(hub, item);
        self.items[index].elem
    }

    /// Project the published catalog onto the page's rows, keys and shelves. **The cost follows the
    /// change, not the catalog**: the ring moves a row on or off its cards on every press of a long
    /// page, and a row whose identity, title and cards (or descriptor size) are the ones it was
    /// projected from keeps its projection, its shelf and its entries in `elem_at` untouched, so a
    /// landing that changes two of 170 rows registers and maps two rows. A row inserted or removed
    /// above the rest moves them, and then every row from there on is projected again (`last_row`
    /// is where a card's key remembers it sat).
    fn sync_catalog<H: HomeLike>(&mut self, cx: &Cx<'_, H>) {
        let view = H::hubs(cx);
        if self.projected_generation == Some(view.generation) {
            return;
        }
        let mut rows = Vec::with_capacity(view.hub_count());
        // Where a row was, by identity: at the same position first (the ring changes a row's
        // cards, not its place), then by the map of the old rows. The matching is the registry's,
        // shared with the Library's Recommended page.
        let mut seats: RowSeats<HubProjection, HomeHubIdentity> =
            RowSeats::new(std::mem::take(&mut self.rows), view.hub_count());
        for row in 0..view.hub_count() {
            let Some(hub) = view.hub(row) else { continue };
            let placeholders = !hub.items.is_empty() && hub.items.len() <= MAX_ITEMS && hub.items.iter().all(|item| item.rk.is_empty());
            let same_place = hub.identity.is_some()
                && seats.at(row).is_some_and(|old| old.identity.borrowed() == hub.identity);
            // A descriptor that is still a descriptor of the same size keeps the slots it has: its
            // cards are nowhere on screen or near it, so a ring move costs nothing per such row.
            // A row of cards keeps them while they are the same cards.
            if same_place && seats.at(row).is_some_and(|old|
                !old.stale && old.placeholders == placeholders && old.title == hub.title
                    && if placeholders {
                        old.elems.len() == hub.items.len()
                    } else {
                        old.cards.len() == hub.items.len().min(MAX_ITEMS)
                            && old.cards.iter().zip(hub.items).all(|(a, b)| Arc::ptr_eq(a, b))
                    }) {
                rows.push(seats.keep(row));
                continue;
            }
            let identity = Self::hub_identity(view, row, hub);
            let old_index = seats.locate(row, same_place, hub.identity.is_some().then_some(&identity),
                |old| old.identity.clone());
            let group = self.group_for(&identity);
            if identity == HomeHubIdentity::ContinueWatching && !placeholders {
                let old = old_index.and_then(|at| seats.at(at));
                self.rebind_shows(&identity, hub.items, hub.offset, old);
            }
            let reused = placeholders.then(|| old_index.and_then(|at| seats.at(at)))
                .flatten().filter(|old| !old.stale && old.placeholders && old.elems.len() == hub.items.len()).map(|old| old.elems.clone());
            let elems = match reused {
                Some(elems) => elems,
                None => {
                    let mut elems = Vec::with_capacity(hub.items.len().min(MAX_ITEMS));
                    for (col, item) in hub.items.iter().take(MAX_ITEMS).enumerate() {
                        let item_ref = if item.rk.is_empty() {
                            ItemRef::Slot { generation: view.generation, ordinal: col as u32 }
                        } else {
                            ItemRef::Item { sid: item.sid, rk: &item.rk }
                        };
                        let index = self.item_registration(&identity, item_ref);
                        elems.push(self.place_item(index, row as u32, col as u32));
                    }
                    elems
                }
            };
            // The publishing section, for an identifier whose own section segment does not parse:
            // a hub lists one section's items.
            let link = match hub.identity {
                Some(HubIdentity::Identifier { sid, id, key }) =>
                    plx_plex::plex::collections::promoted_collection_link(
                        sid, id, key, hub.title, hub.items.first().map_or(0, |item| item.sec)),
                _ => None,
            };
            seats.rebuilt(old_index);
            rows.push(HubProjection {
                identity,
                group,
                elems,
                placeholders,
                link,
                title: hub.title.to_owned(),
                cards: if placeholders { Vec::new() } else { hub.items.iter().take(MAX_ITEMS).cloned().collect() },
                stale: false,
            });
        }
        // `unclaimed` is the old rows that were not kept whole, by position, for the element map.
        // `same_rows`: every row is the old row at its own position, so nothing moved, the shelves
        // stand and the element map is patched for the rows projected again.
        let RowDiff { same_rows, matched, rebuilt: reprojected, unclaimed: left } = seats.finish();
        if !same_rows {
            let mut old_shelves: Vec<Option<Shelf>> = std::mem::take(&mut self.grid.shelves).into_iter().map(Some).collect();
            self.grid.shelves = Vec::with_capacity(rows.len());
            for (row, projection) in rows.iter().enumerate() {
                // a shelf stays with its identity, wherever the row went
                let kept = matched[row].and_then(|old| old_shelves.get_mut(old)?.take());
                let shelf = kept.unwrap_or_else(|| Shelf::new(self.entry, &RowStyle::HOME).cull_margin(GLOW_PAD)
                    .animate_landings(projection.identity == HomeHubIdentity::ContinueWatching));
                self.grid.shelves.push(shelf);
            }
        }
        if !self.restored_scroll.is_empty() {
            for (row, projection) in rows.iter().enumerate() {
                if let Some(&(_, scroll)) = self.restored_scroll.iter().find(|(group, _)| *group == projection.group.0) {
                    self.grid.shelves[row].restore_scroll(scroll, projection.elems.len());
                }
            }
        }
        debug_assert_eq!(self.grid.shelves.len(), rows.len());
        // A fresh shelf rests at `base_y = 0`, and this runs on events (`Mount`, `StoreChanged`,
        // `Uncover`) as well as on the `Tick` whose `update_grid` lays the grid out. A frame drawn
        // between the two showed the new rows at the top of the screen, over the billboard.
        self.layout_grid();
        let mut removed: HashSet<u32> = HashSet::new();
        // The patch is exact while every row is its own hub. A publication can carry one hub twice
        // (the store does not dedupe `(hubIdentifier, key)`): the two rows share their keys, the map
        // keeps the first row's place for a shared element, and a first row that changed while the
        // second was kept would take the element away from the second. Two rows of one group are
        // that case, and it maps the page again (a pass over the groups, no hashing of identities).
        if same_rows && (reprojected.is_empty() || Self::groups_are_distinct(&rows)) {
            // the changed rows only: forget what each held, map what it holds now
            for &row in &reprojected {
                if let Ok(found) = left.binary_search_by_key(&row, |(at, _)| *at) {
                    let old = &left[found].1;
                    for &elem in &old.elems {
                        if self.elem_at.get(&elem).is_some_and(|&(r, _)| r as usize == row) {
                            self.elem_at.remove(&elem);
                            removed.insert(elem);
                        }
                    }
                }
            }
            for &row in &reprojected {
                for (col, &elem) in rows[row].elems.iter().enumerate() {
                    // First row wins, as the scan this replaces did.
                    #[cfg(test)]
                    { self.cost.elem_at_writes += 1; }
                    self.elem_at.entry(elem).or_insert((row as u32, col as u32));
                }
            }
            removed.retain(|elem| !self.elem_at.contains_key(elem));
        } else {
            let was = std::mem::take(&mut self.elem_at);
            for (row, hub) in rows.iter().enumerate() {
                for (col, &elem) in hub.elems.iter().enumerate() {
                    #[cfg(test)]
                    { self.cost.elem_at_writes += 1; }
                    self.elem_at.entry(elem).or_insert((row as u32, col as u32));
                }
            }
            removed.extend(was.keys().filter(|elem| !self.elem_at.contains_key(elem)));
        }
        self.rows = rows;
        self.restored_scroll.retain(|(group, _)| {
            view.state != plx_data::pms::HubState::Ready
                && !self.rows.iter().any(|row| row.group.0 == *group)
        });
        self.projected_generation = Some(view.generation);
        self.stash_removed_continue_watching(view);
        self.prune_keys(&removed, view.generation, cx.focus.current.map(|key| key.elem), &cx.focus.remembered);
        self.reconcile_carousel(view);
    }

    /// Whether every row has a group of its own, which is to say every row is its own hub.
    fn groups_are_distinct(rows: &[HubProjection]) -> bool {
        let mut seen: HashSet<u32> = HashSet::with_capacity(rows.len());
        rows.iter().all(|row| seen.insert(row.group.0))
    }

    /// **Focus follows the show** (layer-1 view state, 3g). A show's Continue Watching tile is its
    /// episode, so when the episode is watched the server answers with the NEXT one under another
    /// rating key, and the key table would mint a new element for it: the old element leaves the
    /// page, focus clamps off it with a `FocusMoved`, and the row plays a remove and an insert for
    /// what the viewer sees as the same tile changing its art. Here the old element is INHERITED
    /// instead: a card of the previous projection that this window no longer holds hands its key
    /// to a card this window adds for the same show on the same server, so the element, the focus
    /// on it and its remembered cursor are all unchanged and only the art differs.
    ///
    /// Narrow on purpose. Only the Continue Watching row (the one that animates its landings);
    /// only episodes of a show (a movie has no show, so it keeps the clamp); the match includes
    /// the server (the same rating key and the same show on a friend's server are different
    /// cards); a card that is already keyed does not inherit (it is not new); one old key goes to
    /// one new card, pairs taken in column order (two tiles of one show); and never across a
    /// window that slid (`offset`), where a card leaving is paging, not a replacement.
    fn rebind_shows(
        &mut self,
        hub: &HomeHubIdentity,
        items: &[Arc<PmsMovie>],
        offset: usize,
        old: Option<&HubProjection>,
    ) {
        let Some(old) = old else { return };
        if self.cw_cards.is_empty() || self.cw_offset != offset {
            return;
        }
        let items = &items[..items.len().min(MAX_ITEMS)];
        let held: HashSet<(plx_plex::plex::ServerId, &str)> =
            items.iter().filter(|item| !item.rk.is_empty()).map(|item| (item.sid, item.rk.as_str())).collect();
        // Cards the previous projection showed that this window does not hold, in column order.
        let mut gone: Vec<(u32, &PmsMovie)> = old.elems.iter()
            .filter_map(|elem| Some((*elem, &**self.cw_cards.get(elem)?)))
            .filter(|(_, card)| show_of(card).is_some() && !held.contains(&(card.sid, card.rk.as_str())))
            .collect();
        let mut inherited: Vec<(u32, plx_plex::plex::ServerId, String)> = Vec::new();
        for item in items {
            let Some(show) = show_of(item) else { continue };
            if item.rk.is_empty() || self.item_slot(hub, ItemRef::Item { sid: item.sid, rk: &item.rk }).is_some() {
                continue;
            }
            let Some(at) = gone.iter().position(|(_, card)| card.sid == item.sid && show_of(card) == Some(show)) else { continue };
            let (elem, _) = gone.remove(at);
            inherited.push((elem, item.sid, item.rk.clone()));
        }
        if inherited.is_empty() {
            return;
        }
        let keys = Arc::make_mut(&mut self.items);
        for (elem, sid, rk) in inherited {
            if let Some(key) = keys.iter_mut().find(|key| key.elem == elem) {
                key.identity = HomeItemIdentity::Item { hub: hub.clone(), sid, rk: rk.into() };
                self.rebound.insert(elem);
            }
        }
        self.rebuild_indexes();
    }

    /// **The finished episode's focus goes to the show's next episode when it is already in the
    /// row** (owner rule: "the next-episode tile, else the same index"). A tile the row removed
    /// (a ghost while its landing plays) names an episode; if the Continue Watching row holds
    /// another card of the same show on the same server, that tile is where focus goes.
    /// `rebind_shows` covers the other case, where the next episode is new and inherits the element.
    fn next_episode_of(&self, gone: u32) -> Option<u32> {
        let (_, card) = self.ghost_cards.values().find(|(elem, _)| *elem == gone)?;
        let show = show_of(card)?;
        let row = self.rows.iter().find(|r| r.identity == HomeHubIdentity::ContinueWatching)?;
        row.elems.iter().copied().find(|elem| self.cw_cards.get(elem)
            .is_some_and(|held| held.sid == card.sid && show_of(held) == Some(show)))
    }

    /// The return half of [`Self::rebind_shows`]. A page that was left and comes back is mounted
    /// on the CURRENT catalog before its memory arrives, so a show whose next episode replaced the
    /// one it remembers (the hold on the return from the player lets that land first) holds a
    /// fresh element for the new card while the memory names the old. Hand the remembered element
    /// to the new card, so the tile, and the focus on it, are the same; a fresh key that held that
    /// element takes the displaced one's number (both are fresh). The rows were projected from the
    /// numbers before the swap, so a swap invalidates the projection
    /// ([`Self::invalidate_projection`]) and the next sync projects every row again from the keys. Same rules as `rebind_shows`: the same server and show, a card the
    /// row no longer holds, and not a window that slid.
    fn adopt_shows(&mut self, memory: &HomeMemory, view: HubsView<'_>) {
        if memory.cw_shows.is_empty() { return }
        let Some(hub) = self.rows.iter().position(|r| r.identity == HomeHubIdentity::ContinueWatching)
            .and_then(|row| self.hub(view, row)) else { return };
        if hub.offset != memory.cw_offset { return }
        let items = &hub.items[..hub.items.len().min(MAX_ITEMS)];
        let held: HashSet<(plx_plex::plex::ServerId, &str)> =
            items.iter().filter(|item| !item.rk.is_empty()).map(|item| (item.sid, item.rk.as_str())).collect();
        let mut gone: Vec<&(u32, plx_plex::plex::ServerId, String, String)> = memory.cw_shows.iter()
            .filter(|(_, sid, rk, _)| !held.contains(&(*sid, rk.as_str()))).collect();
        let keys = Arc::make_mut(&mut self.items);
        let mut changed = false;
        for item in items {
            let Some(show) = show_of(item) else { continue };
            if item.rk.is_empty() { continue }
            let named = |key: &HomeItemKey| matches!(&key.identity,
                HomeItemIdentity::Item { hub, sid, rk } if *hub == HomeHubIdentity::ContinueWatching && *sid == item.sid && **rk == *item.rk);
            if memory.items.iter().any(named) { continue }
            let Some(at) = gone.iter().position(|(_, sid, _, s)| *sid == item.sid && s == show) else { continue };
            let (elem, ..) = *gone.remove(at);
            let (Some(mine), other) = (keys.iter().position(named), keys.iter().position(|k| k.elem == elem)) else { continue };
            let displaced = keys[mine].elem;
            if let Some(other) = other { keys[other].elem = displaced; }
            keys[mine].elem = elem;
            self.rebound.insert(elem);
            changed = true;
        }
        // The elements were swapped between keys, not their identities or places: the indexes
        // stand, and the rows, which list the elements the keys held a moment ago, do not.
        if changed { self.invalidate_projection(); }
    }

    /// Keep the Continue Watching cards the projection just removed (by element, so a later
    /// publication cannot make a ghost show another card) and remember the row's cards for the next.
    fn stash_removed_continue_watching(&mut self, view: HubsView<'_>) {
        let now: HashMap<u32, Arc<PmsMovie>> = self.rows.iter().position(|r| r.identity == HomeHubIdentity::ContinueWatching)
            .and_then(|row| Some((self.hub(view, row)?, &self.rows[row])))
            .map(|(hub, row)| row.elems.iter().copied().zip(hub.items.iter().cloned()).collect())
            .unwrap_or_default();
        for (elem, movie) in std::mem::take(&mut self.cw_cards) {
            if !now.contains_key(&elem) {
                self.ghost_cards.insert(ui_cards::elem_key(&elem), (elem, movie));
            }
        }
        self.cw_cards = now;
        self.cw_offset = self.rows.iter().position(|r| r.identity == HomeHubIdentity::ContinueWatching)
            .and_then(|row| self.hub(view, row)).map_or(0, |hub| hub.offset);
    }

    /// Bound the key tables. A key is only needed while its card is shown, was just shown (so a
    /// focus on a card that rotated out can recover to its row), or is the focus. Volatile keys
    /// (a `Slot` item, an item of an `Ephemeral` hub) carry their generation and can never match
    /// again once their publication is gone, so those go in the next sweep, which runs once
    /// [`KEY_PRUNE_BATCH`] keys have left the page (closing the gaps moves every key behind them,
    /// so a sync that changed a row or two does not pay for it) or the table is over its bound;
    /// stable keys stay until the table outgrows twice the shown cards plus 256, then the oldest
    /// go (elems are minted in order).
    /// An `Ephemeral` group of an older generation is never projected again. A remembered cursor
    /// older than what is retained recovers to the first row (`reconcile`). A Home whose hubs are
    /// stable and whose cards carry rating keys, under the cap, drops nothing.
    ///
    /// Each row's remembered card (`remembered`, the engine's cursor per group) is kept whether or
    /// not the row holds cards now: a row the ring released is a row of placeholders, and the card
    /// the user left in it must still be the card they return to. One key per row.
    fn prune_keys(&mut self, removed: &HashSet<u32>, generation: u32, focus: Option<u32>,
        remembered: &[(GroupId, u32)]) {
        let now = &self.elem_at;
        self.since_prune += removed.len();
        let stale: Vec<usize> = self.groups.iter().enumerate()
            .filter(|(_, key)| matches!(key.identity, HomeHubIdentity::Ephemeral { generation: g, .. } if g != generation))
            .map(|(at, _)| at).collect();
        // The sweep of the item keys is batched (`KEY_PRUNE_BATCH`): a sync that changed a row or
        // two only leaves a handful of keys behind, and closing their gaps moves every key after
        // them, in the table and in the position indexes. It runs when enough have left the page,
        // or at once when the table stands over its bound.
        let swept = self.since_prune >= KEY_PRUNE_BATCH || self.items.len() > key_cap(now.len());
        let mut drop: Vec<usize> = Vec::new();
        if swept {
            #[cfg(test)]
            { self.cost.sweeps += 1; }
            self.since_prune = 0;
            let pinned: HashSet<u32> = remembered.iter().map(|&(_, elem)| elem).collect();
            // shown now, or a sync ago (`removed` is what left the page this sync), or the focus
            let shown = |elem: u32| now.contains_key(&elem) || removed.contains(&elem) || Some(elem) == focus;
            let live = |elem: u32| shown(elem) || pinned.contains(&elem);
            drop = self.items.iter().enumerate()
                .filter(|(_, key)| {
                    let volatile = match &key.identity {
                        HomeItemIdentity::Slot { .. } => true,
                        HomeItemIdentity::Item { hub, .. } => matches!(hub, HomeHubIdentity::Ephemeral { .. }),
                    };
                    // a placeholder's key is a pure function of its publication: never worth keeping
                    volatile && !shown(key.elem)
                })
                .map(|(at, _)| at)
                .collect();
            let cap = key_cap(now.len());
            let kept = self.items.len() - drop.len();
            if kept > cap {
                let dropped: HashSet<u32> = drop.iter().map(|&at| self.items[at].elem).collect();
                let held = unlive_excess(
                    self.items.iter().map(|key| key.elem),
                    |elem| live(elem) || dropped.contains(&elem),
                    kept - cap,
                    EvictOrder::Elem,
                );
                drop.extend(self.items.iter().enumerate().filter(|(_, key)| held.contains(&key.elem)).map(|(at, _)| at));
            }
        }
        // The tables compact in place and the indexes follow the positions they moved: no identity
        // is cloned or hashed again for the entries that stay.
        // (a table shared with the memory is detached only when something goes)
        let moved = if drop.is_empty() { None } else { Compaction::remove(Arc::make_mut(&mut self.items), drop) };
        if let Some(moved) = moved {
            self.item_index.retain(|_, keys| {
                keys.by_rk.retain(|_, held| {
                    held.retain_mut(|(_, at)| moved.follow_in_place(at));
                    !held.is_empty()
                });
                keys.slots.retain(|_, at| moved.follow_in_place(at));
                !keys.is_empty()
            });
        }
        if let Some(moved) = Compaction::remove(&mut self.groups, stale) {
            self.group_index.retain(|_, at| moved.follow_in_place(at));
        }
    }

    fn identity_of(item: &PmsMovie) -> Option<(plx_plex::plex::ServerId, String)> {
        (!item.rk.is_empty()).then(|| (item.sid, item.rk.clone()))
    }

    fn same_item(item: &PmsMovie, identity: &(plx_plex::plex::ServerId, String)) -> bool {
        item.sid == identity.0 && item.rk == identity.1
    }

    fn hero_by_identity<'a>(
        view: HubsView<'a>,
        identity: &(plx_plex::plex::ServerId, String),
    ) -> Option<HeroRef<'a>> {
        (0..view.hero_count())
            .filter_map(|i| view.hero(i))
            .find(|h| Self::same_item(h.item, identity))
    }

    fn first_hero(view: HubsView<'_>) -> Option<HeroRef<'_>> {
        view.hero(0).or_else(|| {
            let hub = view.hub(0)?;
            Some(HeroRef {
                item: hub.items.first()?,
                source: hub.source,
            })
        })
    }

    /// Keep the carousel on a hero the pool holds. When its hero has LEFT the pool (a finished
    /// film off Continue Watching, a watched episode replaced by the next under another key — the
    /// post-playback refetch), the billboard goes where the Continue Watching focus goes, by the
    /// same owner rule (`next_episode_of`): the show's next episode, else the hero now at the SAME
    /// index — the one after it, whose art the billboard already holds as its next neighbour
    /// (`prefetch_order`). Restarting at hero 0 put another title, one the billboard
    /// had no reason to hold, under the viewer the moment they came back (owner report 2026-10-16,
    /// "an unpleasant home hero reload"). With no hero before it, the rotation starts at the first.
    fn reconcile_carousel(&mut self, view: HubsView<'_>) {
        let n = view.hero_count();
        let at = |id: &(plx_plex::plex::ServerId, String)| {
            (0..n).find(|&i| view.hero(i).is_some_and(|h| Self::same_item(h.item, id)))
        };
        if let Some(index) = self.carousel.as_ref().and_then(at) {
            self.anchor_carousel(view, index);
            return;
        }
        let successor = self.carousel.as_ref().and(self.carousel_anchor.as_ref()).filter(|_| n > 0).map(|anchor| {
            anchor
                .show
                .as_ref()
                .and_then(|(sid, show)| {
                    (0..n).find(|&i| view.hero(i).is_some_and(|h| h.item.sid == *sid && show_of(h.item) == Some(show)))
                })
                .unwrap_or(anchor.index.min(n - 1))
        });
        self.outgoing = None;
        self.hero_slide.jump(1.0);
        let index = successor.or((n > 0).then_some(0));
        self.carousel = index
            .and_then(|i| view.hero(i))
            .or_else(|| Self::first_hero(view))
            .and_then(|h| Self::identity_of(h.item));
        match index.filter(|_| self.carousel.is_some()) {
            Some(i) => self.anchor_carousel(view, i),
            None => self.carousel_anchor = None,
        }
    }

    fn anchor_carousel(&mut self, view: HubsView<'_>, index: usize) {
        // Runs on every tick: compare borrowed before allocating.
        let show = view.hero(index).and_then(|h| Some((h.item.sid, show_of(h.item)?)));
        if self.carousel_anchor.as_ref().is_some_and(|a| {
            a.index == index && a.show.as_ref().map(|(sid, show)| (*sid, show.as_str())) == show
        }) {
            return;
        }
        let show = show.map(|(sid, show)| (sid, show.to_owned()));
        self.carousel_anchor = Some(CarouselAnchor { index, show });
    }

    fn selected_hero<'a>(&self, view: HubsView<'a>) -> Option<HeroRef<'a>> {
        self.carousel
            .as_ref()
            .and_then(|id| Self::hero_by_identity(view, id))
            .or_else(|| Self::first_hero(view))
    }

    fn outgoing_hero<'a>(&self, view: HubsView<'a>) -> Option<HeroRef<'a>> {
        self.outgoing
            .as_ref()
            .and_then(|id| Self::hero_by_identity(view, id))
    }

    fn carousel_index(&self, view: HubsView<'_>) -> usize {
        self.carousel
            .as_ref()
            .and_then(|id| {
                (0..view.hero_count())
                    .find(|&i| view.hero(i).is_some_and(|h| Self::same_item(h.item, id)))
            })
            .unwrap_or(0)
    }

    fn flip(&mut self, view: HubsView<'_>, dir: i32) -> bool {
        let n = view.hero_count();
        if n <= 1 || self.hero_flip_cd > 0.0 {
            return false;
        }
        let cur = self.carousel_index(view).min(n - 1);
        let next = (cur as i32 + dir).rem_euclid(n as i32) as usize;
        let Some(current) = view.hero(cur).and_then(|h| Self::identity_of(h.item)) else {
            return false;
        };
        let Some(incoming) = view.hero(next).and_then(|h| Self::identity_of(h.item)) else {
            return false;
        };
        self.outgoing = Some(current);
        self.carousel = Some(incoming);
        self.hero_dir = dir as f32;
        self.hero_slide.jump(0.0);
        self.hero_flip_cd = HERO_FLIP_CD;
        self.hero_auto = HERO_AUTO_S;
        // One line per page TURN (a key press or the 8 s advance, never a frame): the demo film
        // reads the pages it showed from these (`tools/site_video.py`, `hero_pool.logged`, `hero_pool.turns`).
        if let Some(page) = view.hero(next) {
            plx_base::eventlog::log(&format!("home: hero page slot {next} rk={}", page.item.rk));
        }
        true
    }

    fn slide_offsets(&self) -> Option<(f32, f32)> {
        self.outgoing.as_ref()?;
        Some((
            -self.hero_dir * self.hero_slide.pos * SCR_W,
            self.hero_dir * (1.0 - self.hero_slide.pos) * SCR_W,
        ))
    }

    fn locate(&self, elem: u32) -> Option<Located> {
        match elem {
            HERO_PLAY_ELEM => return Some(Located::Hero(0)),
            HERO_INFO_ELEM => return Some(Located::Hero(1)),
            HERO_WATCHLIST_ELEM => return Some(Located::Hero(2)),
            _ => {}
        }
        if elem & HEADING_BASE != 0 {
            return self.rows.iter().position(|row| {
                heading_elem(row.group) == elem && row.link.is_some()
            }).map(Located::Heading);
        }
        self.elem_at
            .get(&elem)
            .map(|&(row, col)| Located::Item(row as usize, col as usize))
    }

    fn focused_loc(&self, focus: Option<FocusKey<u32>>) -> Option<Located> {
        focus
            .filter(|k| k.entry == self.entry)
            .and_then(|k| self.locate(k.elem))
    }

    fn focused_grid(&self, focus: Option<FocusKey<u32>>) -> Option<(usize, usize)> {
        match self.focused_loc(focus)? {
            Located::Item(row, col) => Some((row, col)),
            Located::Hero(_) | Located::Heading(_) => None,
        }
    }

    /// The row a focused linked heading heads.
    fn focused_heading(&self, focus: Option<FocusKey<u32>>) -> Option<usize> {
        match self.focused_loc(focus)? {
            Located::Heading(row) => Some(row),
            Located::Hero(_) | Located::Item(..) => None,
        }
    }

    /// **The shelf the page rests on for the ENGINE's `focus`**: the focused card's or linked
    /// heading's, and the first for the billboard (`ui_cards::grid_rest_scroll` rests row 0 at the
    /// head), so a fold to the billboard brings the page back to the first shelf instead of leaving
    /// the deep rest it had under the hidden shelves. `None` (the top bar, nothing focused) leaves
    /// the rest where it was.
    ///
    /// The picture's focus is not this: the billboard stays "focused" in the picture until the dive
    /// is half way (`activation_elem`), so resting by it kept the page's last deep target through
    /// the fold and the first Down drew the deep shelves on its way back up.
    fn rest_row(&self, focus: Option<FocusKey<u32>>) -> Option<usize> {
        match self.focused_loc(focus)? {
            Located::Item(row, _) | Located::Heading(row) => Some(row),
            Located::Hero(_) => Some(0),
        }
    }

    /// The row whose label band is open (or opening) for `focus` — the focused card's row, `None`
    /// on a linked heading, and `fallback` (the row a hero→grid door would open) otherwise.
    /// The band the shelf at `row` opens when focused (`None`, a linked heading focused, opens none):
    /// the Continue Watching deck's is its own ([`shelf_policy`]).
    fn band_open_at(&self, row: Option<usize>) -> f32 {
        row.and_then(|row| self.rows.get(row)).map_or_else(|| band_open_of(false),
            |hub| band_open_of(hub.identity == HomeHubIdentity::ContinueWatching))
    }

    fn band_row(&self, focus: Option<FocusKey<u32>>, fallback: usize) -> Option<usize> {
        match self.focused_loc(focus) {
            Some(Located::Item(row, _)) => Some(row),
            Some(Located::Heading(_)) => None,
            Some(Located::Hero(_)) | None => Some(fallback),
        }
    }

    /// Where a row's linked heading leads, if it has one.
    fn linked(&self, row: usize) -> Option<&plx_plex::plex::collections::CollectionRef> {
        self.rows.get(row)?.link.as_ref()
    }

    /// The linked heading of `row`: title case, the hub's own title, its collection's member count
    /// and its source annotation.
    fn heading_widget<'a>(&self, view: HubsView<'a>, row: usize) -> Option<LinkedHeading<'a>> {
        self.linked(row)?;
        let hub = self.hub(view, row)?;
        Some(LinkedHeading::heading(hub.title, hub.source).total(hub.total))
    }

    /// The heading's face: its cap top rides the row exactly as an unlinked heading does.
    fn heading_rect(&self, view: HubsView<'_>, row: usize, measure: &dyn Measure, at: At,
        focus: Option<FocusKey<u32>>) -> Option<Rect> {
        let heading = self.heading_widget(view, row)?;
        let m = heading.measure(measure);
        let focused = self.focused_heading(focus) == Some(row);
        let y = match at {
            At::Drawn => heading_y(self.grid.base_y(row), self.grid.lift(row)),
            At::SpringTarget => GRID_TOP_Y + shelf_top_banded(row, self.band_row(focus, row), self.band_open_at(self.band_row(focus, row)))
                - self.grid.scroll_target - TITLE_DY,
        };
        Some(heading.face_rect(MARGIN_X, y, f32::from(focused), &m))
    }

    fn visible_focus(&self, focus: Option<FocusKey<u32>>) -> Option<FocusKey<u32>> {
        focus.map(|key| FocusKey {
            entry: key.entry,
            elem: self.activation_elem(key.elem),
        })
    }

    fn hub<'a>(&self, view: HubsView<'a>, row: usize) -> Option<HubRef<'a>> {
        (row < self.rows.len()).then(|| view.hub(row)).flatten()
    }

    fn item_at<'a>(&self, view: HubsView<'a>, row: usize, col: usize) -> Option<&'a PmsMovie> {
        self.hub(view, row)?.items.get(col).map(|m| &**m)
    }

    fn env(&self, dt: f32) -> Env {
        let mut env = Env::inert();
        env.dt = dt;
        env.sp = self.snap.pos;
        env.hero_a = hero_alpha(self.snap.pos, 0.55);
        env
    }

    fn layout_grid(&mut self) {
        for (shelf, hub) in self.grid.shelves.iter_mut().zip(&self.rows) {
            shelf.set_caption_policy(Some(shelf_policy(hub.identity == HomeHubIdentity::ContinueWatching)));
        }
        let top = PEEK_Y + (GRID_TOP_Y - PEEK_Y) * self.snap.pos;
        let mut flow = 0.0;
        for shelf in &mut self.grid.shelves {
            shelf.set_base_y(top + flow - self.grid.scroll_y.pos * self.snap.pos);
            flow += ui_cards::ROW_PITCH_FIXED + shelf.under_band();
        }
    }

    /// `cx` is the PICTURE's view of focus (the shelves read it, and read none until the dive has
    /// reached them); `engine` is the engine's own focus, which the page rests by ([`Self::rest_row`]).
    fn update_grid<H: HomeLike>(&mut self, view: HubsView<'_>, cx: &Cx<'_, H>, engine: Option<FocusKey<u32>>,
        fx: &mut Effects<'_, H>, t: Tick) {
        let dt = t.dt();
        let focused = self.focused_grid(cx.focus.current);
        // Until the dive has reached the shelves the engine's card is not the picture's focus: the
        // shelves read none (they let go) and keep what the moves said, so the card grows from
        // rest when they wake rather than being adopted whole.
        let dormant = self.snap.pos <= 0.5;
        let mut shelves = std::mem::take(&mut self.grid.shelves);
        let mut on_screen: Option<(usize, usize)> = focused.map(|(row, _)| (row, row));
        for (row, shelf) in shelves.iter_mut().enumerate() {
            shelf.dormant(dormant);
            let top = heading_y(shelf.base_y(), shelf.heading_lift());
            let bottom = shelf.base_y() + CARD_H + shelf.under_band();
            let visible = !dormant && on_axis(top, bottom - top, SCR_H, 0.0);
            if visible {
                on_screen = Some(on_screen.map_or((row, row), |(lo, hi)| (lo.min(row), hi.max(row))));
            }
            let mut src = self.cards(view, row);
            src.paging = visible && focused.is_some_and(|(r, col)|
                r == row && col.saturating_add(ui_cards::PAGE_EDGE_CARDS) >= self.rows[row].elems.len());
            // The shelf skips a row at exact rest with nothing focused, and parks one that has
            // settled once focus left it.
            let want = shelf.on(&ScreenEvent::Tick(t), cx, &src, fx);
            let offset = self.hub(view, row).map_or(0, |hub| hub.offset);
            let wanted = matches!(want, Some(ui_cards::CardEvent::Want(_)));
            if shelf.page_cancel(cx, &src, offset, visible).is_some() {
                if let Some(cmd) = self.hub(view, row).and_then(|hub| hub_page_cmd(&hub,
                    |sid, id, key| HubsCmd::CancelPage { sid, id, key })) {
                    fx.push(Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(cmd))));
                }
            }
            if let Some(edge) = shelf.page_ask(cx, &src, wanted, offset, visible) {
                let before = edge == ui_cards::PageEdge::Before;
                if let Some(cmd) = self.hub(view, row).and_then(|hub| hub_page_cmd(&hub,
                    |sid, id, key| HubsCmd::Page { sid, id, key, before, seen: view.generation })) {
                    fx.push(Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(cmd))));
                }
            }
        }
        self.grid.shelves = shelves;
        if !self.ghost_cards.is_empty() && !self.grid.shelves.iter().any(Shelf::landing_pending) {
            self.ghost_cards.clear();
        }
        // The store holds cards for the rows in view and `HOLD_MARGIN` either side (`HoldBand` asks
        // for `HOLD_SLACK` more, once, so a scroll does not re-send the band on every row); the rest
        // keep a descriptor.
        if !self.rows.is_empty() {
            let (lo, hi) = on_screen.unwrap_or((0, 0));
            if let Some(hold) = self.hold.next((lo, hi), self.rows.len()) {
                fx.push(Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(HubsCmd::Hold { lo: hold.0, hi: hold.1 }))));
            }
        }
        if let Some(row) = self.rest_row(engine).filter(|row| *row < self.rows.len()) {
            self.grid.scroll_target = rest_scroll(row, self.rows.len(), self.band_open_at(self.rows.len().checked_sub(1)));
        }
        self.grid
            .scroll_y
            .step(self.grid.scroll_target, K_SCROLL, dt);
        self.layout_grid();
    }

    /// The two screen-specific answers the hold hint needs (`ui::hold_hint`, "Adopting it"): which
    /// SHELF TILE focus is resting on (`None` for the tab strip, the hero, a linked heading, a
    /// covered page — a menu or modal is up — or the plaintext question), and whether everything
    /// under it has stopped moving. The press clock is passed straight through.
    fn hint_input<H: HomeLike>(&self, cx: &Cx<'_, H>, snap_moving: bool) -> HintInput {
        let tile = cx.focus.current.filter(|key| key.entry == self.entry);
        let focused = tile.and_then(|key| self.focused_grid(Some(key)).map(|at| (key.elem, at)));
        let Some((elem, (row, col))) = focused.filter(|_| !self.covered && !self.plaintext_alert.is_open())
        else {
            return HintInput::default();
        };
        let count = self.rows.get(row).map_or(0, |r| r.elems.len());
        let glide = self
            .grid
            .shelves
            .get(row)
            .map_or(0.0, |shelf| shelf.settle_lag(count, col));
        // The screenshot pipeline pins every free-running, time-driven animation (`stillclock`,
        // `motion::phase_clocks_held`): a hint that stands up 1.5 s after a scene settles is one,
        // and would put itself into every documentation figure that rests on a card. A real hold
        // is input, not a clock, so it is not pinned.
        let settled = !snap_moving
            && !plx_machine::motion::phase_clocks_held()
            && self.snap.pos >= 0.5
            && self.visible_activation.is_none()
            && glide.abs() < HINT_REST_PX
            && (self.grid.scroll_y.pos - self.grid.scroll_target).abs() < HINT_REST_PX;
        HintInput::new(Some(elem), settled, cx.press.held_ms)
    }

    fn tick<H: HomeLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let dt = t.dt();
        self.sync_catalog(cx);
        let view = H::hubs(cx);
        // A flip or an addressed command moves the carousel between publications, and
        // `sync_catalog` only reconciles on a new one: note where the hero stands every tick.
        if let Some(index) = self.carousel.as_ref().and_then(|id| {
            (0..view.hero_count()).find(|&i| view.hero(i).is_some_and(|h| Self::same_item(h.item, id)))
        }) {
            self.anchor_carousel(view, index);
        }
        let cta_available = self.hero_len(view) > 0;
        if cta_available
            && !self.cta_available
            && !self.strip_chosen
            && cx
                .focus
                .current
                .is_none_or(|key| key.elem >= plx_ui::dispatch::STRIP_BASE)
        {
            self.reseat(FocusTarget::ContainerGroup(HERO_GROUP), cx, fx);
        }
        self.cta_available = cta_available;
        if self.rows.is_empty() {
            self.snap_target = 0.0;
            self.snap.jump(0.0);
        }
        let current = plx_plex::plex::client_for(plx_plex::plex::current_server()).map(|c| c.machine_id());
        // `|` not `||`: both watches must re-read.
        if self.plaintext.refresh(current, Near::First) | self.clock.refresh(current) {
            fx.invalidate(Provenance::Landing(fx.from()));
        }
        if self.plaintext_alert.is_open()
            && !(status_read(view).is_some_and(|(_, kind, _)| kind == StatusKind::Failed)
                && self.plaintext_alert.subject() == self.plaintext.verdict().map(|v| v.machine_id.as_str())
                && plaintext_question::asks(self.plaintext.verdict()))
        {
            // the read-out it was asked from is gone, or no longer asks about that server
            self.plaintext_alert.withdraw();
        }
        self.plaintext_alert.update(dt);
        self.status_ms = (self.status_ms + dt * 1000.0)
            % (plx_ui::widgets::Spinner::PERIOD_MS as f32 * 1000.0);
        self.hero_flip_cd = (self.hero_flip_cd - dt).max(0.0);
        if self.outgoing.is_some() {
            self.hero_slide.step(1.0, K_SLIDE, dt);
            if (1.0 - self.hero_slide.pos).abs() * SCR_W < HERO_SLIDE_REST_PX {
                self.hero_slide.jump(1.0);
                self.outgoing = None;
            }
        }
        if self.covered {
            // Compact surfaces keep ticking their host page, unlike Sheet/Alert/Opaque surfaces.
            // Cover/Uncover is the shared modal lifecycle across BOTH policies, so retain the
            // countdown here and restart it exactly once when the last surface is dismissed.
        } else if self.hero_pinned {
            // Held by `HomeCmd::PinHero`: no countdown, no flip.
        } else if self.snap.pos < 0.05 && view.hero_count() > 1 {
            // Spelled as an assignment, not `-= dt`: bit-for-bit identical arithmetic to the
            // pre-D4 accumulator, deliberately UNCHANGED — `hero_auto` is HASHED `LogicalState`
            // (`SHAPE`'s `hero_auto:f32`) across three committed replay fixtures, and a
            // `motion::Ramp`'s absolute-`Tick.ms` math computes the same real quantity through a
            // different float operation sequence that measurably diverges the hash (verified:
            // `tests/focusfp.sh --replay` on flow 1 disagreed from frame 7 on, once the arithmetic
            // changed).
            //
            // It reports NO `Motion`, deliberately. Nothing draws `hero_auto`: it is a TIMER, not
            // an animator (the `Timer` class of `docs/retui-invalidation-design.md`, not its
            // `Ramp`), and the dispatcher delivers `Tick` to the page every loop iteration whether or not the
            // frame presents (`plx_machine::idle`, "What this module does NOT do"), so the countdown runs
            // on a closed gate while the page is uncovered and the flip it ends in is what wakes
            // it — `outgoing` is set, and `moving` below reports the slide. Noting `Motion` on
            // every countdown tick made a still billboard present at the full frame rate forever:
            // every modal dismiss and every page pop came back to a Home that never went idle
            // (~24 ms of GPU a frame on the TV), and the next transition's first frame paid that
            // queue in its `glClear`.
            // `a_settled_hero_counting_down_lets_the_gate_close_and_still_flips` holds both halves.
            self.hero_auto = self.hero_auto - dt;
            if self.hero_auto <= 0.0 {
                self.hero_flip_cd = 0.0;
                self.flip(view, 1);
            }
        } else {
            self.hero_auto = HERO_AUTO_S;
        }
        self.snap
            .step(pinned_snap(self.snap_target, self.rows.len()), K_SNAP, dt);
        // ONE answer for "is the dive still running", read by the present gate below. See
        // `SNAP_REST_POS`/`SNAP_REST_VEL` for why both terms are needed.
        let snap_moving = (self.snap.pos - self.snap_target).abs() > SNAP_REST_POS
            || self.snap.vel.abs() > SNAP_REST_VEL;
        let engine_on_grid = self.focused_loc(cx.focus.current).is_some_and(Located::on_grid);
        let picture_is_grid = self.snap.pos >= 0.5;
        if engine_on_grid == picture_is_grid {
            self.visible_activation = None;
        }
        let visible_focus = self.visible_focus(cx.focus.current);
        let hero_focus = match self.focused_loc(visible_focus) {
            Some(Located::Hero(i)) if status_read(view).is_none() => Some(i),
            _ => None,
        };
        // decorative: an in-place scale of the focused control, no layout — must not hold the
        // page-freeze snapshot (`idle::decorative`)
        plx_machine::idle::decorative(|| self.hero_pop.step(hero_focus, dt));
        // The bookmark disc: the title it is for (read by the application's feed) and its flip.
        self.disc.shown = status_read(view)
            .is_none()
            .then(|| self.selected_hero(view))
            .flatten()
            .and_then(|h| disc_hex(h.item));
        self.disc.step(dt);
        let visible_cx: Cx<'_, H> = Cx {
            views: cx.views,
            tick: cx.tick,
            measure: cx.measure,
            press: cx.press,
            focus: plx_machine::machine::FocusRead {
                current: visible_focus,
            ..Default::default() },
            owner: cx.owner,
        };
        self.update_grid(view, &visible_cx, cx.focus.current, fx, t);

        let grid_item = self
            .focused_grid(visible_focus)
            .and_then(|(r, c)| self.item_at(view, r, c));
        let selected = self.selected_hero(view);
        let outgoing = self.outgoing_hero(view);
        self.backdrop.update(
            selected,
            outgoing,
            grid_item,
            self.carousel.as_ref(),
            self.snap.pos,
            dt,
            t.ms,
        );
        self.prefetch(view);
        let hint = self.hint_input(cx, snap_moving);
        self.hold_hint.step(hint, t.ms, dt, &mut |ev| fx.note(ev));

        let moving = self.outgoing.is_some()
            || snap_moving
            || matches!(status_read(view), Some((_, StatusKind::Working, _)));
        if moving {
            fx.note(PresentEvent::Motion);
        }
        // Route-owned work is queued only after this read-only step has completed.
        fx.push(Fx::App(AppFx::StoreWork(StoreWork::Hubs)));
        fx.push(Fx::App(AppFx::StoreWork(StoreWork::BrowseDiscovery)));
    }

    /// **The billboard's neighbours are HELD, not hoped for.** Both backdrops a flip can land on
    /// (the carousel's ±[`HERO_PREFETCH`] slots, wrapping) are asked for through the DRAW path every
    /// frame the billboard is up — from the moment an index is shown, busy pipeline or not, and
    /// through the slide, so a flip re-arms its new neighbours on the flip itself. One mechanism
    /// for the manual flip and the auto-advance alike: both call [`Self::flip`], and both land on
    /// whatever this made resident.
    ///
    /// Why a draw probe and not a `warm`: a warm is speculation the source may drop — it runs only
    /// into an idle pipeline, takes LRU age 0 (the FIRST victim of the next miss, so the two
    /// neighbours and their logos evicted one another in a full store), and a warm never revives a
    /// slot the render cache released. That was the manual-flip blink (owner report 2026-09-30):
    /// a press within a second or so of the last flip found its backdrop not even requested, the
    /// outgoing art slid off over the flat ground and the new one faded in afterwards, while the
    /// 8 s countdown always gave the warm time to land. A probe is demand — LRU-touched,
    /// evict-protected, re-armed after a release — and still goes through the source's queue and
    /// the frame's upload [`Budget`] like every other picture. Its cost is the two neighbours'
    /// residency (≈7.4 MB of the 44 MB ceiling) while Home's billboard is up.
    ///
    /// The neighbours' clearLogos stay speculative: a late logo is a small pop, not a blank panel.
    fn prefetch(&self, view: HubsView<'_>) {
        use plx_ui::tex::Warm;
        if !neighbours_armed(self.snap.pos) {
            return;
        }
        let mut order = [0i32; 2 * HERO_PREFETCH];
        let count = prefetch_order(
            self.carousel_index(view) as i32,
            view.hero_count() as i32,
            &mut order,
        );
        let neighbours = || order[..count].iter().filter_map(|&i| view.hero(i as usize));
        for hero in neighbours() {
            plx_ui::widgets::resolve_tex_wh_on(hero.item.sid.raw(), &hero.item.art, 1280, 720, 0);
        }
        if !(prefetch_armed(self.snap.pos, self.outgoing.is_some())
            && plx_ui::tex::source_idle())
        {
            return;
        }
        for hero in neighbours() {
            if plx_ui::tex::logo_warm(hero.item.sid.raw(), hero_logo_rk(hero.item))
                == Warm::Claimed
            {
                return;
            }
        }
    }

    fn request_for_strip(elem: u32) -> Option<HomeReq> {
        Some(match elem {
            STRIP_HOME_ELEM => HomeReq::Tab(HomeTab::Home),
            STRIP_MOVIES_ELEM => HomeReq::Tab(HomeTab::Movies),
            STRIP_SHOWS_ELEM => HomeReq::Tab(HomeTab::Shows),
            STRIP_SEARCH_ELEM => HomeReq::Tab(HomeTab::Search),
            STRIP_WATCHLIST_ELEM => HomeReq::Tab(HomeTab::Watchlist),
            STRIP_LIVETV_ELEM => HomeReq::Tab(HomeTab::LiveTv),
            STRIP_ACCOUNT_ELEM => HomeReq::Account,
            _ => return None,
        })
    }

    fn reseat<H: HomeLike>(&mut self, focus: FocusTarget<u32>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        // Enter is queued behind this frame's Tick. Consume the CTA availability observed by
        // this seating request now, so that Tick cannot queue an implicit initial Hero seat
        // over the accepted target while the engine still holds its previous focus.
        self.cta_available = self.hero_len(H::hubs(cx)) > 0;
        fx.push(Fx::Deliver(
            MachineId::Instance(self.instance),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus })),
        ));
    }

    fn emit_item_menu<H: HomeLike>(&self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        if self.snap.pos < 0.5 {
            return Handled::No;
        }
        let Some((row, col)) = self.focused_grid(cx.focus.current) else {
            return Handled::No;
        };
        let Some(item) = self.item_at(H::hubs(cx), row, col) else {
            return Handled::No;
        };
        // A channel has no item menu: it is no library item (`KIND_LIVE_CHANNEL`).
        if item.rk.is_empty() || plx_data::pms::live_channel_of(item).is_some() {
            return Handled::No;
        }
        fx.push(Fx::App(AppFx::Home(HomeReq::ItemMenu {
            sid: item.sid,
            rk: item.rk.clone(),
        })));
        fx.invalidate(Provenance::Input);
        Handled::Yes
    }

    fn command<H: HomeLike>(
        &mut self,
        command: HomeCmd,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        match command {
            HomeCmd::FocusGrid { row, col } => {
                let Some(key) = self.dev_focus_key(row, col, cx) else {
                    return Handled::No;
                };
                self.reseat(FocusTarget::Elem(key), cx, fx);
            }
            HomeCmd::Hero => {
                self.strip_chosen = false;
                self.reseat(FocusTarget::ContainerGroup(HERO_GROUP), cx, fx);
            }
            HomeCmd::FocusStrip(tab) => {
                let elem = match tab {
                    HomeTab::Home => STRIP_HOME_ELEM,
                    HomeTab::Movies => STRIP_MOVIES_ELEM,
                    HomeTab::Shows => STRIP_SHOWS_ELEM,
                    HomeTab::Search => STRIP_SEARCH_ELEM,
                    HomeTab::Watchlist => STRIP_WATCHLIST_ELEM,
                    HomeTab::LiveTv => STRIP_LIVETV_ELEM,
                };
                self.strip_chosen = true;
                self.reseat(
                    FocusTarget::Elem(FocusKey {
                        entry: self.entry,
                        elem,
                    }),
                    cx,
                    fx,
                );
            }
            HomeCmd::Flip(direction) => {
                if direction == 0 || !self.flip(H::hubs(cx), direction.signum()) {
                    return Handled::No;
                }
                fx.invalidate(Provenance::Input);
            }
            HomeCmd::SelectHero(index) | HomeCmd::PinHero(index) => {
                let view = H::hubs(cx);
                if view.hero_count() == 0 {
                    return Handled::No;
                }
                let index = (index.max(0) as usize).min(view.hero_count() - 1);
                let Some(hero) = view.hero(index) else {
                    return Handled::No;
                };
                let Some(identity) = Self::identity_of(hero.item) else {
                    return Handled::No;
                };
                self.carousel = Some(identity);
                self.outgoing = None;
                self.hero_slide.jump(1.0);
                if matches!(command, HomeCmd::PinHero(_)) {
                    self.hero_pinned = true;
                    plx_base::eventlog::log(&format!("home: hero pinned at slot {index}"));
                }
                fx.invalidate(Provenance::Input);
            }
            HomeCmd::ItemMenu => return self.emit_item_menu(cx, fx),
        }
        Handled::Yes
    }

    fn activate<H: HomeLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if let Some(req) = Self::request_for_strip(elem) {
            fx.push(Fx::App(AppFx::Home(req)));
            return;
        }
        let view = H::hubs(cx);
        if status_read(view).is_some_and(|(_, kind, _)| kind == StatusKind::Failed)
            && elem == HERO_PLAY_ELEM
            && plaintext_question::asks(self.plaintext.verdict())
        {
            // The server is on this network and only answers without encryption: *Connect*
            // asks the shared question rather than retrying what cannot succeed.
            if let Some(v) = self.plaintext.verdict() {
                let machine = v.machine_id.clone();
                let sid = plx_plex::plex::id_of_machine(&machine);
                self.plaintext_alert.open(&machine, sid, MachineId::Instance(self.instance), fx);
                fx.invalidate(Provenance::Input);
            }
            return;
        }
        if status_read(view).is_some() && elem == HERO_PLAY_ELEM {
            fx.push(Fx::App(AppFx::Store(
                StoreId::Hubs,
                StoreCmd::Hubs(HubsCmd::Retry),
            )));
            return;
        }
        let elem = self.activation_elem(elem);
        let Some(located) = self.locate(elem) else {
            return;
        };
        let (item, req) = match located {
            Located::Hero(0) => {
                let Some(item) = self.selected_hero(view).map(|h| h.item) else {
                    return;
                };
                let resume_ns = plx_data::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000);
                (
                    item,
                    HomeReq::Play {
                        sid: item.sid,
                        rk: item.rk.clone(),
                        resume_ns,
                    },
                )
            }
            Located::Hero(1) => {
                let Some(item) = self.selected_hero(view).map(|h| h.item) else {
                    return;
                };
                (
                    item,
                    HomeReq::Detail {
                        sid: item.sid,
                        rk: item.rk.clone(),
                    },
                )
            }
            Located::Hero(2) => {
                // The bookmark disc: the item menu's own write, identity captured here. The app
                // queues it on the Watchlist store, which flips the face at once and puts it back
                // if plex.tv refuses (`app::input::apply_item_action`).
                if let Some(sel) = self.selected_hero(view) {
                    if let Some(act) = self.disc.press(sel.item) {
                        fx.push(Fx::App(AppFx::ItemMenu(crate::registry::ItemMenuReq {
                            act, sid: sel.item.sid, item: None, season: None, loaded_episode: false, from_home: true,
                        })));
                    }
                }
                return;
            }
            Located::Hero(_) => return,
            Located::Heading(row) => {
                // OK on a linked heading opens the collection it names. BACK from that page
                // returns here: the heading is an ordinary engine key the return state restores.
                if let Some(link) = self.linked(row) {
                    fx.push(Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Collection(
                        link.clone(),
                    )))));
                }
                return;
            }
            Located::Item(row, col) => {
                let Some(item) = self.item_at(view, row, col) else {
                    return;
                };
                if let Some((dvr, channel)) = plx_data::pms::live_channel_of(item) {
                    (
                        item,
                        HomeReq::Tune {
                            sid: item.sid,
                            dvr: dvr.to_owned(),
                            channel: channel.to_owned(),
                            title: item.title.clone(),
                        },
                    )
                } else if self
                    .rows
                    .get(row)
                    .is_some_and(|h| h.identity == HomeHubIdentity::ContinueWatching)
                    && plx_media::route::deck_press().press_plays()
                {
                    let resume_ns =
                        plx_data::metadata::resume_ns(item.resume_ms, item.dur_ns / 1_000_000);
                    (
                        item,
                        HomeReq::Play {
                            sid: item.sid,
                            rk: item.rk.clone(),
                            resume_ns,
                        },
                    )
                } else {
                    (
                        item,
                        HomeReq::Detail {
                            sid: item.sid,
                            rk: item.rk.clone(),
                        },
                    )
                }
            }
        };
        if !item.rk.is_empty() {
            fx.push(Fx::App(AppFx::Home(req)));
        }
    }

    fn draw_page<H: HomeLike>(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let view = H::hubs(f.cx);
        let visible_focus = self.visible_focus(f.focus.current);
        let env = self.env(0.0);
        let focus = self.focused_loc(visible_focus);
        let p = f.painter.alpha(f.page_alpha);
        let slide = self.slide_offsets();
        // Re-bind the art to the heroes `slide` is about: an event delivered after this frame's
        // `Tick` (a manual flip) has changed them since `update` ran (`Backdrop::bind`).
        self.backdrop.bind(
            self.selected_hero(view),
            self.outgoing_hero(view),
            self.carousel.as_ref(),
            self.snap.pos,
        );
        plx_ui::profile::phase("hm.backdrop", || self.backdrop.draw(p, &env, slide));
        if env.hero_a > 0.01 {
            plx_ui::profile::phase("hm.hero", || {
                self.draw_hero(
                    view,
                    &env,
                    p.alpha(env.hero_a),
                    f.page_alpha,
                    focus,
                    f.measure,
                    f.press.scale,
                )
            });
        }
        plx_ui::profile::phase("hm.grid", || {
            let heading = self.focused_heading(visible_focus);
            self.draw_grid(f, view, &env, p, heading)
        });
        plx_ui::profile::phase("hm.status", || self.draw_status(view, &env, p, focus));
        plx_ui::testpat::underlay(p);
        plx_ui::testpat::draw(p);
        // A standing note, never a target: drawn over the page, records no stop and no hit rect.
        self.hold_hint.draw(p, f.measure);
        if !self.plaintext_alert.visible() {
            // the question owns the pointer while it is up; nothing under it is a target
            self.record_stops(f, view);
        }
        self.plaintext_alert.draw(f, self.entry);
    }

    fn draw_hero(
        &self,
        view: HubsView<'_>,
        env: &Env,
        p: Painter,
        page_alpha: f32,
        focus: Option<Located>,
        measure: &dyn Measure,
        press_scale: f32,
    ) {
        let Some(hero) = self.selected_hero(view) else {
            return;
        };
        if let Some((out_x, in_x)) = self.slide_offsets() {
            if let Some(old) = self.outgoing_hero(view) {
                let po = p.translate(out_x, 0.0);
                hero_content(old.item, display_source(old.source), po, out_x, measure);
                self.draw_hero_actions(old.item, env, po, out_x, false, page_alpha, focus, measure, press_scale);
            }
            let pi = p.translate(in_x, 0.0);
            hero_content(hero.item, display_source(hero.source), pi, in_x, measure);
            self.draw_hero_actions(hero.item, env, pi, in_x, true, page_alpha, focus, measure, press_scale);
        } else {
            hero_content(hero.item, display_source(hero.source), p, 0.0, measure);
            self.draw_hero_actions(hero.item, env, p, 0.0, true, page_alpha, focus, measure, press_scale);
        }
        if view.hero_count() > 1 {
            PageDots::new(view.hero_count())
                .active(self.carousel_index(view))
                .centered_at(SCR_W * 0.5, HERO_ROW_Y + HERO_CTRL_D + theme::space::SM)
                .draw(env, p);
        }
    }

    fn draw_hero_actions(
        &self,
        hero: &PmsMovie,
        env: &Env,
        p: Painter,
        dx: f32,
        live: bool,
        page_alpha: f32,
        focus: Option<Located>,
        measure: &dyn Measure,
        press_scale: f32,
    ) {
        let resumes = plx_data::metadata::resume_ns(hero.resume_ms, hero.dur_ns / 1_000_000) > 0;
        let label = plx_appkit::hero_layout::play_label(resumes);
        let HeroRects { play: pill, disc, info } = hero_rects(measure, hero, label);
        let mark_d = HERO_PAGER_D.round();
        let mark = Rect::new(
            info.x + info.w + HERO_PAGER_PAD,
            HERO_ROW_Y + (HERO_CTRL_D - mark_d) * 0.5,
            mark_d,
            mark_d,
        );
        if !on_axis(
            MARGIN_X + dx,
            mark.x + mark.w + HERO_PAGER_PAD - MARGIN_X,
            SCR_W,
            0.0,
        ) {
            return;
        }
        let may_read = live && dx.abs() < 0.5 && page_alpha >= 0.999;
        let palette = plx_gfx::gfx::sample_control_ground(
            [pill.x + dx, pill.y, info.x + info.w - pill.x, pill.h],
            may_read,
        )
        .map(ControlPalette::ambient)
        .unwrap_or_default();
        let pop = |i| if live { self.hero_pop.scale_with(i, press_scale) } else { 1.0 };
        // The fill rides the lift spring (`CtlPop::focus`) so the plate cross-fades (and the ink flips) with the pop;
        // a frame drawn off the live row (a transition's snapshot) has no spring and reads the focus.
        let fill = |i| if live { self.hero_pop.focus(i) } else { f32::from(focus == Some(Located::Hero(i))) };
        Button::new(label.as_ptr(), theme::size::BODY, pill)
            .icon(Icon::Play)
            .focus_mix(fill(0))
            .palette(palette)
            .scale(pop(0))
            .draw(env, p);
        if let Some((rect, hex)) = disc.zip(disc_hex(hero)) {
            // At either rest the plain face; only a flip in flight layers the fill.
            let (icon, mid) = plx_appkit::watchlist_disc::paint(self.disc.progress(hex));
            let mut button = CircleButton::new(c"".as_ptr())
                .icon(icon)
                .at(rect.x, rect.y)
                .focus_mix(fill(2))
                .palette(palette)
                .scale(pop(2));
            if let Some(frame) = mid {
                button = button.icon_fill(plx_appkit::watchlist_disc::face_icon(plx_appkit::watchlist_disc::Face::Filled), frame.fill, frame.ribbon_dy);
            }
            button.draw(env, p);
        }
        CircleButton::new(c"".as_ptr())
            .icon(Icon::Info)
            .at(info.x, info.y)
            .focus_mix(fill(1))
            .palette(palette)
            .scale(pop(1))
            .draw(env, p);
        plx_ui::icons::draw(p, Icon::Chevron, mark, theme::TEXT_SECONDARY);
    }

    /// Whether shelf `row` overlaps the screen vertically. The shelf spans from its heading's
    /// line — ABOVE the cards — to the bottom of its label band. Culling by the card rect alone
    /// left a heading already on screen undrawn until the first card pixel crossed the bottom
    /// edge, so it popped in mid-scroll.
    fn shelf_on_screen(&self, row: usize) -> bool {
        let base_y = self.grid.base_y(row);
        let top = heading_y(base_y, self.grid.lift(row));
        let bottom = base_y + CARD_H + self.grid.under_band(row);
        on_axis(top, bottom - top, SCR_H, 0.0)
    }

    fn draw_grid<H: HomeLike>(
        &self,
        f: &DrawFrame<'_, '_, H>,
        view: HubsView<'_>,
        env: &Env,
        p: Painter,
        heading: Option<usize>,
    ) {
        for row in 0..self.rows.len() {
            let Some(hub) = self.hub(view, row) else {
                continue;
            };
            if !self.shelf_on_screen(row) {
                continue;
            }
            let row_y = self.grid.base_y(row);
            // The heading rides the page under the chrome, which stays: it fades by where it is
            // DRAWN (`scroll_chrome`'s clearance, at the place the lift leaves it) and is gone
            // before it reaches the chip, so no ink of it ever stands over the chip's rows. The
            // focused row's heading rests whole: its drawn place is clear of the ramp.
            let alpha = env.sp * plx_ui::scroll_chrome::clearance_under_shown_chrome(heading_y(row_y, self.grid.lift(row)));
            if let Some(linked) = self.heading_widget(view, row).filter(|_| env.sp > 0.02) {
                let m = linked.measure(f.measure);
                let focus_t = f32::from(heading == Some(row) && env.sp > 0.5);
                linked.draw(
                    p.alpha(alpha),
                    MARGIN_X,
                    heading_y(row_y, self.grid.lift(row)),
                    focus_t,
                    &m,
                    f.measure,
                );
            } else if env.sp > 0.02 {
                ui_cards::draw_heading(
                    p.alpha(alpha),
                    hub.title,
                    hub.source,
                    MARGIN_X,
                    heading_y(row_y, self.grid.lift(row)),
                    f32::INFINITY,
                    f.measure,
                );
            }
            if let Some(shelf) = self.grid.shelves.get(row) {
                if shelf.has_ghosts() {
                    shelf.paint_ghosts(f, p, &self.ghost_source(shelf), self.section(row));
                }
                shelf.paint_resting(f, p, &self.cards(view, row), self.section(row));
            }
        }
        // The focused tile after every row, so its glow overlaps the neighbouring rows.
        if let Some((row, _)) = self.focused_grid(self.visible_focus(f.focus.current)) {
            if let (Some(shelf), Some(_)) = (self.grid.shelves.get(row), self.hub(view, row)) {
                shelf.paint_focused(f, p, &self.cards(view, row), self.section(row));
            }
        }
    }

    /// Where row `row`'s tiles are drawn: the top of the tile (screen space), clipped under the top bar.
    fn section(&self, row: usize) -> SectionFrame {
        SectionFrame {
            y: self.grid.base_y(row) + CARD_DY,
            clip: Rect::new(0.0, plx_ui::widgets::TOP_BAR_BOTTOM, SCR_W, SCR_H - plx_ui::widgets::TOP_BAR_BOTTOM),
            // what stands under this row's caption is the next shelf's heading, as it is drawn
            below_rise: self.grid.lift(row + 1),
        }
    }

    /// The cards `shelf`'s landing is fading out, in the order it names them.
    fn ghost_source(&self, shelf: &Shelf) -> GhostCards<'_> {
        GhostCards { cards: shelf.ghost_keys().map(|key| self.ghost_cards.get(&key)).collect() }
    }

    /// Row `row`'s cards as `cards::Shelf` reads them.
    fn cards<'a>(&'a self, view: HubsView<'a>, row: usize) -> HomeCards<'a> {
        HomeCards { paging: false, home: self, view, row, hover: Hover::Focus }
    }

    fn draw_status(&self, view: HubsView<'_>, env: &Env, p: Painter, focus: Option<Located>) {
        let Some(overlay) = status_overlay(view, &self.plaintext, &self.clock) else {
            return;
        };
        overlay
            .phase(self.status_ms as u32)
            .focused(focus == Some(Located::Hero(0)))
            .draw(env, p);
    }

    pub fn record_stops<H: HomeLike>(&self, f: &mut DrawFrame<'_, '_, H>, view: HubsView<'_>) {
        if !f.records_stops() { return; }
        let focus = f.focus.current;
        let (slots, hero_len) = self.hero_slots(view);
        if self.snap.pos < 0.5 {
            for &slot in &slots[..hero_len] {
                let elem = hero_elem(slot);
                let Some(placed) = Focusable::<H>::place(self, &elem, f.cx, At::Drawn) else {
                    continue;
                };
                f.stop(
                    f.painter,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem,
                        },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Press,
                    },
                );
            }
        }
        if self.snap.pos >= 0.5 {
            let focused_row = self.focused_grid(focus).map(|(row, _)| row);
            for (row_index, row) in self.rows.iter().enumerate() {
                // Keep the whole focused row hoverable while vertical reveal is in flight.
                // Other partially visible rows must not steal focus as they pass the pointer.
                let row_focused = focused_row == Some(row_index);
                // A row off screen has nothing to hit; only the focused one keeps its stops.
                if !row_focused && !self.shelf_on_screen(row_index) {
                    continue;
                }
                // Every tile of the row shares one y, so one answer says whether a pointer passing
                // over the row may take its focus: its posters stand clear of the top margin and
                // their upper part clears the bottom one ([`POINTER_REACH`]), which is what the shelf
                // below the focused one is at rest.
                let rest = Rect::new(0.0, self.grid.base_y(row_index) + CARD_DY, CARD_W, CARD_H)
                    .scaled(RowStyle::HOME.focus_scale);
                let pointable = rest.y >= 40.0 && rest.y + rest.h * POINTER_REACH <= SCR_H - 20.0;
                let src = HomeCards {
                    hover: if pointable || row_focused { Hover::Focus } else { Hover::OnlyIfFocused },
                    ..self.cards(view, row_index)
                };
                if let Some(shelf) = self.grid.shelves.get(row_index) {
                    shelf.record_stops(f, f.painter, &src, self.section(row_index));
                }
                // After the row's tiles, so a popped tile's glow never wins the heading's hit.
                if let (Some(heading), Some(rect)) = (
                    self.heading_widget(view, row_index),
                    self.heading_rect(view, row_index, f.measure, At::Drawn, focus),
                ) {
                    let visible = rect.y >= plx_ui::widgets::TOP_BAR_BOTTOM
                        && rect.y + rect.h <= SCR_H;
                    if visible {
                        heading.stop(f, rect, FocusKey { entry: self.entry, elem: heading_elem(row.group) });
                    }
                }
            }
        }
    }

    pub fn hero_item<'a, H: HomeLike>(&self, cx: &Cx<'a, H>) -> Option<&'a PmsMovie> {
        self.selected_hero(H::hubs(cx)).map(|h| h.item)
    }

    pub fn focused_item<'a, H: HomeLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        cx: &Cx<'a, H>,
    ) -> Option<&'a PmsMovie> {
        let view = H::hubs(cx);
        match self.focused_loc(focus)? {
            Located::Hero(_) => self.selected_hero(view).map(|h| h.item),
            Located::Item(row, col) => self.item_at(view, row, col),
            Located::Heading(_) => None,
        }
    }

    pub fn grid_position<H: HomeLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        _cx: &Cx<'_, H>,
    ) -> Option<(usize, usize)> {
        self.focused_grid(focus)
    }

    /// How many cards shelf `row` holds, once the catalog has projected it.
    #[cfg(feature = "devtriggers")]
    pub fn shelf_len(&self, row: usize) -> Option<usize> {
        self.rows.get(row).map(|projection| projection.elems.len())
    }

    /// Every card's pop as the draw paints it, shelf by shelf (`focus` the engine's focus): what a route test
    /// reads to say a card the focus has left is at EXACT rest (see `World::card_pops`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_card_pops(&self, focus: Option<FocusKey<u32>>) -> Vec<Vec<f32>> {
        let at = self.focused_grid(focus);
        self.rows.iter().enumerate().map(|(row, projection)| {
            let col = at.filter(|&(r, _)| r == row).map(|(_, c)| c);
            let Some(shelf) = self.grid.shelves.get(row) else { return Vec::new() };
            (0..projection.elems.len()).map(|i| shelf.drawn_pop(i, col)).collect()
        }).collect()
    }

    /// Diagnostic view of the actual animated state: snap, retained shelf offset,
    /// shelf velocity. No second motion owner or stored telemetry state.
    #[cfg(feature = "devtriggers")]
    pub fn motion_witness(&self, row: usize) -> Option<[f32; 3]> {
        self.rows.get(row)?;
        Some([self.snap.pos, self.grid.scroll_x(row), self.grid.shelves.get(row).map_or(0.0, Shelf::scroll_velocity)])
    }

    pub fn snap_target(&self) -> f32 {
        self.snap_target
    }

    /// Where the hero-to-grid dive actually IS this frame (`snap_target` is where it is going).
    /// The frame-drop line carries both, so a slow frame can be placed on the dive's curve.
    pub fn snap_pos(&self) -> f32 {
        self.snap.pos
    }

    pub fn focused_rect<H: HomeLike>(
        &self,
        focus: Option<FocusKey<u32>>,
        cx: &Cx<'_, H>,
        at: At,
    ) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.entry)?;
        self.focused_grid(Some(key))?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|p| p.rect)
    }

    pub fn redraw_focused<H: HomeLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        focus: Option<FocusKey<u32>>,
    ) {
        let Some(key) = focus.filter(|k| k.entry == self.entry) else {
            return;
        };
        let Some((row, _)) = self.focused_grid(Some(key)) else {
            return;
        };
        let view = H::hubs(f.cx);
        let p = f.painter.alpha(f.page_alpha);
        let cut = plx_ui::widgets::TOP_BAR_BOTTOM;
        let _clip = f.clip(p, Rect::new(0.0, cut, SCR_W, SCR_H - cut));
        if let Some(shelf) = self.grid.shelves.get(row) {
            shelf.redraw_focused(f, p, &self.cards(view, row), self.section(row), Some(key));
        }
    }

    pub fn dev_focus_key<H: HomeLike>(
        &self,
        row: usize,
        col: usize,
        _cx: &Cx<'_, H>,
    ) -> Option<FocusKey<u32>> {
        Some(FocusKey {
            entry: self.entry,
            elem: *self.rows.get(row)?.elems.get(col)?,
        })
    }
}

impl LogicalState for HomeScreen {
    fn write(&self, c: &mut Canon) {
        // Exhaustive field census. Entry/instance identity is encoded by Navigation; spinner
        // phase and backdrop resources only paint. Every motion value queried by input/reveal
        // is encoded below, including velocities that determine the next Tick's answer. A shelf
        // encodes its springs and focus (`Shelf::write`); the moves it has been told since its
        // last tick (`seen`, `left`) and its wake latch (`slept`, `dormant`) are hand-offs from an
        // event to the next `Tick`, not motion, and are not encoded (`dormant` is re-derived from
        // `snap` on every `Tick`).
        let Self { entry: _, instance: _, groups: _, items: _, next_group: _, next_elem: _,
            // `elem_at` is derived from `rows`, which the census already covers via its elems.
            // `item_index` / `group_index` are caches of `items` / `groups`, which are covered.
            rows: _, elem_at: _, item_index: _, group_index: _, projected_generation: _, cw_cards: _, cw_offset: _, carousel_anchor: _, rebound: _, ghost_cards: _, restored_scroll: _, restore_reveal: _, hold: _, since_prune: _, #[cfg(test)] cost: _, carousel: _,
            outgoing: _, hero_flip_cd: _, hero_slide: _, hero_dir: _, hero_auto: _, hero_pinned: _, covered: _,
            snap_target: _,
            visible_activation: _, cta_available: _, strip_chosen: _, snap: _, status_ms: _,
            hero_pop: _, backdrop: _, grid: _,
            // The disc's membership is the Watchlist store's (the application feeds it); the flip
            // is paint, like Detail's.
            disc: _,
            // The plaintext question is the grant table's (`plex::grant::offers`), which no
            // recording can raise, and its alert only paints over a failed read-out. The clock
            // fact is `net::keypin`'s, likewise unraisable by a recording.
            plaintext: _, plaintext_alert: _, clock: _,
            // The hold hint is a paint-only standing note on a `Tick` timer: no input, reveal or
            // focus decision reads it, and it is not hashed (the way `status_ms` is not).
            hold_hint: _ } = self;
        c.f32(self.snap_target)
            .f32(self.snap.pos).f32(self.snap.vel)
            .f32(self.hero_flip_cd)
            .f32(self.hero_auto)
            .bool(self.covered)
            .option(self.visible_activation, |c, elem| {
                c.u32(elem);
            })
            .bool(self.cta_available)
            .bool(self.strip_chosen)
            .u32(self.next_group)
            .u32(self.next_elem)
            .seq(self.groups.len());
        for key in &self.groups {
            write_hub_identity(&key.identity, c);
            c.u32(key.group);
        }
        c.seq(self.items.len());
        for key in self.items.iter() {
            write_item_identity(&key.identity, c);
            c.u32(key.elem).u32(key.last_row).u32(key.last_col);
        }
        c.option(self.carousel.as_ref(), |c, (sid, rk)| {
            c.u32(u32::from(sid.raw())).str(rk);
        });
        c.option(self.outgoing.as_ref(), |c, (sid, rk)| {
            c.u32(u32::from(sid.raw())).str(rk);
        });
        c.f32(self.hero_slide.pos).f32(self.hero_slide.vel).f32(self.hero_dir);
        self.hero_pop.write_motion(c);
        c.option(self.projected_generation, |c, generation| { c.u32(generation); });
        c.f32(self.grid.scroll_y.pos).f32(self.grid.scroll_y.vel)
            .f32(self.grid.scroll_target).seq(self.rows.len());
        for (index, row) in self.rows.iter().enumerate() {
            write_hub_identity(&row.identity, c);
            c.u32(row.group.0).seq(row.elems.len());
            for elem in &row.elems { c.u32(*elem); }
            // a row with no shelf yet (the grid trails the rows by a sync) hashes as a resting one
            match self.grid.shelves.get(index) {
                Some(shelf) => shelf.write(c),
                None => Shelf::new(self.entry, &RowStyle::HOME).write(c),
            }
        }
        c.seq(self.restored_scroll.len());
        for &(group, scroll) in &self.restored_scroll { c.u32(group).f32(scroll); }
        c.bool(self.restore_reveal);
    }

    fn probe(&self, out: &mut String) {
        out.push_str("home");
    }
}

impl<H: HomeLike> Focusable<H> for HomeScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.plaintext_alert.groups(out) {
            return;
        }
        let view = H::hubs(cx);
        let hero_len = self.hero_len(view);
        let hero_end = self
            .hero_button_rect(view, self.hero_last_slot(view), cx.measure)
            .unwrap_or(Rect::new(MARGIN_X, HERO_ROW_Y, 0.0, HERO_CTRL_D));
        out.push(GroupSpec {
            id: HERO_GROUP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            // The hero is reached only through declared strip/first-shelf links. Its fixed
            // billboard extent remains on the page while folded and must never win a geometric
            // search from a lower shelf.
            reachable: AxisMask(0),
            edge: [
                EdgeRule::Geometric,
                EdgeRule::Geometric,
                EdgeRule::Screen,
                EdgeRule::Screen,
            ],
            extent: Rect::new(
                MARGIN_X,
                HERO_ROW_Y,
                hero_end.x + hero_end.w - MARGIN_X,
                HERO_CTRL_D,
            ),
            len: hero_len,
            elem: ElemKind::Control,
        });
        let band_row = self.band_row(cx.focus.current, 0);
        for (row, projection) in self.rows.iter().enumerate() {
            let top = GRID_TOP_Y + shelf_top_banded(row, band_row, self.band_open_at(band_row)) - self.grid.scroll_target;
            let linked = self.linked(row).is_some();
            if linked {
                if let Some(rect) =
                    self.heading_rect(view, row, cx.measure, At::SpringTarget, cx.focus.current)
                {
                    out.push(linked_heading::group_spec(heading_group(projection.group), rect));
                }
            }
            out.push(GroupSpec {
                id: projection.group,
                kind: GroupKind::Row { wrap: false },
                // DOWN from the linked heading returns to the card this shelf remembered;
                // every other door keeps the shelf's nearest-column projection.
                seat: if linked {
                    linked_heading::shelf_seat(
                        &cx.focus,
                        heading_elem(projection.group),
                        projection.group,
                        Seat::Nearest,
                    )
                } else {
                    Seat::Nearest
                },
                reachable: AxisMask::BOTH,
                edge: [
                    EdgeRule::Geometric,
                    EdgeRule::Geometric,
                    EdgeRule::Stop,
                    EdgeRule::Stop,
                ],
                extent: Rect::new(
                    MARGIN_X,
                    top + CARD_DY,
                    SCR_W - 2.0 * MARGIN_X,
                    CARD_H + band_h_of(projection.identity == HomeHubIdentity::ContinueWatching),
                ),
                len: projection.elems.len(),
                elem: ElemKind::Card,
            });
        }
    }

    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        if let Some(answer) = self.plaintext_alert.group_of(*key) {
            return answer;
        }
        match self.locate(*key)? {
            Located::Hero(slot) => self.hero_has(H::hubs(cx), slot).then_some(HERO_GROUP),
            Located::Item(row, _) => self.rows.get(row).map(|r| r.group),
            Located::Heading(row) => self.rows.get(row).map(|r| heading_group(r.group)),
        }
    }

    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        if let Some(step) = self.plaintext_alert.neighbour(key, dir) {
            return step;
        }
        let (slots, n) = self.hero_slots(H::hubs(cx));
        let next = match self.locate(key.elem) {
            Some(Located::Hero(slot)) => {
                // Left and Right walk the row in its drawn order, not in slot order.
                let at = slots[..n].iter().position(|&s| s == slot);
                match (dir, at) {
                    (Dir::Left, Some(at)) if at > 0 => Some(hero_elem(slots[at - 1])),
                    (Dir::Right, Some(at)) if at + 1 < n => Some(hero_elem(slots[at + 1])),
                    _ => None,
                }
            }
            Some(Located::Item(row, col)) => match dir {
                Dir::Left if col > 0 => self
                    .rows
                    .get(row)
                    .and_then(|r| r.elems.get(col - 1))
                    .copied(),
                Dir::Right => self
                    .rows
                    .get(row)
                    .and_then(|r| r.elems.get(col + 1))
                    .copied(),
                _ => None,
            },
            // One element, LEFT/RIGHT inert; UP/DOWN are the group's doors.
            Some(Located::Heading(_)) | None => None,
        };
        next.map(|elem| {
            Step::Move(FocusKey {
                entry: key.entry,
                elem,
            })
        })
        .unwrap_or(Step::Edge)
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        if let Some(placed) = self.plaintext_alert.place(*key) {
            return placed;
        }
        let view = H::hubs(cx);
        match self.locate(*key)? {
            Located::Hero(index) => {
                let mut base = self.hero_button_rect(view, index, cx.measure)?;
                if status_read(view).is_some() {
                    // StatusOverlay's lone action has no focus pop or press scale by design.
                    return Some(Placed { rect: base, rest_rect: base, clip: Rect::FULL,
                        index: Some(index as u32) });
                }
                if matches!(at, At::Drawn) {
                    base.x += self.slide_offsets().map_or(0.0, |(_, incoming)| incoming);
                }
                let scale = match at {
                    At::Drawn => self.hero_pop.scale_with(index, cx.press.scale),
                    At::SpringTarget => {
                        if self.focused_loc(cx.focus.current) == Some(Located::Hero(index)) {
                            plx_ui::widgets::CTRL_FOCUS_SCALE
                        } else {
                            1.0
                        }
                    }
                };
                Some(Placed {
                    rect: base.scaled(scale),
                    rest_rect: base.scaled(plx_ui::widgets::CTRL_FOCUS_SCALE),
                    clip: Rect::FULL,
                    index: Some(index as u32),
                })
            }
            Located::Heading(row) => {
                let rect = self.heading_rect(view, row, cx.measure, at, cx.focus.current)?;
                Some(Placed {
                    rect,
                    rest_rect: rect,
                    clip: Rect::new(
                        0.0,
                        plx_ui::widgets::TOP_BAR_BOTTOM,
                        SCR_W,
                        SCR_H - plx_ui::widgets::TOP_BAR_BOTTOM,
                    ),
                    index: None,
                })
            }
            Located::Item(row, _) => {
                let shelf = self.grid.shelves.get(row)?;
                if matches!(at, At::Drawn) {
                    return shelf.place(cx, &self.cards(view, row), key, self.section(row), At::Drawn);
                }
                // Where it will settle: the dive's destination, the settled layout.
                let col = self.item_col(row, *key)?;
                let focus_row = self
                    .focused_grid(cx.focus.current)
                    .map(|(r, _)| r)
                    .unwrap_or(row);
                let y = GRID_TOP_Y + shelf_top_settled(row, focus_row, self.band_open_at(Some(focus_row))) - self.grid.scroll_target;
                let base = Rect::new(
                    card_x(col, self.grid.scroll_x(row)),
                    y + CARD_DY,
                    CARD_W,
                    CARD_H,
                );
                let scale = if self.focused_grid(cx.focus.current) == Some((row, col)) {
                    RowStyle::HOME.focus_scale
                } else {
                    1.0
                };
                Some(Placed {
                    rect: base.scaled(scale),
                    rest_rect: base.scaled(RowStyle::HOME.focus_scale),
                    clip: self.section(row).clip,
                    index: Some(col as u32),
                })
            }
        }
    }

    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.plaintext_alert.reconcile(want) {
            return key;
        }
        if let Some(located) = self.locate(want.elem) {
            let valid = match located {
                Located::Hero(slot) => self.hero_has(H::hubs(cx), slot),
                Located::Item(_, _) | Located::Heading(_) => true,
            };
            if valid {
                return want;
            }
            // The hero rotated to a title with no disc under a focus that was on it: Play, the
            // control the disc sits beside, rather than a slide down onto the shelves.
            if matches!(located, Located::Hero(_)) {
                return FocusKey { entry: want.entry, elem: HERO_PLAY_ELEM };
            }
        }
        if let Some(elem) = self.next_episode_of(want.elem) {
            return FocusKey { entry: want.entry, elem };
        }
        if let Some(saved) = self.items.iter().find(|k| k.elem == want.elem) {
            let hub_identity = match &saved.identity {
                HomeItemIdentity::Item { hub, .. } | HomeItemIdentity::Slot { hub, .. } => hub,
            };
            if let Some(row) = self.rows.iter().find(|r| &r.identity == hub_identity)
                .or_else(|| self.rows.get((saved.last_row as usize).min(self.rows.len().saturating_sub(1)))) {
                let col = (saved.last_col as usize).min(row.elems.len().saturating_sub(1));
                if let Some(&elem) = row.elems.get(col) {
                    return FocusKey {
                        entry: want.entry,
                        elem,
                    };
                }
            }
        }
        self.rows
            .iter()
            .find_map(|r| r.elems.first().copied())
            .map(|elem| FocusKey {
                entry: want.entry,
                elem,
            })
            .unwrap_or(FocusKey {
                entry: want.entry,
                elem: HERO_PLAY_ELEM,
            })
    }

    fn seat(&self, group: GroupId, from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        if let Some(key) = self.plaintext_alert.seat(group, self.entry) {
            return key;
        }
        if group == HERO_GROUP {
            return FocusKey {
                entry: self.entry,
                elem: HERO_PLAY_ELEM,
            };
        }
        if let Some(row) = self.rows.iter().find(|r| heading_group(r.group) == group) {
            return FocusKey {
                entry: self.entry,
                elem: heading_elem(row.group),
            };
        }
        let Some((row_index, row)) = self.rows.iter().enumerate().find(|(_, r)| r.group == group)
        else {
            return FocusKey {
                entry: self.entry,
                elem: HERO_PLAY_ELEM,
            };
        };
        let col = ui_cards::column_near_x(
            from.rect.cx(),
            MARGIN_X,
            CARD_W + GAP,
            CARD_W,
            self.grid.scroll_x(row_index),
            row.elems.len(),
            from.index.unwrap_or(0) as usize,
        );
        FocusKey {
            entry: self.entry,
            elem: row.elems.get(col).copied().unwrap_or(HERO_PLAY_ELEM),
        }
    }
}

/// What a host test outside this crate may read of the Continue Watching row: its elements in
/// column order, and whether its landing is still playing (an animation, or a ghost still fading).
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, PartialEq, Eq)]
pub struct HomeProbe {
    pub cw_elems: Vec<u32>,
    pub animating: bool,
    /// The hero's bookmark disc: the title it was last fed a membership for, and the membership it
    /// wears (`Unknown` and `Off` draw the outline, `On` the filled face).
    pub disc: Option<([u8; 12], plx_data::catalog::Membership)>,
}

#[cfg(any(test, feature = "test-support"))]
impl HomeScreen {
    pub fn probe_for_test(&self) -> HomeProbe {
        let row = self.rows.iter().position(|r| r.identity == HomeHubIdentity::ContinueWatching);
        HomeProbe {
            cw_elems: row.map(|row| self.rows[row].elems.clone()).unwrap_or_default(),
            animating: row.is_some_and(|row| self.grid.shelves[row].landing_pending() || self.grid.shelves[row].has_ghosts()),
            disc: self.disc.of.map(|hex| (hex, self.disc.flip.membership())),
        }
    }
}

impl HomeScreen {
    /// The column of `elem` in row `row`.
    fn item_col(&self, row: usize, elem: u32) -> Option<usize> {
        self.elem_at.get(&elem).filter(|&&(r, _)| r as usize == row).map(|&(_, c)| c as usize)
    }

    /// Tell every row's shelf what moved (`FocusMoved`): it keeps no focus of its own and reads
    /// the engine's at the next tick. Presses and holds are NOT routed through it; they stay the
    /// page's own `activate` and `emit_item_menu`, on the element the engine holds.
    fn feed_shelves<H: HomeLike>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let view = H::hubs(cx);
        let mut shelves = std::mem::take(&mut self.grid.shelves);
        for (row, shelf) in shelves.iter_mut().enumerate() {
            shelf.on(ev, cx, &self.cards(view, row), fx);
        }
        self.grid.shelves = shelves;
    }

    fn cancel_row_page<H: HomeLike>(&mut self, row: usize, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let requested = self.grid.shelves.get(row).is_some_and(ui_cards::Shelf::has_page_request);
        if let Some(shelf) = self.grid.shelves.get_mut(row) { shelf.reset_page_requests(); shelf.drop_page_flight(); }
        if !requested { return; }
        if let Some(hub) = self.hub(H::hubs(cx), row).filter(|hub| hub.more || hub.offset > 0) {
            if let Some(cmd) = hub_page_cmd(&hub, |sid, id, key| HubsCmd::CancelPage { sid, id, key }) {
                fx.push(Fx::App(AppFx::Store(StoreId::Hubs, StoreCmd::Hubs(cmd))));
            }
        }
    }

    fn activation_elem(&self, engine_elem: u32) -> u32 {
        let engine_grid = self.locate(engine_elem).is_some_and(Located::on_grid);
        let picture_grid = self.snap.pos >= 0.5;
        if engine_grid != picture_grid {
            self.visible_activation.unwrap_or(engine_elem)
        } else {
            engine_elem
        }
    }

    /// The hero controls in row order, as slots, and how many: the failure read-out's lone action,
    /// else Play, the bookmark disc when the selected title has one, then Info.
    fn hero_slots(&self, view: HubsView<'_>) -> ([usize; HERO_NBTN], usize) {
        match status_read(view) {
            Some((_, _, action)) => ([0, 0, 0], usize::from(action.is_some())),
            None => self.selected_hero(view).map_or(([0, 1, 0], 2), |h| item_slots(h.item)),
        }
    }

    fn hero_len(&self, view: HubsView<'_>) -> usize {
        self.hero_slots(view).1
    }

    fn hero_has(&self, view: HubsView<'_>, slot: usize) -> bool {
        let (slots, n) = self.hero_slots(view);
        slots[..n].contains(&slot)
    }

    /// The slot at the row's end (the group's focus extent).
    fn hero_last_slot(&self, view: HubsView<'_>) -> usize {
        let (slots, n) = self.hero_slots(view);
        slots[n.saturating_sub(1)]
    }

    /// The title the hero shows now, for the application's Watchlist feed. `None` while the
    /// failure read-out or a title with no Watchlist identity is up.
    pub fn watchlist_hex(&self) -> Option<[u8; 12]> {
        self.disc.shown
    }

    /// The application's answer: `hex`'s membership on the Watchlist (`Unknown` while the list is
    /// unread, which wears the Add face). See [`plx_appkit::watchlist_disc::Disc::set`] for when this flips and when it snaps.
    pub fn set_watchlist(&mut self, hex: [u8; 12], membership: plx_data::catalog::Membership) {
        self.disc.set(hex, membership);
    }

    fn hero_button_rect(
        &self,
        view: HubsView<'_>,
        index: usize,
        measure: &dyn Measure,
    ) -> Option<Rect> {
        if let Some((_, _, action)) = status_read(view) {
            if index != 0 || action.is_none() {
                return None;
            }
            return status_overlay(view, &self.plaintext, &self.clock)?.action_frame_measured(measure);
        }
        let hero = self.selected_hero(view)?.item;
        let resumes = plx_data::metadata::resume_ns(hero.resume_ms, hero.dur_ns / 1_000_000) > 0;
        let label = plx_appkit::hero_layout::play_label(resumes);
        let rects = hero_rects(measure, hero, label);
        match index {
            0 => Some(rects.play),
            1 => Some(rects.info),
            2 => rects.disc,
            _ => None,
        }
    }
}

impl<H: HomeLike> Machine<H> for HomeScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match self.plaintext_alert.step(ev, cx) {
            AlertStep::Pass => {}
            AlertStep::Done(handled) => return handled,
            AlertStep::Answer(cmd) => {
                if let Some(cmd) = cmd {
                    fx.push(Fx::App(AppFx::Session(cmd)));
                }
                self.reseat(FocusTarget::ContainerGroup(HERO_GROUP), cx, fx);
                fx.invalidate(Provenance::Input);
                return Handled::Yes;
            }
        }
        if let ScreenEvent::Input(InputEvent { at, kind: InputKind::Key { key, edge, .. }, .. }) = ev {
            self.backdrop.note_key(*key, *edge, at.ms);
        }
        match ev {
            ScreenEvent::Mount => {
                self.sync_catalog(cx);
                Handled::Yes
            }
            ScreenEvent::RestoreMemory(PageMemory::Home(memory)) => {
                self.adopt_shows(memory, H::hubs(cx));
                self.restore(memory);
                self.sync_catalog(cx);
                self.cta_available = self.hero_len(H::hubs(cx)) > 0;
                Handled::Yes
            }
            ScreenEvent::Enter(_) => {
                self.sync_catalog(cx);
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Cover => {
                for row in 0..self.rows.len() { self.cancel_row_page(row, cx, fx); }
                self.covered = true;
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                self.covered = false;
                self.hero_auto = HERO_AUTO_S;
                self.sync_catalog(cx);
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Tick(tick) => {
                self.tick(*tick, cx, fx);
                Handled::Yes
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == StoreId::Hubs.ord() => {
                for shelf in &mut self.grid.shelves { shelf.reset_page_requests(); }
                self.projected_generation = None;
                self.sync_catalog(cx);
                if self.rows.is_empty() {
                    self.snap_target = 0.0;
                }
                fx.invalidate(Provenance::Landing(fx.from()));
                Handled::Yes
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                if let Some((row, _)) = self.focused_grid(*from) {
                    if self.focused_grid(Some(*to)).is_none_or(|(next, _)| next != row) {
                        self.cancel_row_page(row, cx, fx);
                    } else if let Some(shelf) = self.grid.shelves.get_mut(row) {
                        // A page the store gave up on leaves no landing to clear the latch.
                        shelf.reset_page_requests();
                    }
                }
                self.feed_shelves(ev, cx, fx);
                let from_loc = from.and_then(|key| self.locate(key.elem));
                let to_loc = self.locate(to.elem);
                if let (Some(a), Some(b)) = (from_loc, to_loc) {
                    if a.on_grid() != b.on_grid() {
                        self.visible_activation = from.map(|key| key.elem);
                    }
                }
                match to_loc {
                    Some(Located::Hero(_)) => self.snap_target = 0.0,
                    Some(Located::Item(_, _) | Located::Heading(_)) => self.snap_target = 1.0,
                    None => {}
                }
                if matches!(by, By::Restore) && self.restore_reveal {
                    // A restored page opens at its saved region, rather than replaying the
                    // hero-to-grid door with an invisible engine cursor during the transition.
                    self.restore_reveal = false;
                    self.snap.jump(self.snap_target);
                    self.visible_activation = None;
                    if let Some(Located::Item(row, col)) = to_loc {
                        // The saved viewport is a preference, not an assertion about today's
                        // catalog order. Rest the reconciled item's shelf before first draw.
                        if let (Some(shelf), Some(hub)) =
                            (self.grid.shelves.get_mut(row), self.rows.get(row))
                        {
                            let count = hub.elems.len();
                            let scroll = ui_cards::scroll_into_view(shelf.scroll(), col, count,
                                CARD_W, GAP, SCR_W - 2.0 * MARGIN_X);
                            shelf.restore_scroll(scroll, count);
                        }
                        self.grid.scroll_target = rest_scroll(row, self.rows.len(), self.band_open_at(self.rows.len().checked_sub(1)));
                        self.grid.scroll_y.jump(self.grid.scroll_target);
                    }
                    self.layout_grid();
                }
                if to.elem >= plx_ui::dispatch::STRIP_BASE && matches!(by, By::Dir | By::Pointer)
                {
                    self.strip_chosen = true;
                } else if to_loc.is_some() {
                    self.strip_chosen = false;
                }
                if matches!(by, By::Dir | By::Pointer) {
                    self.hero_auto = HERO_AUTO_S;
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(elem) = cx
                    .focus
                    .current
                    .filter(|k| k.entry == self.entry)
                    .map(|k| k.elem)
                {
                    self.activate(elem, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => {
                self.activate(*elem, cx, fx);
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => self.emit_item_menu(cx, fx),
            ScreenEvent::App(AppMsg::Home(command)) => {
                self.sync_catalog(cx);
                self.command(*command, cx, fx)
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key,
                        edge: Edge::Down | Edge::Repeat,
                        at_edge,
                        ..
                    },
                ..
            }) if *at_edge && matches!(key, Key::Left | Key::Right) => {
                let direction = if *key == Key::Left { -1 } else { 1 };
                if let Some((row, _)) = self.focused_grid(cx.focus.current) {
                    if let Some(shelf) = self.grid.shelves.get_mut(row) { shelf.reset_page_requests(); }
                }
                if matches!(self.focused_loc(cx.focus.current), Some(Located::Hero(_)))
                    && self.flip(H::hubs(cx), direction)
                {
                    fx.invalidate(Provenance::Input);
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key: Key::Back,
                        edge: Edge::Down,
                        ..
                    },
                ..
            }) => {
                if self.snap_target >= 0.5
                    || self.snap.pos >= 0.5
                    || self.focused_loc(cx.focus.current).is_some_and(Located::on_grid)
                {
                    self.snap_target = 0.0;
                    self.visible_activation = cx
                        .focus
                        .current
                        .filter(|key| key.entry == self.entry)
                        .map(|_| HERO_PLAY_ELEM);
                    fx.push(Fx::App(AppFx::Home(HomeReq::FoldToHero)));
                    fx.invalidate(Provenance::Input);
                } else {
                    fx.push(Fx::App(AppFx::Loop(LoopReq::BackAtRoot)));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: HomeLike> Screen<H> for HomeScreen {
    /// A grid card only: the hero and a heading are not a card, so an opener has no rect for them.
    fn focused_card<'a>(&self, cx: &Cx<'a, H>, focus: Option<plx_machine::machine::FocusKey<u32>>, at: Option<At>) -> Option<plx_ui::screen::FocusedCard<'a>> {
        let key = focus.filter(|k| k.entry == self.entry)?;
        self.focused_grid(Some(key))?;
        let item = self.focused_item(Some(key), cx)?;
        let placed = at.and_then(|at| Focusable::<H>::place(self, &key.elem, cx, at));
        Some(plx_ui::screen::FocusedCard::new(item, placed))
    }
    fn focused_card_rect(&self, cx: &Cx<'_, H>, focus: Option<plx_machine::machine::FocusKey<u32>>, at: At) -> Option<Rect> {
        HomeScreen::focused_rect::<H>(self, focus, cx, at)
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<plx_machine::machine::FocusKey<u32>>) {
        HomeScreen::redraw_focused::<H>(self, f, focus)
    }
    fn name(&self) -> &'static str {
        super::registry::word::HOME
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        self.draw_page(f);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn strip_reachable(&self) -> bool {
        self.snap.pos < 0.5
    }
    fn links(&self, out: &mut Vec<Link>) {
        out.push(Link {
            from: plx_ui::containers::tabs::STRIP,
            dir: Dir::Down,
            to: HERO_GROUP,
        });
        out.push(Link {
            from: HERO_GROUP,
            dir: Dir::Up,
            to: plx_ui::containers::tabs::STRIP,
        });
        // A collection shelf's linked heading sits between the shelf above and its own cards:
        // UP from any card reaches it, DOWN returns to the remembered card, and it is the next
        // stop DOWN from the shelf above. The engine takes the FIRST matching link, so these
        // precede the first shelf's UP door to the hero.
        for (row, projection) in self.rows.iter().enumerate() {
            if projection.link.is_none() {
                continue;
            }
            let heading = heading_group(projection.group);
            out.extend(linked_heading::links(heading, projection.group));
            let above = row.checked_sub(1).map_or(HERO_GROUP, |r| self.rows[r].group);
            out.push(Link { from: heading, dir: Dir::Up, to: above });
            out.push(Link { from: above, dir: Dir::Down, to: heading });
        }
        if let Some(first) = self.rows.first() {
            out.push(Link {
                from: HERO_GROUP,
                dir: Dir::Down,
                to: first.group,
            });
            out.push(Link {
                from: first.group,
                dir: Dir::Up,
                to: HERO_GROUP,
            });
        }
    }
    fn memory(&self) -> H::Memory {
        PageMemory::Home(HomeMemory {
            groups: self.groups.clone(),
            items: Arc::clone(&self.items),
            next_group: self.next_group,
            next_elem: self.next_elem,
            carousel: self.carousel.clone(),
            strip_chosen: self.strip_chosen,
            scroll_y: self.grid.scroll_y.pos,
            row_scroll: self.rows.iter().enumerate()
                .map(|(index, row)| (row.group.0, self.grid.scroll_x(index)))
                .chain(self.restored_scroll.iter().copied()).collect(),
            // Column order: a hash map's order would make two memories of one page differ.
            cw_shows: self.rows.iter().find(|r| r.identity == HomeHubIdentity::ContinueWatching)
                .map_or_else(Vec::new, |row| row.elems.iter()
                    .filter_map(|&elem| {
                        let card = self.cw_cards.get(&elem)?;
                        Some((elem, card.sid, card.rk.clone(), show_of(card)?.to_owned()))
                    })
                    .collect()),
            cw_offset: self.cw_offset,
        })
    }
    fn memory_at(&self, _focus: Option<FocusKey<u32>>) -> H::Memory {
        <HomeScreen as Screen<H>>::memory(self)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

fn write_hub_identity(identity: &HomeHubIdentity, c: &mut Canon) {
    match identity {
        HomeHubIdentity::ContinueWatching => {
            c.u32(0);
        }
        HomeHubIdentity::Identifier { sid, id, key } => {
            c.u32(1).u32(u32::from(sid.raw())).str(id).str(key);
        }
        HomeHubIdentity::Key { sid, key } => {
            c.u32(2).u32(u32::from(sid.raw())).str(key);
        }
        HomeHubIdentity::Ephemeral {
            generation,
            ordinal,
        } => {
            c.u32(3).u32(*generation).u32(*ordinal);
        }
    }
}

fn write_item_identity(identity: &HomeItemIdentity, c: &mut Canon) {
    match identity {
        HomeItemIdentity::Item { hub, sid, rk } => {
            c.u32(0);
            write_hub_identity(hub, c);
            c.u32(u32::from(sid.raw())).str(rk);
        }
        HomeItemIdentity::Slot {
            hub,
            generation,
            ordinal,
        } => {
            c.u32(1);
            write_hub_identity(hub, c);
            c.u32(*generation).u32(*ordinal);
        }
    }
}

fn pinned_snap(target: f32, rows: usize) -> f32 {
    if rows == 0 {
        0.0
    } else {
        target
    }
}

fn status_read(
    view: HubsView<'_>,
) -> Option<(
    &'static std::ffi::CStr,
    StatusKind,
    Option<&'static std::ffi::CStr>,
)> {
    if view.hub_count() > 0 {
        return None;
    }
    Some(match view.state {
        plx_data::pms::HubState::Loading => {
            // placeholder-exempt: builds the value only; counted where drawn (StatusOverlay::draw_geometry, Working)
            (plx_platform::i18n::msg::browse_home_loading_c(), StatusKind::Working, None)
        }
        plx_data::pms::HubState::Failed => (
            plx_platform::i18n::msg::browse_home_failed_c(),
            StatusKind::Failed,
            Some(plx_platform::i18n::msg::browse_action_retry_c()),
        ),
        plx_data::pms::HubState::Ready => (
            plx_platform::i18n::msg::browse_home_empty_c(),
            StatusKind::Empty,
            Some(plx_platform::i18n::msg::browse_home_refresh_c()),
        ),
    })
}

/// **The hub read-out, built ONCE for its draw and its hit rect** — the same kind, caption and
/// action on both, so the pill a click lands in is the pill on screen. It fills the page, so a
/// failure stands on the shared page lines (`StatusOverlay::page`), level with the sign-in
/// failure's and a Library section's; loading and the empty answer stay centred.
///
/// A failure while discovery offers "Connect without encryption?" for a server (`plaintext`)
/// says why in the reason slot and — until it is answered — makes *Connect* the primary
/// (`screens::plaintext_question`, shared with the sign-in and a Library source's read-out).
fn status_overlay<'a>(view: HubsView<'_>, plaintext: &'a OfferWatch, clock: &ClockWatch) -> Option<StatusOverlay<'a>> {
    let (caption, kind, action) = status_read(view)?;
    // Home's hub failure carries no typed cause of its own — it is the same untyped "can't reach
    // the server" verdict as the Library's own, so the two share the glyph rather than one
    // inventing a cause the other doesn't have.
    let mut overlay = StatusOverlay::new(Rect::FULL, caption, kind).page(plx_ui::icons::Icon::ServerBadgeMinus);
    let mut action = action;
    if kind == StatusKind::Failed {
        if let (Some(verdict), Some(reason)) = (plaintext.verdict(), plaintext.reason()) {
            overlay = overlay.reason(reason);
            action = Some(plaintext_question::primary(Some(verdict)));
        } else if let Some((reason, glyph)) = clock.reason() {
            // No offer to answer: a wrong clock is the one cause the read-out can name, and it
            // brings its own glyph (the same placement, a different mark).
            overlay = overlay.reason(reason).page(glyph);
        }
    }
    Some(match action {
        Some(label) => overlay.action(label),
        None => overlay,
    })
}


/// The focus element of hero control `slot`.
fn hero_elem(slot: usize) -> u32 {
    match slot {
        0 => HERO_PLAY_ELEM,
        1 => HERO_INFO_ELEM,
        _ => HERO_WATCHLIST_ELEM,
    }
}

/// The hero controls an item draws, in ROW order, as slots: Play, the bookmark disc when the item
/// stands for a Watchlist title, then Info. The disc is second, as on Detail.
fn item_slots(item: &PmsMovie) -> ([usize; HERO_NBTN], usize) {
    if disc_hex(item).is_some() { ([0, 2, 1], 3) } else { ([0, 1, 0], 2) }
}

/// The catalog id the hero's bookmark disc acts on: the item's own, or its show's for a season or
/// an episode. `None` (no `plex://movie|show` guid, or a collection) means no disc at all, which is
/// what Detail does too; Home does not resolve a missing id through the server record as the item
/// menu does, because the disc's presence moves the focus row and must not appear a beat late.
fn disc_hex(item: &PmsMovie) -> Option<[u8; 12]> {
    item.watchlist_subject().and_then(|(_, hex, _)| hex)
}

/// The bookmark disc's Home half: which title the shared [`plx_appkit::watchlist_disc::Disc`] is
/// for, and the title that was shown before it (the page sliding out keeps its face). The hero
/// rotates through many titles on the one disc; everything about the flip itself is the `Disc`'s.
/// Fed by the application.
#[derive(Default)]
struct HeroDisc {
    /// The title the application last fed a membership for.
    of: Option<[u8; 12]>,
    /// That title's membership and flip, stepped toward its face.
    flip: plx_appkit::watchlist_disc::Disc,
    /// The previous title and whether it wore the filled face, for the page sliding out.
    prev: Option<([u8; 12], bool)>,
    /// The title the hero shows now, cached each tick for the application's feed.
    shown: Option<[u8; 12]>,
}

impl HeroDisc {
    /// What a press on the disc of `item` asks for, `None` when it names no title.
    fn press(&mut self, item: &PmsMovie) -> Option<crate::registry::ItemMenuAction> {
        use crate::registry::ItemMenuAction as Action;
        let (kind, hex, _) = item.watchlist_subject()?;
        let hex = hex?;
        let on = self.of == Some(hex) && self.flip.membership() == plx_data::catalog::Membership::On;
        self.flip.press();
        Some(if on {
            Action::WatchlistRemove(plx_data::watchlist::guid_of(kind, &hex))
        } else {
            Action::WatchlistAdd { hex }
        })
    }

    /// The membership of `hex`. A different title (the hero rotated) rests the disc on its face and
    /// keeps the title that left as `prev`; the same title is the `Disc`'s own rule
    /// ([`plx_appkit::watchlist_disc::Disc::set`]), so the flip plays only on a real membership
    /// change of the shown title.
    fn set(&mut self, hex: [u8; 12], m: plx_data::catalog::Membership) {
        if self.of == Some(hex) {
            self.flip.set(m);
        } else {
            self.prev = self.of.map(|h| (h, plx_appkit::watchlist_disc::target(self.flip.membership()) >= 1.0));
            self.flip.reset(m);
            self.of = Some(hex);
        }
    }

    /// The flip progress the page of the title `hex` draws its disc at.
    fn progress(&self, hex: [u8; 12]) -> f32 {
        match (self.of, self.prev) {
            (Some(h), _) if h == hex => self.flip.pos(),
            (_, Some((h, filled))) if h == hex => f32::from(filled),
            _ => 0.0,
        }
    }

    /// Step the flip toward the face of the title it belongs to.
    fn step(&mut self, dt: f32) {
        if self.of.is_some() {
            self.flip.step(dt);
        }
    }
}

/// Where each hero control sits for one hero item (shared by the draw, the hit rects and the focus
/// extent, so they cannot disagree).
struct HeroRects {
    play: Rect,
    disc: Option<Rect>,
    info: Rect,
}

fn hero_rects(measure: &dyn Measure, item: &PmsMovie, label: &std::ffi::CStr) -> HeroRects {
    // One row: Play, the watchlist disc when the title has one (absent: no room, no gap), Info.
    let has_disc = disc_hex(item).is_some();
    let items = [
        Item::new(plx_appkit::hero_layout::pill_w(measure, label, theme::size::BODY, true, None)),
        if has_disc { Item::new(HERO_CTRL_D) } else { Item::absent() },
        Item::new(HERO_CTRL_D),
    ];
    let row = HStack::new(&items, HERO_CTRL_GAP, Align::Leading(MARGIN_X));
    let at = |i: usize| row.span(i).map(|(x, w)| Rect::new(x, HERO_ROW_Y, w, HERO_CTRL_D));
    HeroRects {
        play: at(0).unwrap_or_default(),
        disc: at(1),
        info: at(2).unwrap_or_default(),
    }
}

fn card_x(col: usize, scroll: f32) -> f32 {
    MARGIN_X + col as f32 * (CARD_W + GAP) - scroll
}
#[cfg(test)]
fn col_at(x: f32, scroll: f32, count: usize) -> Option<usize> {
    (0..count).find(|&col| {
        let left = card_x(col, scroll);
        x >= left && x <= left + CARD_W
    })
}
fn heading_y(row_y: f32, lift: f32) -> f32 {
    row_y - TITLE_DY - lift
}
/// The caption policy a shelf here follows: the stored Under posters setting, and for the Continue
/// Watching deck (`deck`) its [`captions::deck_policy`](crate::registry::captions::deck_policy)
/// (the progress line is always shown, so the deck's band keeps a line under Nothing).
fn shelf_policy(deck: bool) -> plx_ui::caption_policy::CaptionPolicy {
    let policy = crate::registry::captions::policy();
    if deck { crate::registry::captions::deck_policy(&policy) } else { policy }
}
/// The focused shelf's caption band (0 / 80 / 112 px) under [`shelf_policy`]: the height the shelf
/// reserves, opens and reveals by (`Shelf::set_caption_policy`).
fn band_h_of(deck: bool) -> f32 {
    plx_ui::caption_policy::band_h(&shelf_policy(deck))
}
/// How far the focused band carries the rows below it down: the band of the shelf holding focus
/// (`deck` when it is the Continue Watching deck), per shelf and never per tile.
fn band_open_of(deck: bool) -> f32 {
    ui_cards::band_open_for(band_h_of(deck))
}
/// **The scroll a focused shelf RESTS at: the Library's rest rule** (`poster_grid::rest_scroll`,
/// `ui_cards::grid_rest_scroll`): the shelf's top comes to `GRID_TOP_Y`, the first shelf's own
/// resting line, and the document's end holds the last shelves lower. Every shelf above it has
/// scrolled off, so no heading but the focused one stands in the top band.
///
/// It was the NEAREST scroll that showed the focused shelf with its caption band open
/// (`ui_cards::reveal` between a `lo` and the shelf's top): Down came to rest at `lo`, which is
/// the caption's reach short of the bottom margin, so the focused shelf's y followed the Under
/// posters band (531 / 563 / 603 for 112 / 80 / 0) and the heading of the shelf above rested at
/// 22 / 54 / 94 with it, under the account chip at the two smaller bands, while Up came to rest at
/// the shelf's top. A rest that depends on the band and on the direction of the last move is not a
/// rest a heading can be kept out of the chrome by.
fn rest_scroll(row: usize, rows: usize, last_open: f32) -> f32 {
    ui_cards::grid_rest_scroll(row, ui_cards::ROW_PITCH_FIXED + ui_cards::LABEL_BAND_COLLAPSED, 0.0, grid_max_scroll(rows, last_open))
}
/// The document's end, with `last_open` the band the LAST shelf opens when focused.
fn grid_max_scroll(rows: usize, last_open: f32) -> f32 {
    let n = rows.max(1);
    let h = ui_cards::settled_top_for(n, Some(n - 1), ui_cards::ROW_PITCH_FIXED, last_open);
    (h - (SCR_H - CONTENT_Y) + 60.0).max(0.0)
}
fn shelf_top_settled(row: usize, focus_row: usize, open: f32) -> f32 {
    shelf_top_banded(row, Some(focus_row), open)
}
/// [`shelf_top_settled`] with the open label band named explicitly: `None` while a linked heading
/// holds focus, which opens no row's band.
/// `open` is the band the focused shelf opens ([`HomeScreen::band_open_at`]).
fn shelf_top_banded(row: usize, band_row: Option<usize>, open: f32) -> f32 {
    ui_cards::settled_top_for(row, band_row, ui_cards::ROW_PITCH_FIXED, open)
}

fn wash_corners(hero: Option<&PmsMovie>, grid: [[f32; 4]; 4], snap: f32) -> [[f32; 4]; 4] {
    let hero = hero
        .filter(|m| m.has_blur)
        .map(|m| AmbientWash::keyed(m.blur, HERO_WASH_W))
        .unwrap_or([theme::SURFACE_APP; 4]);
    let t = snap.clamp(0.0, 1.0);
    std::array::from_fn(|i| theme::mix(hero[i], grid[i], t))
}
fn wash_hidden(snap: f32, art: f32, outgoing: Option<f32>) -> bool {
    snap <= 0.005 && art > 0.995 && outgoing.map(|a| a > 0.995).unwrap_or(true)
}
fn reveal(tex: u32, spring: &Spring) -> f32 {
    if tex == 0 {
        0.0
    } else {
        spring.pos
    }
}
fn art_rect(tex: (u32, f32, f32), snap: f32, dx: f32) -> Rect {
    Rect::new(dx, -snap * (SCR_H - 120.0), SCR_W, SCR_H).cover(tex.1, tex.2)
}
fn backdrop_art(p: Painter, tex: (u32, f32, f32), snap: f32, dx: f32, alpha: f32) {
    if tex.0 == 0 || alpha <= 0.01 || !on_axis(dx, SCR_W, SCR_W, 0.0) {
        return;
    }
    p.tex(
        tex.0,
        art_rect(tex, snap, dx),
        0.0,
        theme::with_a(theme::TINT_WHITE, alpha * (1.0 - snap)),
    );
}
/// The hero's atmospheric ramp as the ground carries it — the two `rect`s the layered path draws
/// (`base_scrim_ramp`: nothing above `y0`, `mid` at the knee, `foot` at the panel's foot).
fn hero_ramp(hero_a: f32) -> plx_ui::widgets::WashRamp {
    let [y0, knee, mid, foot] = base_scrim_ramp(hero_a);
    plx_ui::widgets::WashRamp {
        ink: theme::scrim(1.0),
        stops: [(y0, 0.0), (knee, mid), (SCR_H, foot)],
    }
}
/// The un-sliding hero art as the layer [`AmbientWash::draw_ground`] carries — exactly what
/// [`backdrop_art`] would have drawn at `dx = 0`, down to its skip (a texture of 0 draws no art).
fn hero_art(tex: (u32, f32, f32), snap: f32, alpha: f32) -> plx_ui::widgets::WashArt {
    plx_ui::widgets::WashArt {
        tex: if alpha <= 0.01 { 0 } else { tex.0 },
        rect: art_rect(tex, snap, 0.0),
        uv: plx_gfx::gfx::UV_FULL,
        tint: theme::with_a(theme::TINT_WHITE, alpha * (1.0 - snap)),
    }
}
/// The show an episode card belongs to: `None` for a movie, a season, a collection or an episode
/// that names no show (the cards that have no "next one" to follow).
fn show_of(item: &PmsMovie) -> Option<&str> {
    (item.kind == 3 && !item.show_rk.is_empty()).then_some(item.show_rk.as_str())
}
fn hero_logo_rk(item: &PmsMovie) -> &str {
    if item.kind == 3 && !item.show_rk.is_empty() {
        &item.show_rk
    } else {
        &item.rk
    }
}
fn prefetch_order(cur: i32, n: i32, out: &mut [i32; 2 * HERO_PREFETCH]) -> usize {
    if n <= 1 {
        return 0;
    }
    let cur = cur.rem_euclid(n);
    let mut count = 0;
    for distance in 1..=HERO_PREFETCH as i32 {
        for step in [distance, -distance] {
            let index = (cur + step).rem_euclid(n);
            if index != cur && !out[..count].contains(&index) {
                out[count] = index;
                count += 1;
            }
        }
    }
    count
}
/// Is the billboard up, so its neighbours' backdrops are held ([`HomeScreen::prefetch`])?
fn neighbours_armed(snap: f32) -> bool {
    snap < 0.05
}
/// …and is it also settled, so the neighbours' logos may be warmed?
fn prefetch_armed(snap: f32, sliding: bool) -> bool {
    neighbours_armed(snap) && !sliding
}

fn display_source(real: &str) -> &str {
    static OVERRIDE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| plx_base::devtrig::read("shared"))
        .as_deref()
        .unwrap_or(real)
}

fn shared_by(source: &str) -> String {
    plx_ui::fmt::shared_by(source).unwrap_or_default()
}
fn meta_source_flow(
    base_w: f32,
    source: &str,
    mut run: impl FnMut(&str, f32, f32, i32, i32, [f32; 4]) -> f32,
) -> f32 {
    if source.is_empty() {
        return base_w;
    }
    let mut dx = base_w + SOURCE_PAD;
    dx += run(
        "\u{b7}",
        dx,
        META_FLOW_W - dx,
        theme::size::BODY,
        0,
        theme::TEXT_SEPARATOR,
    );
    dx += SOURCE_PAD;
    dx += run(
        &shared_by(source),
        dx,
        META_FLOW_W - dx,
        theme::size::BODY,
        0,
        theme::TEXT_TERTIARY,
    );
    dx
}
fn draw_meta_source(p: Painter, source: &str, x: f32, y: f32, base_w: f32, measure: &dyn Measure) {
    meta_source_flow(base_w, source, |text, dx, budget, size, bold, ink| {
        let elided = plx_gfx::text::elide_by(text, budget, false, |t| {
            measure.width_str(t, size, bold != 0)
        });
        let Ok(text) = CString::new(elided) else {
            return 0.0;
        };
        let mut label = Label::new(text.as_ptr(), size, ink).v(VAlign::CapTop);
        if bold == 1 {
            label = label.bold();
        }
        label.draw(p, Rect::new(x + dx, y, budget, 0.0))
    });
}
fn meta_drawn_w(view: &TextView<'_>, text: &str, width: f32, measure: &dyn Measure) -> f32 {
    if view.truncates(width) {
        return width;
    }
    measure.width_str(text, theme::size::BODY, false).min(width)
}
fn hero_content(hero: &PmsMovie, source: &str, p: Painter, dx: f32, measure: &dyn Measure) {
    if !on_axis(
        MARGIN_X + dx,
        if source.is_empty() {
            HERO_COL_W
        } else {
            META_FLOW_W
        },
        SCR_W,
        0.0,
    ) {
        return;
    }
    let episode = hero.kind == 3;
    let title = if episode && !hero.show_title.is_empty() {
        &hero.show_title
    } else {
        &hero.title
    };
    let title_h = hero_logo::band_h(LogoRung::Hero);
    let meta = if episode {
        let mut text = plx_ui::fmt::episode_address(i64::from(hero.season_index), i64::from(hero.ep_index));
        if !text.is_empty() && !hero.title.is_empty() {
            text.push_str(" \u{b7} ");
        }
        text.push_str(&hero.title);
        text
    } else {
        format!(
            "{} \u{b7} {} \u{b7} {}",
            if hero.kind == 1 { plx_platform::i18n::msg::browse_kind_show() } else { plx_platform::i18n::msg::browse_kind_movie() },
            hero.year,
            if hero.rating.is_empty() {
                plx_platform::i18n::msg::browse_detail_unrated()
            } else {
                &hero.rating
            }
        )
    };
    let meta_view = TextView::new(&meta, theme::size::BODY, theme::TEXT_SECONDARY).max_lines(1);
    let meta_h = meta_view.measure_h(HERO_COL_W);
    let synopsis = (!hero.summary.is_empty()).then(|| plx_ui::hero_synopsis(&hero.summary, ""));
    let stack = hero_stack(title_h, meta_h, synopsis.as_ref().map(|v| v.measure_h(HERO_COL_W)));
    HeroLogo::new(hero.sid.raw(), hero_logo_rk(hero), title, LogoRung::Hero)
        .draw(p, Rect::new(MARGIN_X, stack.title.y, HERO_COL_W, title_h), measure);
    let y = stack.meta.y;
    meta_view.draw(p, Rect::new(MARGIN_X, y, HERO_COL_W, 0.0));
    if !source.is_empty() {
        draw_meta_source(
            p,
            source,
            MARGIN_X,
            y,
            meta_drawn_w(&meta_view, &meta, HERO_COL_W, measure),
            measure,
        );
    }
    if let (Some(view), Some(span)) = (synopsis, stack.synopsis) {
        view.draw(p, Rect::new(MARGIN_X, span.y, HERO_COL_W, 0.0));
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod placeholder_tests;
#[cfg(test)]
pub(crate) mod cards_harness; // Tier 2 card conformance (cards_conformance_tests.rs)

/// The cards a landing removed, each under the element it had, in the order the shelf names them
/// (`Shelf::ghost_keys`); a card the screen did not keep is `None` and is not drawn.
struct GhostCards<'a> {
    cards: Vec<Option<&'a (u32, Arc<PmsMovie>)>>,
}

impl GhostCards<'_> {
    fn item(&self, i: usize) -> Option<&PmsMovie> {
        self.cards.get(i).copied().flatten().map(|(_, movie)| &**movie)
    }
}

impl<H: HomeLike> CardSource<H> for GhostCards<'_> {
    fn len(&self) -> usize {
        self.cards.len()
    }
    /// The card's own element; a ghost is never focused, pressed or registered, so nothing asks.
    fn elem(&self, i: usize) -> u32 {
        self.cards.get(i).copied().flatten().map_or(0, |(elem, _)| *elem)
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.cards.iter().position(|c| c.is_some_and(|(elem, _)| elem == e))
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Poster(self.item(i).map(tile_facts::of))
    }
    fn label(&self, _i: usize) -> ui_cards::TileLabel {
        ui_cards::TileLabel::default()
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.item(i).and_then(PmsMovie::resume_frac)
    }
    fn loaded(&self, i: usize) -> bool {
        self.item(i).is_some_and(|item| !item.rk.is_empty())
    }
}

/// One row's cards, as `cards::Shelf` reads them: the published hub's items under the elements
/// the page interned for them. The screen keeps the heading, the vertical flow, the focus groups,
/// the dive and every press action.
struct HomeCards<'a> {
    home: &'a HomeScreen,
    view: HubsView<'a>,
    row: usize,
    /// How the row's stops treat a passing pointer (one answer for the row, it shares one y).
    hover: Hover,
    paging: bool,
}

impl<'a> HomeCards<'a> {
    fn item(&self, i: usize) -> Option<&'a PmsMovie> {
        self.home.item_at(self.view, self.row, i)
    }
}

impl<H: HomeLike> CardSource<H> for HomeCards<'_> {
    fn more(&self) -> bool {
        self.paging && self.home.hub(self.view, self.row).is_some_and(|hub| hub.more)
    }
    fn page_epoch(&self) -> u32 {
        self.home.hub(self.view, self.row).map_or(0, |hub| hub.epoch)
    }
    /// The rows change only when a publication is projected.
    fn revision(&self) -> Option<u64> {
        self.home.projected_generation.map(u64::from)
    }
    /// The placeholder rule is not the paging rule: a row that continues shows it whether or not
    /// focus is near the edge that asks.
    fn more_after(&self) -> bool {
        self.home.hub(self.view, self.row).is_some_and(|hub| hub.more)
    }
    fn more_before(&self) -> bool {
        self.home.hub(self.view, self.row).is_some_and(|hub| hub.offset > 0)
    }
    fn len(&self) -> usize {
        self.home.rows.get(self.row).map_or(0, |r| r.elems.len())
    }

    /// The window's server offset plus the card's place in it.
    fn global(&self, i: usize) -> usize {
        self.home.hub(self.view, self.row).map_or(0, |hub| hub.offset) + i
    }

    fn elem(&self, i: usize) -> u32 {
        self.home.rows.get(self.row).and_then(|r| r.elems.get(i)).copied().unwrap_or_default()
    }

    fn index_of(&self, e: &u32) -> Option<usize> {
        self.home.item_col(self.row, *e)
    }

    /// The card the PICTURE shows focused: none until the dive has made the shelves the picture,
    /// and the card the page came from while the dive back to the billboard still shows them.
    fn focus_index(&self, e: &u32) -> Option<usize> {
        if self.home.snap.pos <= 0.5 {
            return None;
        }
        self.home.item_col(self.row, self.home.activation_elem(*e))
    }

    fn art(&self, i: usize) -> Art<'_> {
        Art::Poster(self.item(i).map(tile_facts::of))
    }

    fn label(&self, i: usize) -> ui_cards::TileLabel {
        let Some(item) = self.item(i) else { return ui_cards::TileLabel::default() };
        let deck = self.home.rows[self.row].identity == HomeHubIdentity::ContinueWatching;
        crate::registry::captions::shelf_label(&crate::registry::captions::policy(), item, deck)
    }

    fn progress(&self, i: usize) -> Option<f32> {
        self.item(i).and_then(PmsMovie::resume_frac)
    }

    /// A card has landed once it has a rating key; the ring's placeholder slot is an empty movie.
    fn loaded(&self, i: usize) -> bool {
        self.item(i).is_some_and(|item| !item.rk.is_empty())
    }

    fn hover(&self, _i: usize) -> Hover {
        self.hover
    }
}
