//! The screen contract (spec §6.1), the container query protocol focus and hit resolution run
//! over (§7.1), and the `Composed`/`Part` pair that lets a screen be ASSEMBLED from components
//! rather than hand-drawn (§7.1).
//!
//! A composed screen gets its `Focusable` through the `composed_*` free functions and the
//! `focusable_via_composed!` macro rather than a blanket impl: coherence forbids a blanket over
//! `Composed` once the widgets implement `Focusable` themselves (`ui::geom`, phase 3a).
//! `DrawFrame::stop` folds the painter's scale/clip cascade into the registered stop and
//! `DrawFrame::clip` is the RAII scissor scope (phase 3a); the nav alphas land with the
//! containers (3b).
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

use std::borrow::Cow;
use std::ops::Deref;

use super::a11y::{A11y, A11yPath};
use super::frame::{Budget, RenderReport};
use plx_machine::machine::{
    Cx, Effects, EntryId, FocusKey, GroupId, Host, InstanceId, Leave, LogicalState, Machine,
    PartId, PressRead,
};
// The screen vocabulary the machine runtime itself names (`Host::Arg: ScreenArg`,
// `Delivery::Screen(ScreenEvent)`, and `Enter`/`FocusTarget`/`By` through the event) is defined
// in `plx_machine::machine`, below `ui` in the layer graph (docs/module-layers.md, step L4). These paths
// stay how the containers, the dispatcher and every screen name it.
pub use plx_machine::machine::{By, Enter, FocusTarget, ScreenArg, ScreenEvent};
use super::{Painter, Rect};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderStrategy {
    Page,
    VideoPlane,
}

/// **The modal dim a surface asks its HOST PAGE for** (spec §6.2, §8.3). The element a surface was
/// opened from is lifted back out of that dim by the dispatcher's one lift pass, not here
/// ([`Screen::opener`], [`Screen::bar_opener`]).
///
/// It is a REQUEST rather than a drawing, and that is the whole of why this type exists. The dim
/// sits between the page and the surface's own glass, so it is part of what that glass looks
/// through — which means it has to be on the framebuffer *before* the surface's glass grabs its
/// backdrop, i.e. inside the page pass, several call frames away from the surface that owns it.
/// The surface therefore states its weight here and the container
/// ([`ModalStack::draw_scrims`](crate::containers::modal::ModalStack::draw_scrims)) draws it,
/// in one place, for every surface, in phase order.
///
/// **The weight is a ROLE, and the ink is the page's own light.** `alpha` is one of the
/// `theme::underlay::DIM_*` rows — no surface states a number of its own — and the container paints
/// it through the stack's one `UnderlayField` (`containers::modal::ModalUnderlay`) as
/// `Role::Dim { weight: theme::underlay::TINT }`, latched from the undimmed host page (or, over the
/// video plane, [`UnderlaySource::Flat`], the neutral ink). So the dim keeps the page's colour where the
/// page has it; at `TINT == 0` it is the flat `theme::scrim_black` rect to the bit.
///
/// A surface that wants no dim overrides nothing: [`Screen::scrim`] defaults to [`Scrim::NONE`].
#[derive(Clone, Copy)]
pub struct Scrim {
    /// Peak ink alpha at full appear. The container multiplies it by the surface's own appear
    /// spring and by `nav::page_alpha`, so the dim ramps with the panel and dips with a route
    /// change exactly as `Popover::scrim` did.
    pub alpha: f32,
    /// Where the dim's inherited light comes from — see [`UnderlaySource`]. Every constructor
    /// but [`Scrim::over_video`] answers [`UnderlaySource::Page`].
    pub source: UnderlaySource,
    /// The surface's OWN 0..1 ramp for the dim, when it has one: the container uses it in place of
    /// the surface's appear spring ([`Scrim::following`]). `None` = the appear spring.
    pub ramp: Option<f32>,
}

/// **What a surface's dim inherits.** A dim here is not a black sheet: the container paints it
/// through ONE `ui::underlay::UnderlayField` per stack (`containers::modal::ModalUnderlay`), and
/// this says where that field is latched from.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum UnderlaySource {
    /// The host page itself, read off the framebuffer before any dim is on it — every surface
    /// presented over a page.
    Page,
    /// Nothing honest to inherit (a surface over the hardware video plane, which GL cannot read
    /// back, and which takes no colour from the playing item's artwork either): the flat
    /// `theme::scrim_black` ink.
    Flat,
}

impl Scrim {
    /// No dim — every page, and every surface that draws its own ground.
    pub const NONE: Scrim = Scrim {
        alpha: 0.0,
        source: UnderlaySource::Page,
        ramp: None,
    };

    /// A dim of `alpha`. `alpha` is a `theme::underlay::DIM_*`
    /// role, never a literal (`containers::tests::no_surface_states_its_own_dim_weight`).
    pub const fn dim(alpha: f32) -> Scrim {
        Scrim {
            alpha,
                source: UnderlaySource::Page,
            ramp: None,
        }
    }

    /// A dim of `alpha` over the VIDEO PLANE in the neutral flat ink — the player's panels, whose
    /// page is a hole GL cannot sample, and which stay neutral rather than casting the playing
    /// item's artwork colour over the picture.
    pub const fn over_video(alpha: f32) -> Scrim {
        Scrim {
            alpha,
                source: UnderlaySource::Flat,
            ramp: None,
        }
    }

    /// This dim with the surface's own `ramp` (0..1) in place of the container's appear spring: a
    /// morphing menu's dim follows its MORPH (`Popover::dim`), so the page dims as the glass grows
    /// and clears as the slab drains, not on a clock of its own.
    pub const fn following(self, ramp: f32) -> Scrim {
        Scrim { alpha: self.alpha, source: self.source, ramp: Some(ramp) }
    }
}

/// The screen (§6.1). `step` is the only entrance that mutates logical state after construction.
pub trait Screen<H: Host>: Machine<H, Ev = ScreenEvent<H>> + Focusable<H> {
    /// The heartbeat word — byte-identical to today's route word (§15.3).
    fn name(&self) -> &'static str;
    /// **The accessibility namespace** (`a11y::A11yPath`, the `<ns>` of an id `<ns>.<path>`): the
    /// route word as it is, `-` included. It defaults to [`Self::name`]; two screens that share a
    /// heartbeat word override it so their ids do not meet (Filmography answers `filmography`,
    /// Recommended `recommended`). A screen's `Focusable::a11y` answers a path RELATIVE to it.
    fn a11y_ns(&self) -> &'static str {
        self.name()
    }
    /// **How far the page has tucked the app's top chrome away** (the nav tab bar and the profile
    /// chip), 0 (fully shown, the default) to 1 (gone). The dispatcher hands the chrome pass
    /// [`NavPresentation::tucked`]: the nav's chrome alpha multiplied by `1 - tuck`, and the band
    /// lifted by [`chrome_lift`] (up to its own height, [`widgets::TOP_BAR_BOTTOM`]), so it fades
    /// WHILE it travels up off the top edge; the application's `draw_chrome` applies both. At tuck 1
    /// the band is gone and the dispatcher does not draw it at all (a band at `chrome_alpha` 0
    /// declares no glass and samples no ground, whatever the lift), and the bar's pointer stops are
    /// withdrawn with it; part way up they sit where the bar is DRAWN ([`NavPresentation::chrome_drawn`],
    /// [`NavPresentation::chrome_rect`]). A page
    /// answers from a `scroll_chrome::Handoff` it owns (the Library, Collections and the Watchlist:
    /// decided from the scroll target, run on the `xfade` clock); the handoff reports its own motion
    /// to the idle gate, so a running one asks for frames and a resting page keeps the frame loop
    /// asleep. A page that keeps its chrome shown (Home, the landing page's navigation) answers 0
    /// and keeps what it rides under the chrome clear of it with
    /// `scroll_chrome::clearance_under_shown_chrome` instead.
    fn chrome_tuck(&self) -> f32 {
        0.0
    }
    fn state(&self) -> &dyn LogicalState;
    fn crumb(&self, cx: &Cx<'_, H>) -> Option<Cow<'_, str>>;
    /// RENDER resources only.
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>);
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>);
    fn render(&self) -> RenderStrategy;
    /// Whether remounting an evicted child surface can read this page's identity-matched data.
    fn covered_surfaces_ready(&self) -> bool { true }
    /// May directional navigation or hover seat the container's strip (§6.2)? Home answers
    /// `false` while snapped to the grid; visible strip controls still accept direct clicks.
    fn strip_reachable(&self) -> bool {
        true
    }
    /// **Does the page scroll itself by a wheel?** A page that answers `true` moves its own
    /// scroll on `InputKind::Wheel` (fractional `dy` included, focus staying where it is), so the
    /// application's pointer EDGE GLIDE (`app::input::EdgeScroll`) is offered to it. A page that
    /// answers `false` (Home, whose scroll follows its focus) is never glided and keeps the
    /// pointer's hover at its edges.
    fn glides(&self) -> bool {
        false
    }
    /// The page's declared `Link`s (§7.3 step 3) — the strip→hero door among them.
    fn links(&self, _out: &mut Vec<Link>) {}
    /// **Does the page hold this arrow key instead of letting it move focus?** Asked only while
    /// nothing of the page holds focus: a page whose first seat is still on its way
    /// ([`PageRegions::holds`](crate::page_regions::PageRegions::holds)) answers `true` for Down,
    /// Left and Right, and the dispatcher then consumes the key BEFORE the page or the engine see
    /// it, so it seats nothing and the arrival is still the page's to make when its cells land.
    /// Defaults to `false`: the engine's no-focus fallback seats the first group it can.
    fn holds_arrival(&self, _dir: Dir) -> bool {
        false
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    /// **The pointer is held** while this screen answers `true`: the dispatcher swallows every
    /// pointer move, click and drag addressed to it BEFORE hit resolution, so none of them is ever
    /// a hit, a hover or a MISS (and so never an `OnMiss::Dismiss`), and a hit map built from
    /// content in motion is never consulted. For a surface whose rows slide or resize
    /// (`ui::panel_motion`); keys are unaffected. Defaults to `false`.
    fn pointer_held(&self) -> bool {
        false
    }
    /// **The render this screen holds of its own** — how many backing textures, and their bytes —
    /// for the frame's [`RenderSet`](crate::frame::RenderSet) check (§8.3).
    ///
    /// The default is [`RenderReport::NONE`] and it is the truthful answer for almost every screen
    /// here: they draw immediate-mode, and the textures they put on the panel come from shared
    /// pools (`ui::tex`'s posters, `ui::icons`, the glyph cache, the ONE `popover::host`
    /// `FrameCache` a covered host is served from, the blur chain) which are counted once, where
    /// they live, and never per screen that samples them. Override this only where the screen's
    /// own code called `upload_rgba` and its own code will call `delete_tex` — today that is
    /// `screens::login` (the QR bitmap) and `screens::player` (the PGS/VobSub display set). The
    /// inventory and the ceiling's derivation are in `ui/frame/render_set.rs`.
    ///
    /// Pure, like every other query on this trait: answering allocates nothing and uploads
    /// nothing.
    fn render_report(&self) -> RenderReport {
        RenderReport::NONE
    }
    /// **The modal dim this SURFACE asks its host page for** — see [`Scrim`]. A page answers
    /// [`Scrim::NONE`] and so does every surface that draws its own ground; the container asks
    /// only surfaces, and only ones whose host is cached rather than replaced.
    fn scrim(&self) -> Scrim {
        Scrim::NONE
    }
    /// **How this SURFACE arrives** ([`crate::containers::modal::EntryMotion`]): the shared appear
    /// spring by default, or the route dip (70 ms out, 140 ms in) for a surface with an opaque
    /// ground of its own that does not translate. Asked of the mounted body while the surface is
    /// opening; a page never answers.
    fn entry_motion(&self) -> crate::containers::modal::EntryMotion {
        crate::containers::modal::EntryMotion::Spring
    }
    /// An `Opaque` surface's ground has drawn at full strength: the fold may REPLACE the host
    /// from here (§6.2 `Surface::ground_ready`). The dispatcher copies it onto the surface after
    /// every draw; a page never answers.
    fn ground_ready(&self) -> bool {
        false
    }
    /// This screen's own [`ReturnState::memory`] payload, RIGHT NOW — asked by the dispatcher's
    /// `ret()` (spec §6.1 tier 2) at the moment a navigation OFF this screen is raised, exactly as
    /// `focus` is read off the engine at the same instant. **Pure, like every other query here**:
    /// answering is a snapshot, never a mutation, and a screen that has nothing worth remembering
    /// (most of them) takes the default and never overrides this. `stores/metadata.rs`'s module
    /// doc is the worked example (Detail's `Spot`) and the contract every phase-7 screen that
    /// needs to remember something builds against, rather than re-deciding where its own memory
    /// lives.
    fn memory(&self) -> H::Memory {
        H::Memory::default()
    }
    /// Capture entry memory using the engine's current focus without storing a second cursor.
    fn memory_at(&self, _focus: Option<FocusKey<H::Elem>>) -> H::Memory {
        self.memory()
    }
    /// **Repaint the focused element above a modal dim** — the item-menu opener lift, asked of
    /// whichever page hosts the menu. `focus` is the engine key the surface was opened from, passed
    /// explicitly so the page never keeps a cursor of its own. A page with no card to lift draws
    /// nothing, which is the default. The dispatcher's lift pass calls it only while `focus` is still
    /// what the page has focused (`Dispatcher::lifted_opener`), so an implementer may draw `focus` as focused
    /// without checking that the page has not moved on.
    fn redraw_focused(&self, _f: &mut DrawFrame<'_, '_, H>, _focus: Option<FocusKey<H::Elem>>) {}
    /// **The page element this surface lifts back out of the dim** (`(host page, element)`), for a
    /// surface that has one: the item menu's captured opener and every header menu's control (Library
    /// and Watchlist Sort & Filter, the Library's `+N`), which the host's `redraw_focused` paints
    /// above the scrim and the panel, at the alpha the surface's
    /// [`opener_alpha`](Self::opener_alpha) says (a morphing menu consumes it: gone while the menu
    /// stands, back as the droplet drains into it). The
    /// dispatcher folds it with the surface's phase and the engine's focus into
    /// [`Dispatcher::lifted_opener`] and keeps that element OUT of the frozen page pass
    /// (`DrawFrame::lifted`), so the lift is its only copy. `None` (the default) for every other
    /// screen.
    fn opener(&self) -> Option<(EntryId, Option<FocusKey<H::Elem>>)> {
        None
    }
    /// **The member of the shared top bar this surface hangs from** (the profile chip), for a surface
    /// whose opener is CHROME rather than a page element. It is the same opener rule as
    /// [`opener`](Self::opener) with the host resolved by the dispatcher (the page on top, whose bar
    /// it is), so `Dispatcher::lifted_opener` answers for it too and the lift pass
    /// (`Z::OPENER`) asks the rig to redraw the member lit above the dim and the panel
    /// (`Rig::lift_bar_member`). `None` (the default) for every other screen.
    fn bar_opener(&self) -> Option<H::Elem> {
        None
    }
    /// **Where the bar member this surface hangs from is DRAWN this frame** (its rest rect through
    /// the host page's tuck, the same rect the lift draws at), handed to the surface before every
    /// draw so its morph starts and ends on the live opener, not on a rect measured once at mount.
    /// Called only for a surface that answers [`bar_opener`](Self::bar_opener).
    fn opener_rect(&mut self, _drawn: crate::Rect) {}
    /// **How much of the opener this surface leaves**, 1.0 (whole, the default) .. 0.0 (consumed):
    /// the lift pass (`Z::OPENER`) paints the opener it lifts at this alpha. A MORPHING menu answers
    /// its popover's [`opener_alpha`](crate::popover::Popover::opener_alpha): the glass detaches from
    /// the opener on open (it fades out) and merges back into it on close (it fades back in), so the
    /// opener is part of the morph rather than a control the glass grows from under.
    fn opener_alpha(&self) -> f32 {
        1.0
    }
    /// **A closing MORPH surface has handed its opener back**: its droplet has drained into the
    /// opener (the frame IS the closed frame, [`Popover::handed_back`](crate::popover::Popover::handed_back)):
    /// no glass, no dim, the opener whole. Nothing of the surface is drawn from here, so the
    /// container retires it on this frame (`Surface::faded`, which `ModalStack::prune` reads) and
    /// the opener lift lets go of the opener once, on the frame nothing else of the menu draws: from
    /// the next paint the page's own pass (the chrome pass, for the profile chip) draws the opener as
    /// it rests, where the lift had it on the stack's latched field (a lighter, frozen glass) until
    /// the container's appear spring came to rest. Asked only of a `Closing` surface. `false` (the
    /// default) for every surface whose close is the container's spring: a menu that does not morph.
    fn handed_back(&self) -> bool {
        false
    }
    /// **The opener element LEFT this page and is still drawn as it goes** (a removed card's
    /// landing ghost). While a menu fades, the opener lift stands down once focus moves off the
    /// opener; a page answering `true` here keeps it up instead, because focus moved only because
    /// the card left, and the lift ([`redraw_focused`](Self::redraw_focused)) carries the ghost
    /// above the dim until the menu is gone. `false` (the default) for every other page.
    fn opener_departing(&self, _key: FocusKey<H::Elem>) -> bool {
        false
    }
    /// **A surface body answering `true` lets the page under it REFLOW while it is up**: the page
    /// is not veiled by it ([`page_veiled`](crate::containers::modal::page_veiled) stays false), so
    /// the data the surface's choices change reaches the page, and its landing plays, under the
    /// open surface. The host the glass samples then follows the page live (`popover::host::set_reflow`)
    /// and is captured again once, when the page has settled. `false` (the default) for every
    /// other surface: a menu holds the page still under it.
    fn lets_host_reflow(&self) -> bool {
        false
    }
    /// A page: its cards are moving this frame (a landing is playing), so the host a surface
    /// over it samples is stale until it stops. Read only for a surface that
    /// [`lets_host_reflow`](Self::lets_host_reflow).
    fn reflowing(&self) -> bool {
        false
    }
    /// **What a HELD surface still shows.** A surface is held (`PopoverMotion::held`) on its
    /// capture frame and while its text warms: at appear 0, its `draw` walked through the text
    /// recorder, which paints nothing. For a surface that redraws its own [`opener`](Self::opener)
    /// that is wrong, because the page leaves the opener out from the surface's FIRST frame: the
    /// element would vanish for the length of the hold. Such a surface draws that element here,
    /// as it stands at the start of its motion, through a live painter. The text it draws is the
    /// page's own and was resident a frame ago. The default draws nothing.
    fn draw_held(&mut self, _f: &mut DrawFrame<'_, '_, H>) {}
    /// **A VISIBLE surface is holding content back until its text is resident.** A Settings push
    /// commits at once and the page it brings in would be rasterised, cold, in the push's first
    /// drawn frame; the surface waits at the push's start instead ([`crate::route_screen::RoutePush::begin_warm`])
    /// and answers `true` while it does. The dispatcher's surface pass then walks
    /// [`draw_warm`](Self::draw_warm) through the text recorder after the surface's own `draw` and
    /// drains the queue under the frame's shared prewarm budget. The default is `false`.
    fn warms_text(&self) -> bool {
        false
    }
    /// **Walk the content that is about to be shown through the text recorder** (the frame's painter
    /// records; nothing is drawn). Called only while [`warms_text`](Self::warms_text) is true, and
    /// not in a blur source pass. The default walks nothing.
    fn draw_warm(&mut self, _f: &mut DrawFrame<'_, '_, H>) {}
    /// **The card the page has focused, and where it sits** — the host's question for an item menu
    /// and for the focus probe, asked of whichever card page is on screen so the host names no
    /// screen type. `focus` is the engine key to answer for (a page answers only its own entry's
    /// cards); `None` for a page with no card, which is the default. The rect is measured only
    /// when `at` names how (`None` for a caller that wants the item alone).
    fn focused_card<'a>(&self, _cx: &Cx<'a, H>, _focus: Option<FocusKey<H::Elem>>, _at: Option<At>) -> Option<FocusedCard<'a>> {
        None
    }
    /// **Where the focused card sits, without needing its item to resolve** — the live drawn rect
    /// of the card `focus` names. An opener that only anchors a menu (the item travels on the
    /// request, it is not re-read) asks this instead of [`Self::focused_card`], so a page whose
    /// data moved on between the hold and the request still anchors the menu where the card is.
    /// `None` for a page with no card, which is the default.
    fn focused_card_rect(&self, _cx: &Cx<'_, H>, _focus: Option<FocusKey<H::Elem>>, _at: At) -> Option<Rect> {
        None
    }
    /// Typed application inspection during migration; the library never names a screen type.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// Return state (§6.1 tier 2): on the `Entry`, captured at request time — what an evicted entry
/// remounts from.
///
/// `M` is the opaque per-screen payload (`Host::Memory`), defaulted to `()` so every existing
/// spelling — `ReturnState<H::Elem>`, `ReturnState::default()` — keeps meaning exactly what it did
/// before this field existed: a Host with nothing to remember (`InnerHost`/`FixtureHost`,
/// and any container that only ever names `ReturnState<K>` with one type
/// argument) pays nothing and changes nothing. A screen that DOES need to remember something asks
/// for it through [`Screen::memory_at`] and fixes its Host's `Memory` associated type to a concrete
/// payload — see that method's doc and `stores/metadata.rs`'s module doc (`Spot`, the worked case).
///
/// **Not `Copy`.** It was, while its only fields were a key and a float; `M` is application data
/// (a `Spot` is not `Copy`) and every real use here is a single owned value passed once — `Clone`
/// costs nothing to keep for a caller that genuinely needs a second copy, and dropping the derive
/// only removes a bound this type never relied on.
#[derive(Clone, Debug)]
pub struct ReturnState<K, M = ()> {
    pub focus: Option<FocusKey<K>>,
    /// Engine-owned cursors for every group in this entry, including inactive groups.
    pub remembered: Vec<(GroupId, K)>,
    pub scroll: f32,
    /// The screen's own [`Screen::memory`] snapshot, taken the moment a navigation off it was
    /// raised — tier 2's answer to "where was this page standing" for whatever a screen's `focus`
    /// and `scroll` alone cannot say (spec §6.1; e.g. Detail's season/column/sub-row `Spot`).
    pub memory: M,
}

pub const RETURN_STATE_SHAPE: &str = "ReturnState{focus:Option<(EntryId,H::Elem)>,remembered:[(GroupId,H::Elem)],scroll:f32,memory:H::Memory}";

impl<K, M: Default> Default for ReturnState<K, M> {
    fn default() -> Self {
        Self {
            focus: None,
            remembered: Vec::new(),
            scroll: 0.0,
            memory: M::default(),
        }
    }
}

/// The ONE `mount` match (`screens/registry.rs`). Receives the id first so a request it emits is
/// addressable.
pub trait Mounter<H: Host> {
    fn mount(
        &mut self,
        id: InstanceId,
        arg: &H::Arg,
        ret: &ReturnState<H::Elem, H::Memory>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Box<dyn Screen<H>>;
}

// ---------------------------------------------------------------------------------------------
// §7.1 — the container query protocol
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    /// The direction an arrow key names; `None` for every other key.
    pub fn of_key(key: plx_machine::machine::Key) -> Option<Dir> {
        use plx_machine::machine::Key;
        match key {
            Key::Up => Some(Dir::Up),
            Key::Down => Some(Dir::Down),
            Key::Left => Some(Dir::Left),
            Key::Right => Some(Dir::Right),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum GroupKind {
    Row { wrap: bool },
    Column,
    Grid { cols: usize, holes: &'static [(usize, usize)] },
    Free,
    Document,
}

/// The focus-ENTRY policy (§7.3 step 4), named cases, never one heuristic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seat {
    Nearest,
    /// The group's remembered element — except an UP/DOWN press into a card lattice
    /// (`ElemKind::Card` `Row` or `Grid`), which lands through `seat` on the card above or below
    /// the cursor, so "down, right, up" closes a square (`focus::projects_across`). Sideways
    /// doors, entries and restores, selector rows and columns keep the remembered element.
    Remembered,
    /// Reconcile the group's remembered element even across a vertical card-lattice door. If
    /// it cannot be resolved within the group, seat at the group's head. Linked shelf headings use
    /// this for their DOWN transition. Ordinary row-to-row movement keeps [`Seat::Remembered`]'s
    /// square-closing projection.
    RememberedFirst,
    /// The group's remembered element when it is within `rows` element-sizes of the key the move
    /// leaves, else the one projected under that key; `u8::MAX` is "remembered, else projected"
    /// ([`crate::page_regions::CONTENT_HEAD_SEAT`]).
    RememberedNear { rows: u8 },
    First,
    Projected,
    /// Project through `seat` from another group's engine-owned remembered element, even if
    /// focus most recently came from a toolbar. Its current reconciled placement (including
    /// index) is supplied as `from`; an unavailable source falls back to the incoming placement.
    /// This reads the engine's memory without publishing a second cursor in a screen or view.
    ProjectedFrom(GroupId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeRule {
    Geometric,
    Stop,
    Screen,
    Nav(plx_machine::machine::NavOpKind),
}

/// Bit 0 = horizontal, bit 1 = vertical: the axes along which a geometric search may LAND here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AxisMask(pub u8);

impl AxisMask {
    pub const BOTH: AxisMask = AxisMask(0b11);
    pub const HORIZONTAL: AxisMask = AxisMask(0b01);
    pub const VERTICAL: AxisMask = AxisMask(0b10);
}

/// What a group's elements ARE for the press (§7.4): a `Card` arms a holdable press, a `Control`
/// a non-holdable one, `Bare` delivers `Activate` on the DOWN edge and never arms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElemKind {
    Card,
    Control,
    Bare,
}

#[derive(Clone, Copy, Debug)]
pub struct GroupSpec {
    pub id: GroupId,
    pub kind: GroupKind,
    pub seat: Seat,
    pub reachable: AxisMask,
    /// `[up, down, left, right]`.
    pub edge: [EdgeRule; 4],
    pub extent: Rect,
    pub len: usize,
    pub elem: ElemKind,
}

/// A declared non-standard transition (§7): at `from`'s `dir` edge, focus goes to `to` — and
/// wins over the group's `EdgeRule` for that direction only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Link {
    pub from: GroupId,
    pub dir: Dir,
    pub to: GroupId,
}

/// Who answers a direction for this screen (§7.6): the engine, or the legacy ladders — for a
/// screen that answers `Legacy` the engine and the hit map are INERT. The player is the last
/// one that does (phase 12); the blank route-word page that used to be the other went in 10.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusSource {
    Engine,
    Legacy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitSource {
    Engine,
    Legacy,
}

#[derive(Clone, Copy, Debug)]
pub enum Step<K> {
    Move(FocusKey<K>),
    Edge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum At {
    SpringTarget,
    Drawn,
}

/// The card a page has focused ([`Screen::focused_card`]): the catalog item behind it, as the
/// application's own type (this layer names none, so the host downcasts it), and its rect.
#[derive(Clone, Copy)]
pub struct FocusedCard<'a> {
    pub item: &'a dyn std::any::Any,
    /// The live drawn rect ([`Placed::rect`]).
    pub rect: Option<Rect>,
    /// The settled focus-scaled rect ([`Placed::rest_rect`]) — what an opener anchors to.
    pub rest_rect: Option<Rect>,
}

impl<'a> FocusedCard<'a> {
    /// `item` with the placement the page measured for it (`None` when the caller asked for none).
    pub fn new(item: &'a dyn std::any::Any, placed: Option<Placed>) -> Self {
        Self { item, rect: placed.map(|p| p.rect), rest_rect: placed.map(|p| p.rest_rect) }
    }
}

/// What was painted / where it rests / what clipped it (§7.6).
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub rect: Rect,
    pub rest_rect: Rect,
    pub clip: Rect,
    /// The element's INDEX in its group when it is one — the `from` tie-break of
    /// `card_row::column_near_x` (an exact tie keeps the index you had; any direction-only rule
    /// ratchets), carried on the placement so `seat` can read it without a second argument.
    pub index: Option<u32>,
}

/// EVERY method takes `&self`: the engine never mutates a screen (§7.3 step 5 says who does).
pub trait Focusable<H: Host> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>);
    /// Which group an element belongs to — what the engine consults an `EdgeRule` on.
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId>;
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem>;
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed>;
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem>;
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem>;

    /// **The accessibility projection of `key`** (`a11y`, the a11y-ids plan D2): its id relative to
    /// this view, its role, and what a test reads about it. `None` for a key this view does not own.
    /// Unconditional and defaulted, so nothing changes until a widget answers; `a11y::conformance`
    /// is the suite an implementation runs. The borrow is of `self`: a view built per query returns
    /// `A11y::into_owned`.
    fn a11y(&self, _key: &H::Elem, _cx: &Cx<'_, H>) -> Option<A11y<'_>> {
        None
    }
    /// Every element of group `g`, in index order, pushed onto `out` (`GroupSpec::len` only counts
    /// them, and no other method maps a group and an index to a key). A view pushes NOTHING for a
    /// group it does not own, and lists the off-screen elements too.
    fn a11y_elems(&self, _g: GroupId, _cx: &Cx<'_, H>, _out: &mut Vec<H::Elem>) {}
    /// The inverse of [`Self::a11y`]: the element whose projected path is `path`, off-screen
    /// elements included. The default walks every group (`a11y::default_walk`); an indexed widget
    /// overrides it to read the index out of the path and answer `elem(i)` directly, bounds-checked.
    fn a11y_find(&self, path: &A11yPath, cx: &Cx<'_, H>) -> Option<H::Elem> {
        crate::a11y::default_walk(self, path, cx)
    }
}

/// A component that can be focused AND drawn — what `&dyn Focusable` cannot (§7.1).
pub trait Part<H: Host>: Focusable<H> {
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>);
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, rect: Rect);
}

/// A screen assembled from parts. `part_mut` is the only `&mut` access and it is to a RENDER
/// half; logical state stays behind `step`.
pub trait Composed<H: Host> {
    fn layout(&self, cx: &Cx<'_, H>) -> Vec<(PartId, Rect)>;
    fn part(&self, id: PartId) -> &dyn Part<H>;
    fn part_mut(&mut self, id: PartId) -> &mut dyn Part<H>;
}

/// A `Composed` screen's focus protocol is its parts' concatenated in `layout` order. Not a
/// blanket `impl<T: Composed> Focusable for T` — coherence forbids any other `Focusable` impl
/// beside one (the widgets' own, `ui::geom`) — so a composed screen writes
/// `focusable_via_composed!(Type)`, which delegates to these five.
pub fn composed_groups<H: Host, T: Composed<H> + ?Sized>(s: &T, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
    for (id, _) in s.layout(cx) {
        s.part(id).groups(cx, out);
    }
}

pub fn composed_group_of<H: Host, T: Composed<H> + ?Sized>(s: &T, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
    s.layout(cx)
        .into_iter()
        .find_map(|(id, _)| s.part(id).group_of(key, cx))
}

pub fn composed_neighbour<H: Host, T: Composed<H> + ?Sized>(
    s: &T,
    key: FocusKey<H::Elem>,
    dir: Dir,
    cx: &Cx<'_, H>,
) -> Step<H::Elem> {
    for (id, _) in s.layout(cx) {
        let part = s.part(id);
        if part.place(&key.elem, cx, At::SpringTarget).is_some() {
            return part.neighbour(key, dir, cx);
        }
    }
    Step::Edge
}

pub fn composed_place<H: Host, T: Composed<H> + ?Sized>(s: &T, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
    s.layout(cx)
        .into_iter()
        .find_map(|(id, _)| s.part(id).place(key, cx, at))
}

pub fn composed_reconcile<H: Host, T: Composed<H> + ?Sized>(
    s: &T,
    want: FocusKey<H::Elem>,
    cx: &Cx<'_, H>,
) -> FocusKey<H::Elem> {
    // The owning part gets first repair, even when the old key still places: it may need
    // slot-to-item promotion or separator repair. An unrelated table must not clamp a valid
    // footer key into its own last row before the footer sees it.
    for (id, _) in s.layout(cx) {
        let part = s.part(id);
        if part.group_of(&want.elem, cx).is_some() {
            let r = part.reconcile(want, cx);
            if part.place(&r.elem, cx, At::SpringTarget).is_some() {
                return r;
            }
            break;
        }
    }
    // Unknown/removed owner or an unplaceable repair: retain the ordered fallback. A
    // disappearing footer must recover into the table rather than preserve an invalid key.
    for (id, _) in s.layout(cx) {
        let part = s.part(id);
        let r = part.reconcile(want, cx);
        if part.place(&r.elem, cx, At::SpringTarget).is_some() {
            return r;
        }
    }
    want
}

pub fn composed_seat<H: Host, T: Composed<H> + ?Sized>(s: &T, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
    let mut specs = Vec::new();
    for (id, _) in s.layout(cx) {
        specs.clear();
        let part = s.part(id);
        part.groups(cx, &mut specs);
        if specs.iter().any(|sp| sp.id == g) {
            return part.seat(g, from, cx);
        }
    }
    // No part owns the group: the engine's last resort is the first part's first seat.
    let first = s
        .layout(cx)
        .first()
        .map(|(id, _)| *id)
        .expect("a Composed screen has at least one part");
    s.part(first).seat(g, from, cx)
}

/// The first part that projects `key` answers, as `composed_place` does for a placement. The parts
/// answer relative paths that do not meet (a collection's name is its own part's), so a container
/// that needs a prefix adds it in its own `a11y`.
pub fn composed_a11y<'a, H: Host, T: Composed<H> + ?Sized>(
    s: &'a T,
    key: &H::Elem,
    cx: &Cx<'_, H>,
) -> Option<A11y<'a>> {
    s.layout(cx).into_iter().find_map(|(id, _)| s.part(id).a11y(key, cx))
}

/// Group `g`'s elements, from the part that declares `g` (as `composed_seat` finds its owner), so a
/// part that lists elements for a group it does not own cannot double them.
pub fn composed_a11y_elems<H: Host, T: Composed<H> + ?Sized>(
    s: &T,
    g: GroupId,
    cx: &Cx<'_, H>,
    out: &mut Vec<H::Elem>,
) {
    let mut specs = Vec::new();
    for (id, _) in s.layout(cx) {
        specs.clear();
        let part = s.part(id);
        part.groups(cx, &mut specs);
        if specs.iter().any(|sp| sp.id == g) {
            part.a11y_elems(g, cx, out);
            return;
        }
    }
}

/// The first part that finds `path` answers.
pub fn composed_a11y_find<H: Host, T: Composed<H> + ?Sized>(
    s: &T,
    path: &A11yPath,
    cx: &Cx<'_, H>,
) -> Option<H::Elem> {
    s.layout(cx)
        .into_iter()
        .find_map(|(id, _)| s.part(id).a11y_find(path, cx))
}

/// `impl Focusable<H> for $t` by delegation to its `Composed` parts (see `composed_groups`).
#[macro_export]
macro_rules! focusable_via_composed {
    ($t:ty, $h:ty) => {
        impl $crate::screen::Focusable<$h> for $t {
            fn groups(&self, cx: &plx_machine::machine::Cx<'_, $h>, out: &mut Vec<$crate::screen::GroupSpec>) {
                $crate::screen::composed_groups(self, cx, out)
            }
            fn group_of(
                &self,
                key: &<$h as plx_machine::machine::Host>::Elem,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> Option<plx_machine::machine::GroupId> {
                $crate::screen::composed_group_of(self, key, cx)
            }
            fn neighbour(
                &self,
                key: plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem>,
                dir: $crate::screen::Dir,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> $crate::screen::Step<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::composed_neighbour(self, key, dir, cx)
            }
            fn place(
                &self,
                key: &<$h as plx_machine::machine::Host>::Elem,
                cx: &plx_machine::machine::Cx<'_, $h>,
                at: $crate::screen::At,
            ) -> Option<$crate::screen::Placed> {
                $crate::screen::composed_place(self, key, cx, at)
            }
            fn reconcile(
                &self,
                want: plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem>,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::composed_reconcile(self, want, cx)
            }
            fn seat(
                &self,
                g: plx_machine::machine::GroupId,
                from: $crate::screen::Placed,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::composed_seat(self, g, from, cx)
            }
            fn a11y(
                &self,
                key: &<$h as plx_machine::machine::Host>::Elem,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> Option<$crate::a11y::A11y<'_>> {
                $crate::screen::composed_a11y(self, key, cx)
            }
            fn a11y_elems(
                &self,
                g: plx_machine::machine::GroupId,
                cx: &plx_machine::machine::Cx<'_, $h>,
                out: &mut Vec<<$h as plx_machine::machine::Host>::Elem>,
            ) {
                $crate::screen::composed_a11y_elems(self, g, cx, out)
            }
            fn a11y_find(
                &self,
                path: &$crate::a11y::A11yPath,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> Option<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::composed_a11y_find(self, path, cx)
            }
        }
    };
}

/// `impl Focusable<H> for $t` by delegation to a VIEW the screen builds for the frame — a
/// `TableScreen`/`DocumentScreen` over its own state (`fn $view(&self) -> impl Focusable<$h>`).
/// The screen keeps the widgets; the composition is rebuilt per query, exactly as the draw
/// rebuilds it (phase 5b's family pages).
///
/// The second form is for a screen generic over its host (a page that reads its store through a
/// host trait): `focusable_via_view!(Screen, H: [Bounds], view)` writes
/// `impl<H: Bounds> Focusable<H> for Screen` and calls `self.view::<H>()`, so the view method is
/// generic too (`fn view<H: Bounds>(&self) -> impl Focusable<H> + '_`) — a `cards::Stack` page.
#[macro_export]
macro_rules! focusable_via_view {
    (@impl [$($gen:tt)*] $t:ty, $h:ty, $view:ident, [$($turbofish:tt)*] $(, $press:ident)?) => {
        impl $($gen)* $crate::screen::Focusable<$h> for $t {
            fn groups(&self, cx: &plx_machine::machine::Cx<'_, $h>, out: &mut Vec<$crate::screen::GroupSpec>) {
                $crate::screen::Focusable::<$h>::groups(&self.$view $($turbofish)* ($(cx.$press)?), cx, out)
            }
            fn group_of(
                &self,
                key: &<$h as plx_machine::machine::Host>::Elem,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> Option<plx_machine::machine::GroupId> {
                $crate::screen::Focusable::<$h>::group_of(&self.$view $($turbofish)* ($(cx.$press)?), key, cx)
            }
            fn neighbour(
                &self,
                key: plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem>,
                dir: $crate::screen::Dir,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> $crate::screen::Step<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::Focusable::<$h>::neighbour(&self.$view $($turbofish)* ($(cx.$press)?), key, dir, cx)
            }
            fn place(
                &self,
                key: &<$h as plx_machine::machine::Host>::Elem,
                cx: &plx_machine::machine::Cx<'_, $h>,
                at: $crate::screen::At,
            ) -> Option<$crate::screen::Placed> {
                $crate::screen::Focusable::<$h>::place(&self.$view $($turbofish)* ($(cx.$press)?), key, cx, at)
            }
            fn reconcile(
                &self,
                want: plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem>,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::Focusable::<$h>::reconcile(&self.$view $($turbofish)* ($(cx.$press)?), want, cx)
            }
            fn seat(
                &self,
                g: plx_machine::machine::GroupId,
                from: $crate::screen::Placed,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> plx_machine::machine::FocusKey<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::Focusable::<$h>::seat(&self.$view $($turbofish)* ($(cx.$press)?), g, from, cx)
            }
            // The view is a temporary built for this query, so the projection it answers cannot
            // borrow from it: it is handed back owning its text.
            fn a11y(
                &self,
                key: &<$h as plx_machine::machine::Host>::Elem,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> Option<$crate::a11y::A11y<'_>> {
                $crate::screen::Focusable::<$h>::a11y(&self.$view $($turbofish)* ($(cx.$press)?), key, cx)
                    .map($crate::a11y::A11y::into_owned)
            }
            fn a11y_elems(
                &self,
                g: plx_machine::machine::GroupId,
                cx: &plx_machine::machine::Cx<'_, $h>,
                out: &mut Vec<<$h as plx_machine::machine::Host>::Elem>,
            ) {
                $crate::screen::Focusable::<$h>::a11y_elems(&self.$view $($turbofish)* ($(cx.$press)?), g, cx, out)
            }
            fn a11y_find(
                &self,
                path: &$crate::a11y::A11yPath,
                cx: &plx_machine::machine::Cx<'_, $h>,
            ) -> Option<<$h as plx_machine::machine::Host>::Elem> {
                $crate::screen::Focusable::<$h>::a11y_find(&self.$view $($turbofish)* ($(cx.$press)?), path, cx)
            }
        }
    };
    ($t:ty, $h:ident: [$($bound:tt)+], $view:ident) => {
        $crate::focusable_via_view!(@impl [<$h: $($bound)+>] $t, $h, $view, [::<$h>]);
    };
    ($t:ty, $h:ty, $view:ident) => {
        $crate::focusable_via_view!(@impl [] $t, $h, $view, []);
    };
    ($t:ty, $h:ty, $view:ident, press) => {
        $crate::focusable_via_view!(@impl [] $t, $h, $view, [], press);
    };
}

/// The draw skeleton for a `Composed` screen: iterate `layout`, prepare / draw each part.
pub fn composed_prepare<H: Host, T: Composed<H>>(s: &mut T, b: &mut Budget, cx: &Cx<'_, H>) {
    for (id, _) in s.layout(cx) {
        s.part_mut(id).prepare(b, cx);
    }
}

pub fn composed_draw<H: Host, T: Composed<H>>(s: &mut T, f: &mut DrawFrame<'_, '_, H>) {
    let cx: &Cx<'_, H> = f;
    let layout = s.layout(cx);
    for (id, rect) in layout {
        s.part_mut(id).draw(f, rect);
    }
}

/// A stop the draw registers (§7.5, §7.6).
#[derive(Clone, Copy, Debug)]
pub struct Stop<K> {
    pub key: FocusKey<K>,
    pub rect: Rect,
    pub rest_rect: Rect,
    pub clip: Rect,
    pub hover: Hover,
    pub activate: Activate,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hover {
    Focus,
    Ignore,
    OnlyIfFocused,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activate {
    Press,
    Immediate,
    Direct,
}

/// One draw-pass snapshot of navigation presentation. Screens do not read a live nav singleton.
/// `view_tab` overrides the mounted page's selected tab only while a destination is queued.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavPresentation {
    pub page_alpha: f32,
    /// The top chrome's opacity. At 0 the chrome is not drawn ([`Self::chrome_drawn`]) and has no
    /// pointer stops.
    pub chrome_alpha: f32,
    pub view_tab: Option<u32>,
    pub blur_amount: f32,
    /// How far the top chrome sits above its rest position, in px (0 at rest; a tucked page lifts
    /// it by up to [`widgets::TOP_BAR_BOTTOM`] while it fades). The chrome pass translates by it.
    pub chrome_lift: f32,
}

impl Default for NavPresentation {
    fn default() -> Self {
        Self { page_alpha: 1.0, chrome_alpha: 1.0, view_tab: None, blur_amount: 0.0, chrome_lift: 0.0 }
    }
}

/// The distance the top chrome rises at `tuck` (0 shown .. 1 gone): none at rest, the whole chrome
/// band ([`widgets::TOP_BAR_BOTTOM`]) once fully tucked.
pub fn chrome_lift(tuck: f32) -> f32 {
    crate::widgets::TOP_BAR_BOTTOM * tuck.clamp(0.0, 1.0)
}

impl NavPresentation {
    /// Build a snapshot from the owning stack and the application's tab and blur configuration.
    pub fn from_transition(page: f32, chrome: f32, view_tab: Option<u32>, blur_dissolve: bool) -> Self {
        Self {
            page_alpha: if blur_dissolve { 1.0 } else { page },
            chrome_alpha: chrome,
            view_tab,
            blur_amount: if blur_dissolve { 1.0 - page } else { 0.0 },
            chrome_lift: 0.0,
        }
    }

    /// This presentation with the top chrome tucked away by `tuck` ([`Screen::chrome_tuck`]):
    /// the chrome's alpha falls to 0 and it lifts by [`chrome_lift`] as the tuck reaches 1. The dispatcher hands the chrome pass
    /// this value for the page that answers a tuck; every other page answers 0 and is unchanged.
    pub fn tucked(self, tuck: f32) -> Self {
        Self { chrome_alpha: self.chrome_alpha * (1.0 - tuck.clamp(0.0, 1.0)), chrome_lift: self.chrome_lift + chrome_lift(tuck), ..self }
    }

    /// **Is the shared top bar drawn under this presentation?** The ONE answer, asked of a presentation
    /// that already carries the page's tuck ([`Self::tucked`]): the chrome pass is skipped when it is
    /// `false` (a band at alpha 0 declares no glass and samples no ground, whatever the lift) AND the
    /// bar's pointer stops are withdrawn when it is `false`, because a control that is not on screen
    /// must not be pressable or hoverable. Both ask this and nothing else; the dispatcher's
    /// `chrome_presented` is the pairing of the two.
    pub fn chrome_drawn(&self) -> bool {
        self.chrome_alpha > 0.0
    }

    /// Where the bar draws `rest`, a rect of its RESTING geometry: lifted by [`Self::chrome_lift`], as the
    /// application's `draw_chrome` translates its painter. The bar's pointer stops go through this, so a
    /// click lands where the bar is drawn and not where it would rest.
    pub fn chrome_rect(&self, rest: Rect) -> Rect {
        Rect { y: rest.y - self.chrome_lift, ..rest }
    }
}

/// What `draw` receives (§6.1): the read context, painter, navigation presentation and typed stop
/// sink. `stop` folds the painter's cascade into screen space; `clip` is the RAII scissor scope.
/// Navigation values are captured once by the application/dispatcher, not read live by screens.
pub struct DrawFrame<'a, 'views, H: Host> {
    pub cx: &'a Cx<'views, H>,
    pub painter: Painter,
    pub page_alpha: f32,
    pub chrome_alpha: f32,
    pub view_tab: Option<u32>,
    pub blur_amount: f32,
    /// The ROUTE-level nav dip's page alpha this frame was built with (spec §14 phase 8) — what
    /// `ui::nav::page_alpha()` answered at the one per-frame read the application made, carried
    /// alongside `page_alpha` rather than folded only into it. A container (a `RouteSurface`'s
    /// own appear motion, a `NavStack`'s push/pop transition) is free to overwrite `page_alpha`
    /// with its own LOCAL alpha for the page/surface it draws — surfaces do, at
    /// `dispatch.rs`'s `f.page_alpha = s.motion.appear` — and a screen that also needs the outer
    /// route fade (Settings' scrim wants both: its own appear AND the page fading beneath it;
    /// Detail's hero-button ambient sample must not run while a route change is still in flight)
    /// reads this field instead, so it survives that overwrite. Never mutated after
    /// construction — the one write is `with_navigation`.
    pub nav_page_alpha: f32,
    pub press: PressRead<H::Elem>,
    /// **The page under this surface, as the container latched it** — the `ModalStack`'s one
    /// underlay field (`containers::modal::ModalUnderlay`), which a popover panel's ground is drawn
    /// from (`widgets::panel_ground`). `None` on a page's own frame and on any frame whose
    /// container owns no field; a panel reads `None`, or a field not latched yet, as "draw the flat
    /// sheet". Set by the dispatcher's surface pass, never by a screen.
    pub underlay: Option<&'a crate::underlay::UnderlayField>,
    /// **The page element a surface lifts back out of the dim this frame** (item menu opener,
    /// [`Screen::opener`]). A page leaves that element OUT of the page pass ([`Self::leaves_out`]: a card section, the Detail episode strip)
    /// (`cards::Shelf` / `Grid::draw_card`): the lift ([`Screen::redraw_focused`], drawn live above
    /// the host snapshot at the press's own scale) is its only copy, so the frozen page never holds
    /// a stale (dipped) copy under the growing one and the release spring of a taken hold is not
    /// page damage. `None` on every frame the lift does not run, and on the lift's own frame.
    pub lifted: Option<FocusKey<H::Elem>>,
    /// Page instance addressed by this walk; assigned only by its dispatcher.
    pub lift_owner: Option<InstanceId>,
    lift_receipt: std::cell::Cell<Option<crate::input::LiftReceipt<H::Elem>>>,
    // Render evidence only; the dispatcher owns final-swap admission and logical delivery.
    presentation: std::cell::RefCell<Option<H::Msg>>,
    /// **The lift's own frame for an opener that left the page** ([`Screen::opener_departing`]):
    /// the element whose landing ghost the lift carries above the dim. A grid's
    /// `paint_ghosts` paints that ghost alone on such a frame (and the page pass, whose `lifted`
    /// names it, leaves it out). `None` on every other frame.
    pub lifting: Option<FocusKey<H::Elem>>,
    /// **The push this frame is drawn under, for a page that holds its narrative column**
    /// ([`crate::route_screen::PushFrame`]). `None` everywhere else: a page the surface draws whole
    /// carries its push in `painter` already, and a page drawn at rest has none. Set by the
    /// Settings surface, read through [`Self::narrative_painter`] / [`Self::list_painter`] by the
    /// parts that draw a route's two columns (`table_screen`).
    pub push: Option<crate::route_screen::PushFrame>,
    /// **How far a surface's entry dip has taken the page beneath it to the app ground**, `0..=1`
    /// (`containers::modal::PopoverMotion::floor`). `0` on every frame but the opening of a surface
    /// that opts into the dip ([`Screen::entry_motion`]). Set by the dispatcher's surface pass.
    pub entry_floor: f32,
    stops: Vec<Stop<H::Elem>>,
    wants_frame: bool,
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static STOPS_IN_RECORDING_WALK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a recording walk registers stops (only ever true inside [`record_stops_while_recording`]).
#[cfg(any(test, feature = "test-support"))]
fn stops_in_recording_walk() -> bool {
    STOPS_IN_RECORDING_WALK.with(|v| v.get())
}
#[cfg(not(any(test, feature = "test-support")))]
const fn stops_in_recording_walk() -> bool {
    false
}

/// **Run `f` so a recording walk ALSO registers its stops** (test only). A host test has no GL
/// context, so it can only run a screen's real `draw` through a recording painter, which places no
/// stop (`DrawFrame::records_stops`); inside this scope the stops are placed exactly as the visible
/// walk would place them, and nothing is painted. Per thread, restored on unwind.
#[cfg(any(test, feature = "test-support"))]
pub fn record_stops_while_recording<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            STOPS_IN_RECORDING_WALK.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(STOPS_IN_RECORDING_WALK.with(|v| v.replace(true)));
    f()
}

impl<'a, 'views, H: Host> DrawFrame<'a, 'views, H> {
    /// Route presentation retained independently of a container's local page alpha.
    pub fn route_navigation(&self) -> NavPresentation {
        NavPresentation {
            page_alpha: self.nav_page_alpha,
            chrome_alpha: self.chrome_alpha,
            view_tab: self.view_tab,
            blur_amount: self.blur_amount,
            chrome_lift: 0.0,
        }
    }

    pub fn new(cx: &'a Cx<'views, H>, painter: Painter) -> Self {
        Self::with_navigation(cx, painter, NavPresentation::default())
    }

    pub fn with_navigation(cx: &'a Cx<'views, H>, painter: Painter, nav: NavPresentation) -> Self {
        Self {
            cx,
            painter,
            page_alpha: nav.page_alpha,
            chrome_alpha: nav.chrome_alpha,
            view_tab: nav.view_tab,
            blur_amount: nav.blur_amount,
            nav_page_alpha: nav.page_alpha,
            press: cx.press,
            underlay: None,
            lifted: None,
            lift_owner: None,
            lift_receipt: std::cell::Cell::new(None),
            presentation: std::cell::RefCell::new(None),
            lifting: None,
            push: None,
            entry_floor: 0.0,
            stops: Vec::new(),
            wants_frame: false,
        }
    }

    /// The painter for a route's NARRATIVE column: the frame's own at rest, and under a push that
    /// holds the column ([`Self::push`]) the same position with the push's cross-fade alpha.
    pub fn narrative_painter(&self) -> Painter {
        self.push.map_or(self.painter, |push| push.narrative(self.painter))
    }

    /// The painter for a route's CONTENT column (its table): the frame's own at rest, and under a
    /// push that holds the narrative column ([`Self::push`]) the list's lead-in or exit.
    pub fn list_painter(&self) -> Painter {
        self.push.map_or(self.painter, |push| push.list(self.painter))
    }

    /// **Does the page pass leave `elem` of `entry` out, because a surface lifts it?** The ONE
    /// question every page asks of [`Self::lifted`] before it draws an element a surface can open
    /// from (a card section's tile, a Detail episode's cell). `true` means the opener lift
    /// ([`Screen::redraw_focused`]) draws the only copy above the host snapshot, so the page draws
    /// NOTHING of it - not its art and not its label plate - and the snapshot never holds a stale
    /// (dipped) copy under the lift's. It also says so to the press machine
    /// through a local receipt, whose spring then is no page damage. The lift's own
    /// frame carries no `lifted`, so it draws.
    pub fn leaves_out(&self, entry: EntryId, elem: H::Elem) -> bool {
        let out = self.lifted.is_some_and(|k| k.entry == entry && k.elem == elem);
        if out {
            if let Some(owner) = self.lift_owner {
                self.lift_receipt.set(Some(crate::input::LiftReceipt {
                    press: plx_machine::machine::PressId(self.press.identity),
                    key: FocusKey { entry, elem }, owner,
                }));
            }
        }
        out
    }

    /// Local draw evidence. Only Dispatcher may admit it as logical input policy.
    pub fn lift_receipt(&self) -> Option<crate::input::LiftReceipt<H::Elem>> {
        self.lift_receipt.get()
    }

    /// Speculative, blur, discovery and text-recording walks cannot publish logical policy.
    pub fn visible_lift_receipt(&self) -> Option<crate::input::LiftReceipt<H::Elem>> {
        if self.painter.is_recording() || crate::frame::backdrop::source_walk()
            || plx_gfx::gfx::blur_source_pass() {
            None
        } else {
            self.lift_receipt()
        }
    }

    /// Report one typed paint observation, never a logical mutation. Offscreen walks cannot
    /// manufacture evidence, even when their geometry describes an otherwise visible control.
    pub fn report_presentation(&self, message: H::Msg) {
        if !self.painter.is_recording() && !crate::rec::speculating()
            && !crate::frame::backdrop::source_walk() && !plx_gfx::gfx::blur_source_pass() {
            *self.presentation.borrow_mut() = Some(message);
        }
    }

    pub(crate) fn take_presentation(&self) -> Option<H::Msg> {
        self.presentation.borrow_mut().take()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn take_presentation_for_test(&self) -> Option<H::Msg> {
        self.take_presentation()
    }

    /// Carry this frame's presentation into a nested frame, including any container alpha.
    ///
    /// Deliberately carries the (possibly container-overwritten) `page_alpha`, not
    /// `nav_page_alpha` — a nested frame (a page pushed inside a surface's own stack, say) is a
    /// new LOCAL cascade and should not re-inherit the outer route fade as its own `page_alpha`
    /// a second time; a screen that needs the route fade at any depth reads `nav_page_alpha`
    /// directly, which every nested `DrawFrame` still carries unchanged since nothing but
    /// `with_navigation` ever writes it.
    pub fn navigation(&self) -> NavPresentation {
        NavPresentation {
            page_alpha: self.page_alpha,
            chrome_alpha: self.chrome_alpha,
            view_tab: self.view_tab,
            chrome_lift: 0.0,
            blur_amount: self.blur_amount,
        }
    }

    /// The hit map's only writer (§7.6): records the stop in screen space with insertion index.
    /// `s.rect`/`s.rest_rect` are in the painter's OWN space; the cascade's translate and pop are
    /// folded in here (`Painter::to_screen`) and the stop is clipped to the cascade's clip
    /// intersected with `s.clip` (also in painter space).
    pub fn stop(&mut self, p: Painter, mut s: Stop<H::Elem>) {
        if (p.is_recording() && !stops_in_recording_walk()) || !self.records_stops() {
            return;
        }
        let (rect, _, cascade_clip) = p.to_screen(s.rect);
        let (_, rest, _) = p.to_screen(s.rest_rect);
        let own = Rect::new(s.clip.x + p.dx(), s.clip.y + p.dy(), s.clip.w, s.clip.h);
        s.rect = rect;
        s.rest_rect = rest;
        s.clip = cascade_clip.intersect(own);
        self.stops.push(s);
    }

    /// Open a GL scissor for `r` (painter space) for the rest of the returned scope: the
    /// cascade's clip narrows to it and the scissor is restored when the scope drops — the RAII
    /// replacement for the bare `Painter::clip`/`clip_clear` pair (spec §7.6). Draw the clipped
    /// content through the painter the scope hands back. A scope opened inside another
    /// INTERSECTS with it ([`ClipScope::open_in`]).
    pub fn clip(&mut self, p: Painter, r: Rect) -> ClipScope {
        ClipScope::open_in(p, r)
    }

    /// **Does this walk feed the hit map?** Only the VISIBLE walk does (§7.6). A frame walks the
    /// page closure up to four times — the text prewarm through a recording painter, backdrop
    /// discovery, one replay per blur source (`app::run::draw`) and the visible pass — and the
    /// dispatcher fills `Input`'s hit map from the visible pass alone. A stop placed in any other
    /// walk is layout work thrown away, so every stop producer asks this before it places
    /// anything (`if !f.records_stops() { return; }` at the top of each `record_stops`), and
    /// [`Self::stop`] refuses outside it as the backstop for inline producers.
    pub fn records_stops(&self) -> bool {
        (!self.painter.is_recording() || stops_in_recording_walk()) && !plx_gfx::gfx::blur_source_pass()
    }

    pub fn stops(&self) -> &[Stop<H::Elem>] {
        &self.stops
    }

    /// Deferred renderer work is an effect of this visible frame, never a process wake.
    pub fn is_visible_pass(&self) -> bool {
        !self.painter.is_recording() && !plx_gfx::gfx::blur_source_pass()
            && !crate::frame::backdrop::source_walk()
    }

    pub fn request_frame(&mut self) {
        if self.is_visible_pass() { self.wants_frame = true; }
    }

    pub fn take_frame_request(&mut self) -> bool {
        std::mem::take(&mut self.wants_frame)
    }

    pub fn apply_frame_requests(&mut self, present: &mut plx_machine::present::Present) {
        if self.take_frame_request() { present.note(plx_machine::present::PresentEvent::Wake); }
    }
    pub fn into_stops(self) -> Vec<Stop<H::Elem>> {
        self.stops
    }
}

/// The ROUNDED part of a clip, in screen px: the rounded rect a coverage mask cuts the draws by (the
/// lens of a morphing glass menu), on top of the scissor box. See [`ClipScope::open_rounded_in`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundClip {
    pub rect: Rect,
    pub radius: f32,
}

/// What a [`ClipScope`] holds: the scissor box, and the rounded clip if one is open. The box is the
/// bounding cut every draw gets; the rounding is what the masked programs add (`plx_gfx::gfx::RClip`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub scissor: Rect,
    pub round: Option<RoundClip>,
}

/// A live GL scissor, restored on drop to what was set before it. Scopes NEST by intersection: an
/// inner scope can only narrow the enclosing one, never widen past it, so a widget that clips its
/// own viewport (`TableView::draw`) composes inside an outer panel clip with no knowledge of it.
/// This is the ONE scissor stack — the painter's carried clip (`Painter::clipped`, what the hit
/// map reads) is the same rectangle by construction, so painting and hit-testing cannot drift.
/// Pure bookkeeping on the host test binary (no GL is linked): what is graded is the stack.
///
/// A scope can also carry a ROUNDED clip ([`open_rounded_in`](Self::open_rounded_in)): the scissor
/// is then its bounding box, and the draws that wear a coverage mask (rects, labels, icons) are cut
/// by the arc too. A rect scope opened inside it narrows the box and KEEPS the rounding.
pub struct ClipScope {
    prev: Option<Clip>,
    active: bool,
}

thread_local! {
    /// The clip currently set by a `ClipScope`, if any — what a nested scope restores to.
    static CLIP_STACK: std::cell::Cell<Option<Clip>> = const { std::cell::Cell::new(None) };
}

impl ClipScope {
    /// Open a scissor for `r` (in `p`'s space) for the life of the returned scope, for a caller
    /// that holds only a [`Painter`] (every `TableView`-style widget draw; [`DrawFrame::clip`]
    /// delegates here). The box is `r` placed by the cascade, intersected with the cascade's own
    /// carried clip AND with the scope already open, if any; a recording painter opens nothing.
    /// The rounded clip of an enclosing scope, if any, stays in force.
    pub fn open_in(p: Painter, r: Rect) -> Self {
        // Backdrop DISCOVERY first, as `Painter::clip` does: a discovery painter records, and the
        // backdrop walk keeps its own clip that culls what a scissor would hide. Without the
        // forward, a row scrolled outside a table's frame would be recorded unclipped.
        if !crate::frame::backdrop::discovering() && p.is_recording() {
            return Self::inert();
        }
        Self::open(p.clipped(r).clip_rect(), None)
    }

    /// [`open_in`](Self::open_in) with a ROUNDED clip: `r` (in `p`'s space) with corner `radius`. The
    /// scissor is `r`'s bounding box as `open_in` makes it; the rounding is `r` as `p` PLACES it
    /// (the cascade translate and zoom, NOT intersected with the cascade's clip, which would move the
    /// arc) and the radius under the zoom. Open it on the painter the rect is in, before zooming what
    /// is drawn under it: a zoomed painter then maps its own vertices into the same screen px.
    ///
    /// A radius under half a pixel has no arc to cut and is `open_in` exactly. Rounded clips do not
    /// nest (one lens at a time): a debug build asserts, a release build lets the inner one win while
    /// the scissor still intersects.
    pub fn open_rounded_in(p: Painter, r: Rect, radius: f32) -> Self {
        if !crate::frame::backdrop::discovering() && p.is_recording() {
            return Self::inert();
        }
        let radius = p.px(radius);
        let round = (radius >= 0.5).then(|| RoundClip { rect: p.place(r), radius });
        Self::open(p.clipped(r).clip_rect(), round)
    }

    fn open(screen: Rect, round: Option<RoundClip>) -> Self {
        let prev = CLIP_STACK.with(|c| c.get());
        debug_assert!(
            round.is_none() || prev.is_none_or(|c| c.round.is_none()),
            "rounded clips do not nest: one lens at a time"
        );
        let scissor = prev.map_or(screen, |outer| outer.scissor.intersect(screen));
        let clip = Clip { scissor, round: round.or(prev.and_then(|c| c.round)) };
        CLIP_STACK.with(|c| c.set(Some(clip)));
        apply_scissor(Some(clip));
        Self { prev, active: true }
    }

    fn inert() -> Self {
        Self { prev: None, active: false }
    }

    /// The scissor in force (the innermost open scope), for tests and instruments.
    pub fn current() -> Option<Rect> {
        CLIP_STACK.with(|c| c.get()).map(|c| c.scissor)
    }

    /// The rounded clip in force, if a scope opened one ([`open_rounded_in`](Self::open_rounded_in)).
    pub fn current_round() -> Option<RoundClip> {
        CLIP_STACK.with(|c| c.get()).and_then(|c| c.round)
    }
}

impl Drop for ClipScope {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        CLIP_STACK.with(|c| c.set(self.prev));
        apply_scissor(self.prev);
    }
}

/// Set (or clear) the clip for the current pass: the backdrop walk's own clip while discovery
/// runs (the bounding box only; the walk culls by rectangle, mirroring `Painter::clip`/`clip_clear`),
/// GL otherwise.
fn apply_scissor(c: Option<Clip>) {
    if crate::frame::backdrop::discovering() {
        crate::frame::backdrop::clip(c.map(|c| c.scissor));
    } else {
        gl_scissor(c);
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn gl_scissor(c: Option<Clip>) {
    match c {
        Some(c) => {
            plx_gfx::gfx::clip_set(c.scissor.x, c.scissor.y, c.scissor.w, c.scissor.h);
            plx_gfx::gfx::rclip_set(c.round.map(|r| plx_gfx::gfx::RClip {
                x0: r.rect.x,
                y0: r.rect.y,
                x1: r.rect.x + r.rect.w,
                y1: r.rect.y + r.rect.h,
                radius: r.radius,
            }));
        }
        None => {
            plx_gfx::gfx::clip_clear();
            plx_gfx::gfx::rclip_set(None);
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
fn gl_scissor(_c: Option<Clip>) {}

impl<'a, 'views, H: Host> Deref for DrawFrame<'a, 'views, H> {
    type Target = Cx<'views, H>;
    fn deref(&self) -> &Self::Target {
        self.cx
    }
}

#[cfg(test)]
mod draw_frame_tests {
    use super::*;
    use crate::fixture::FixtureHost;
    use plx_machine::machine::{Cx, EntryId, FocusRead, InputOwner, PressRead, Tick};

    fn cx<'a>(measure: &'a dyn plx_machine::machine::Measure, store: &'a crate::fixture::FixtureView) -> Cx<'a, FixtureHost> {
        Cx {
            views: crate::fixture::FixtureViews { store },
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
        }
    }

    fn stop(r: Rect, clip: Rect) -> Stop<u32> {
        Stop {
            key: FocusKey {
                entry: EntryId(0),
                elem: 1,
            },
            rect: r,
            rest_rect: r,
            clip,
            hover: Hover::Focus,
            activate: Activate::Press,
        }
    }

    /// Spec §7.6: a stop is recorded in SCREEN space — the cascade's translate and pop folded
    /// in, and clipped to the cascade's clip. A tile drawn through `scaled(1.1)` registers its
    /// popped rect; its rest rect is the settled one.
    #[test]
    fn a_stop_is_recorded_in_screen_space_with_the_pop_folded_in() {
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        let p = f.painter.translate(100.0, 50.0).scaled(1.1);
        f.stop(p, stop(Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL));
        let s = &f.stops()[0];
        let want = Rect::new(100.0, 50.0, 100.0, 100.0).scaled(1.1);
        assert_eq!((s.rect.x, s.rect.y, s.rect.w, s.rect.h), (want.x, want.y, want.w, want.h));
        assert_eq!((s.rest_rect.x, s.rest_rect.y, s.rest_rect.w, s.rest_rect.h), (100.0, 50.0, 100.0, 100.0));
    }

    /// **Only the visible walk records stops.** A frame walks the page closure up to four times:
    /// the text prewarm (a recording painter), backdrop discovery, each blur source, and the
    /// visible pass. The hit map is filled from the visible pass alone, so a stop placed by any
    /// other walk is layout work thrown away — Detail's `record_stops` alone places two stops per
    /// episode. `records_stops` is the one answer every stop producer asks before placing, and
    /// `stop` itself refuses outside it.
    #[test]
    fn only_the_visible_walk_records_stops() {
        use crate::frame::backdrop::{self, Z};
        let _guard = plx_base::testlock::serial();
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let record = |painter: Painter| {
            let mut f = DrawFrame::new(&cx, painter);
            f.stop(painter, stop(Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL));
            (f.records_stops(), f.stops().len())
        };
        let sources = std::rc::Rc::new(std::cell::RefCell::new(backdrop::Sources::default()));
        assert_eq!(record(Painter::root()), (true, 1), "the visible pass records");
        assert_eq!(record(Painter::recording()), (false, 0), "the text prewarm walk records nothing");
        {
            let _walk = backdrop::discover(sources.clone());
            assert_eq!(record(Painter::root()), (false, 0), "backdrop discovery records nothing");
        }
        {
            let _walk = backdrop::enter(sources.clone(), Z::OPENER);
            assert_eq!(record(Painter::root()), (false, 0), "a blur source walk records nothing");
        }
        {
            let _walk = backdrop::enter(sources, Z::ALL);
            assert_eq!(record(Painter::root()), (true, 1), "the visible walk records");
        }
    }

    #[test]
    fn frame_demand_is_owned_and_survives_the_current_admission() {
        use plx_machine::present::Present;
        let _serial = plx_base::testlock::serial();
        let measure = crate::fixture::FixtureMeasure;
        let view = crate::fixture::FixtureView::default();
        let context = cx(&measure, &view);
        let mut owner = Present::new();
        let mut other = Present::new();
        let mut worker = Present::global();
        for p in [&mut other, &mut worker] { p.take(0); p.acknowledge(0); }
        assert!(owner.take(0));
        let mut frame = DrawFrame::new(&context, Painter::root());
        frame.request_frame();
        frame.request_frame();
        frame.apply_frame_requests(&mut owner);
        assert!(!frame.take_frame_request(), "the frame effect is consumed exactly once");
        owner.acknowledge(0);
        assert!(owner.peek(1));
        assert!(!other.peek(1));
        assert!(!worker.peek(1));
        assert!(!owner.page_damaged());
        assert!(!owner.page_moving());
        assert!(owner.why().is_none());
    }

    #[test]
    fn source_and_recording_frames_do_not_request_presents() {
        use crate::frame::backdrop::{self, Z};
        let _serial = plx_base::testlock::serial();
        let measure = crate::fixture::FixtureMeasure;
        let view = crate::fixture::FixtureView::default();
        let context = cx(&measure, &view);
        let sources = std::rc::Rc::new(std::cell::RefCell::new(backdrop::Sources::default()));
        let request = |painter| {
            let mut frame = DrawFrame::new(&context, painter);
            frame.request_frame(); frame.take_frame_request()
        };
        assert!(!request(Painter::recording()));
        { let _walk = backdrop::discover(sources.clone()); assert!(!request(Painter::root())); }
        { let _walk = backdrop::enter(sources, Z::OPENER); assert!(!request(Painter::root())); }
        assert!(request(Painter::root()));
    }

    /// A stop under a clipped painter is clipped to the cascade's clip ∩ its own.
    #[test]
    fn a_stop_under_a_clip_carries_the_visible_part_only() {
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        let p = f.painter.translate(10.0, 0.0).clipped(Rect::new(0.0, 0.0, 50.0, 400.0));
        f.stop(p, stop(Rect::new(0.0, 0.0, 100.0, 100.0), Rect::new(0.0, 0.0, 200.0, 40.0)));
        let s = &f.stops()[0];
        // cascade clip: x 10..60; own clip: x 10..210, y 0..40 → x 10..60, y 0..40
        assert_eq!((s.clip.x, s.clip.y, s.clip.w, s.clip.h), (10.0, 0.0, 50.0, 40.0));
    }

    /// The RAII scissor: nested scopes narrow and restore in order, and nothing is left set.
    #[test]
    fn the_clip_scope_nests_and_restores() {
        let _g = plx_base::testlock::serial();
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        assert!(ClipScope::current().is_none());
        {
            let p = f.painter.translate(0.0, 100.0);
            let _outer = f.clip(p, Rect::new(0.0, 0.0, 800.0, 300.0));
            let outer = ClipScope::current().unwrap();
            assert_eq!((outer.x, outer.y, outer.w, outer.h), (0.0, 100.0, 800.0, 300.0));
            {
                let inner_p = p.clipped(Rect::new(0.0, 0.0, 800.0, 300.0));
                let _inner = f.clip(inner_p, Rect::new(100.0, 50.0, 2000.0, 2000.0));
                let inner = ClipScope::current().unwrap();
                assert_eq!((inner.x, inner.y, inner.w, inner.h), (100.0, 150.0, 700.0, 250.0));
            }
            let back = ClipScope::current().unwrap();
            assert_eq!((back.x, back.y), (0.0, 100.0));
        }
        assert!(ClipScope::current().is_none());
    }

    /// **A scope opened without the enclosing painter still intersects**: two sibling widgets
    /// drawing through a bare `Painter` (as `TableView::draw` does) inside an outer panel scope
    /// each get their own viewport narrowed to the panel, and the panel is back in force after
    /// each closes.
    #[test]
    fn a_nested_scope_intersects_the_enclosing_one_and_restores_it() {
        let _g = plx_base::testlock::serial();
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        let panel = Rect::new(100.0, 100.0, 400.0, 300.0);
        let rect = |r: Option<Rect>| r.map(|r| (r.x, r.y, r.w, r.h));
        let outer = f.clip(Painter::root(), panel);
        for viewport in [Rect::new(0.0, 0.0, 300.0, 200.0), Rect::new(450.0, 350.0, 500.0, 500.0)] {
            // a BARE root painter: it does not know about `panel`
            let inner = ClipScope::open_in(Painter::root(), viewport);
            let want = panel.intersect(viewport);
            assert_eq!(rect(ClipScope::current()), Some((want.x, want.y, want.w, want.h)));
            drop(inner);
            assert_eq!(rect(ClipScope::current()), Some((100.0, 100.0, 400.0, 300.0)), "the panel clip is restored");
        }
        drop(outer);
        assert!(ClipScope::current().is_none());
    }

    /// **The rounded scope**: the shared painter's rounded clip. It carries the lens (`p.place(r)`, the
    /// radius in screen px) next to the scissor, which is the bounding box intersected with the scope
    /// already open; a nested rect scope narrows the scissor and keeps the arc; closing restores both.
    #[test]
    fn a_rounded_scope_carries_the_lens_narrows_the_scissor_and_restores_both() {
        let _g = plx_base::testlock::serial();
        let p = Painter::root().translate(30.0, 10.0);
        let r = Rect::new(100.0, 50.0, 300.0, 200.0);
        let rect = |r: Rect| (r.x, r.y, r.w, r.h);
        assert_eq!(ClipScope::current_round(), None);
        let outer = ClipScope::open_in(Painter::root(), Rect::new(0.0, 0.0, 330.0, 120.0));
        let outer_box = ClipScope::current().unwrap();
        {
            let _round = ClipScope::open_rounded_in(p, r, 40.0);
            let placed = p.place(r);
            assert_eq!(ClipScope::current_round(), Some(RoundClip { rect: placed, radius: 40.0 }), "the lens");
            let want = outer_box.intersect(p.clipped(r).clip_rect());
            assert_eq!(rect(ClipScope::current().unwrap()), rect(want), "the scissor is the lens's bounding box ∩ the outer scope");
            {
                let _inner = ClipScope::open_in(Painter::root(), Rect::new(150.0, 70.0, 100.0, 400.0));
                assert_eq!(ClipScope::current_round(), Some(RoundClip { rect: placed, radius: 40.0 }), "a rect scope keeps the arc");
                let narrowed = want.intersect(Rect::new(150.0, 70.0, 100.0, 400.0));
                assert_eq!(rect(ClipScope::current().unwrap()), rect(narrowed), "and narrows the scissor");
            }
            assert_eq!(ClipScope::current_round(), Some(RoundClip { rect: placed, radius: 40.0 }));
            assert_eq!(rect(ClipScope::current().unwrap()), rect(want), "the inner scope restored the rounded one's box");
        }
        assert_eq!(ClipScope::current_round(), None, "closed: no rounding left");
        assert_eq!(rect(ClipScope::current().unwrap()), rect(outer_box), "closed: the outer scope is back");
        drop(outer);
        assert!(ClipScope::current().is_none() && ClipScope::current_round().is_none());
    }

    /// A radius under half a pixel has no arc to cut: the scope is `open_in` exactly. A recording
    /// painter (outside discovery) opens nothing, rounded or not.
    #[test]
    fn a_rounded_scope_with_no_radius_is_a_rect_scope_and_a_recording_painter_opens_nothing() {
        let _g = plx_base::testlock::serial();
        let r = Rect::new(100.0, 50.0, 300.0, 200.0);
        {
            let _s = ClipScope::open_rounded_in(Painter::root(), r, 0.25);
            assert_eq!(ClipScope::current_round(), None);
            let c = ClipScope::current().unwrap();
            assert_eq!((c.x, c.y, c.w, c.h), (100.0, 50.0, 300.0, 200.0), "the scissor is the rect's");
        }
        {
            let _s = ClipScope::open_rounded_in(Painter::recording(), r, 30.0);
            assert_eq!((ClipScope::current(), ClipScope::current_round()), (None, None), "a recording painter is inert");
        }
        assert!(ClipScope::current().is_none());
    }

    /// Rounded inside rounded is a bug, like zoom inside zoom: there is one lens at a time.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "rounded clips do not nest")]
    fn a_rounded_scope_inside_a_rounded_one_is_refused() {
        let _g = plx_base::testlock::serial();
        let _a = ClipScope::open_rounded_in(Painter::root(), Rect::new(0.0, 0.0, 300.0, 200.0), 20.0);
        let _b = ClipScope::open_rounded_in(Painter::root(), Rect::new(10.0, 10.0, 100.0, 100.0), 20.0);
    }

    /// Spec §14 phase 8: `nav_page_alpha` is the ROUTE-level nav dip's page alpha this frame was
    /// built with, and it must survive a container overwriting `page_alpha` with its own local
    /// motion — exactly what `dispatch.rs`'s surface branch does (`f.page_alpha = s.motion.appear`)
    /// so a surface's OWN appear animation, not the outer route fade, drives layout/hit-testing
    /// through `page_alpha`. Before this field existed, a screen that also needed the outer fade
    /// (Settings' scrim, Detail's ambient-sample gate) had nowhere to read it but the `ui::nav`
    /// statics directly — this is the value that replaces that live read.
    #[test]
    fn nav_page_alpha_survives_a_containers_local_page_alpha_overwrite() {
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let navigation = NavPresentation {
            page_alpha: 0.42,
            chrome_alpha: 1.0,
            view_tab: None,
            blur_amount: 0.0,
            chrome_lift: 0.0,
        };
        let mut f = DrawFrame::with_navigation(&cx, Painter::root(), navigation);
        assert_eq!(f.nav_page_alpha, 0.42);
        assert_eq!(f.page_alpha, 0.42, "at construction the two start equal");
        // a surface's own appear motion overwrites `page_alpha` in place, as dispatch.rs does.
        f.page_alpha = 0.9;
        assert_eq!(f.page_alpha, 0.9);
        assert_eq!(f.nav_page_alpha, 0.42, "the outer route fade must survive the overwrite");
    }

    #[test]
    fn transition_presentations_are_independent_and_keep_blur_and_chrome_separate() {
        let first = NavPresentation::from_transition(0.25, 1.0, Some(2), true);
        let second = NavPresentation::from_transition(0.6, 0.4, None, false);
        assert_eq!(first.page_alpha, 1.0);
        assert_eq!(first.blur_amount, 0.75);
        assert_eq!(first.chrome_alpha, 1.0);
        assert_eq!(first.view_tab, Some(2));
        assert_eq!(second.page_alpha, 0.6);
        assert_eq!(second.blur_amount, 0.0);
        assert_eq!(second.chrome_alpha, 0.4);
        assert_eq!(second.view_tab, None);
        assert_eq!(NavPresentation::from_transition(1.0, 1.0, None, false), NavPresentation::default());
    }

    /// `DrawFrame::new` (no explicit navigation) is `NavPresentation::default()`, whose
    /// `page_alpha` is 1.0 — the same rest value `ui::nav::page_alpha()` answers when no route
    /// transition is in flight, so a screen built through the plain constructor sees the same
    /// "at rest" value from `nav_page_alpha` that it used to read live.
    #[test]
    fn nav_page_alpha_defaults_to_the_route_fades_rest_value() {
        let m = crate::fixture::FixtureMeasure;
        let store = crate::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let f = DrawFrame::new(&cx, Painter::root());
        assert_eq!(f.nav_page_alpha, 1.0);
    }
}

#[cfg(test)]
mod composed_owner_tests {
    use super::*;
    use crate::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};

    struct RepairPart { owned: Vec<u32>, placed: Vec<u32>, repaired: u32 }
    impl Focusable<FixtureHost> for RepairPart {
        fn groups(&self, _: &Cx<'_, FixtureHost>, _: &mut Vec<GroupSpec>) {}
        fn group_of(&self, k: &u32, _: &Cx<'_, FixtureHost>) -> Option<GroupId> {
            self.owned.contains(k).then_some(GroupId(0))
        }
        fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, FixtureHost>) -> Step<u32> { Step::Edge }
        fn place(&self, k: &u32, _: &Cx<'_, FixtureHost>, _: At) -> Option<Placed> {
            self.placed.contains(k).then_some(Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None })
        }
        fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            FocusKey { elem: self.repaired, ..want }
        }
        fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, FixtureHost>) -> FocusKey<u32> { unreachable!() }
    }
    impl Part<FixtureHost> for RepairPart {
        fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, FixtureHost>) {}
        fn draw(&mut self, _: &mut DrawFrame<'_, '_, FixtureHost>, _: Rect) {}
    }
    struct Pair([RepairPart; 2]);
    impl Composed<FixtureHost> for Pair {
        fn layout(&self, _: &Cx<'_, FixtureHost>) -> Vec<(PartId, Rect)> {
            vec![(PartId(0), Rect::FULL), (PartId(1), Rect::FULL)]
        }
        fn part(&self, id: PartId) -> &dyn Part<FixtureHost> { &self.0[id.0 as usize] }
        fn part_mut(&mut self, id: PartId) -> &mut dyn Part<FixtureHost> { &mut self.0[id.0 as usize] }
    }
    fn repaired(pair: &Pair, elem: u32) -> u32 {
        let (measure, store) = (FixtureMeasure, FixtureView::default());
        let cx = Cx { views: FixtureViews { store: &store }, tick: Tick::default(), measure: &measure,
            press: PressRead::default(), focus: FocusRead::default(), owner: InputOwner::Entry(EntryId(1)),
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default() };
        composed_reconcile(pair, FocusKey { entry: EntryId(1), elem }, &cx).elem
    }
    fn pair(owned: Vec<u32>, placed: Vec<u32>, repair: u32) -> Pair {
        Pair([RepairPart { owned: vec![0, 1], placed: vec![0, 1], repaired: 1 },
              RepairPart { owned, placed, repaired: repair }])
    }
    #[test]
    fn composed_owner_preserves_each_second_part_answer() {
        for k in [0x4000_0000, 0x4000_0001, 0x8000_0000, u32::MAX] {
            assert_eq!(repaired(&pair(vec![k], vec![k], k), k), k);
        }
    }
    #[test]
    fn composed_owner_repairs_even_a_still_placeable_slot() {
        assert_eq!(repaired(&pair(vec![100, 101], vec![100, 101], 101), 100), 101);
    }
    #[test]
    fn composed_owner_repairs_a_removed_item_before_unrelated_fallback() {
        assert_eq!(repaired(&pair(vec![100], vec![101], 101), 100), 101);
    }
    #[test]
    fn composed_owner_keeps_ordered_fallback_for_unknown_or_unplaceable_repairs() {
        assert_eq!(repaired(&pair(vec![], vec![], 100), 100), 1, "removed band");
        assert_eq!(repaired(&pair(vec![100], vec![], 101), 100), 1, "owner cannot repair");
        assert_eq!(repaired(&pair(vec![100], vec![100], 100), 999), 1, "unknown key");
    }
}

/// Lifecycle-sequence helper (§3.4): the events a Push delivers, in order, as `(entry, event)`.
/// Kept as data so the dispatcher's tables are testable without a container.
pub fn push_sequence<H: Host>(
    old_top: Option<EntryId>,
    new: EntryId,
    focus: FocusTarget<H::Elem>,
) -> Vec<(EntryId, ScreenEvent<H>)> {
    let mut v = Vec::new();
    if let Some(old) = old_top {
        v.push((old, ScreenEvent::WillLeave(Leave::Deeper)));
    }
    v.push((new, ScreenEvent::Mount));
    v.push((new, ScreenEvent::Enter(Enter::Fresh { focus })));
    if let Some(old) = old_top {
        v.push((old, ScreenEvent::Cover));
    }
    v
}

/// The events a Pop delivers, in order (§3.4).
pub fn pop_sequence<H: Host>(top: EntryId, under: Option<EntryId>) -> Vec<(EntryId, ScreenEvent<H>)> {
    let mut v = vec![
        (top, ScreenEvent::WillLeave(Leave::ForGood)),
        (top, ScreenEvent::Unmount),
    ];
    if let Some(u) = under {
        v.push((u, ScreenEvent::Uncover));
        v.push((u, ScreenEvent::Enter(Enter::Restored)));
    }
    v
}
