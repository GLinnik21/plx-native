//! Shared full-screen route geometry used by Settings and its children.
//!
//! A route screen is a two-column composition, not a bag of coordinates: a narrative column on
//! the left, a content column on the right, and one bottom action row. Screens supply words and
//! state; this module owns every spatial relationship between them.
//!
//! # Where BACK goes is the CRUMB, never a hint in the action band
//!
//! A route in this family that was arrived at from somewhere says so on one caption line above
//! its title: a left-pointing chevron and the name of the place BACK returns to (see
//! [`RouteLayout::draw_narrative`]). That line replaced the `Press [BACK] to return` hint this
//! family used to draw in the bottom action row, and the trade is the point rather than a tidy-up
//! — the hint spent a whole 60px band restating a key the remote already has, and it could not
//! say where the key WENT, which on a three-deep push (Settings → Legal notices → a document) is
//! the only part anybody needs. Freeing the band is what lets first-run consent put its two
//! answers there ([`RouteLayout::action_pair`]) instead of hiding them among the readable rows of
//! its list.
//!
//! **The rule for `None` is "BACK does not go anywhere inside the app", not "the Settings root".**
//! Three routes qualify today and a hard count here would be the fourth transcription of a number
//! this project keeps letting rot — take the census with
//! `git grep -A2 'draw_narrative(' -- 'rust-modules/src/ui/*.rs'`. They are the Settings modal
//! (BACK leaves the family), the FIRST first-run consent question (sign-in is behind it and
//! cannot be undone) and the QR sign-in itself (there is no app behind it yet).
//!
//! # ONE focus model for the whole family
//!
//! Every screen here is the same two regions: the **content column** on the right (a table of
//! rows, or a document), and the **action band** at the bottom of the left column (0, 1 or 2
//! controls). [`RouteFocus`] holds where focus is and [`RouteShape`] is what a screen tells it
//! about itself this frame; the rules below are pure, live here, and every screen in the family
//! obeys all of them. They exist because the family had drifted into five different answers —
//! Legal entered on RIGHT and left on LEFT while the Settings root ignored both; the Home editor
//! reached its Done pill only with LEFT; Privacy & data jumped focus onto Done after a toggle and
//! then had no way back; and first-run consent's two answers could not be left rightward at all.
//!
//! 1. **UP/DOWN walk the content column.** A table moves its selection; a document scrolls.
//! 2. **DOWN off the LAST row enters the band**, on the control the band's cursor last rested on.
//! 3. **UP leaves the band for the content column**, on the row it left from — so 2 and 3 are an
//!    exact round trip.
//! 4. **DOWN on the band is the floor.** The band is the bottom of the screen; there is nothing
//!    under it.
//! 5. **LEFT from the content column enters the band**, on the same remembered control as rule 2.
//!    (The band sits in the LEFT column, so this is the spatial move, not a shortcut.)
//! 6. **LEFT inside the band steps to the control on its left.**
//! 7. **RIGHT inside the band steps to the control on its right; from the trailing control it
//!    returns to the content column** — the inverse of rule 5.
//! 8. **RIGHT on a row that opens nested content (a chevron row) enters it**, exactly as OK does.
//!    RIGHT on any other row does nothing.
//! 9. **LEFT is BACK once there is nothing further to its left, UNLESS the screen is holding an
//!    uncommitted edit** ([`RouteShape::uncommitted`]). This is what makes the crumb literal — it
//!    is drawn as a left-pointing chevron naming where BACK goes, and LEFT follows it. The
//!    exception is the one thing a directional key must never do: BACK on these screens DISCARDS
//!    (Privacy & data drops its draft, the Home editor drops its — nothing was written), and
//!    overshooting the band's leading control by one press is not consent to lose an edit.
//!    **`uncommitted` is stated by the screen and is NOT "the screen has a band".** That inference
//!    was this rule's first justification and it is false in both directions: the Home editor's
//!    `Try again` is a band with no edit behind it, and first run's two answers are a band whose
//!    BACK records nothing. So Settings-mode Retry and first-run consent both let LEFT follow
//!    their crumb, while a dirty draft does not.
//! 10. **Focus can never rest on nothing.** [`RouteFocus::settle`] runs after every rebuild: a band
//!    that has gone away hands focus back to the content column, and a content column with no rows
//!    hands it to the band. Privacy & data used to lose focus entirely by toggling a value back —
//!    it parked focus on a Done that the same edit had just removed.
//! 11. **The pointer PARKS focus and nothing else.** Hover moves focus to the row or band control
//!    under the pointer on every screen in the family ([`ActionRow::hit`] +
//!    [`crate::ui::table::TableView::hit_row`]); a click activates whatever is parked; a click that
//!    parks nothing does nothing.

use crate::ui::consts::SAFE;
use crate::ui::icons::{self, Icon};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{AmbientWash, ControlPalette};
use crate::ui::{theme, Painter, Rect, Spring};

/// The left column is the same editorial measure as the Home hero.  Reusing that named measure is
/// what makes first-run routes and Settings feel like one family instead of two similar layouts.
const NARRATIVE_W: f32 = crate::ui::home::HERO_COL_W;
/// Two visually separate columns need a region gap, not a row gap.  Expressed entirely on the
/// spacing ladder so a retune of that ladder moves every route together.
const COLUMN_GAP: f32 = theme::space::XL * 2.0 + theme::space::LG + theme::space::SM;
const TOP_INSET: f32 = theme::space::XL + theme::space::MD;
const ACTION_H: f32 = crate::ui::widgets::StatusOverlay::CTRL_H;
/// The crumb's mark, drawn at the smallest rung a mark is worn at anywhere in the product. It is
/// a return affordance rather than a heading, so it may not outweigh the caption beside it.
const CRUMB_MARK: f32 = theme::size::MICRO as f32;
/// The crumb LINE's height — the mark's, because the mark is the taller of the two things on it
/// (a `CAPTION` cap band is a few px shorter) and both are centred in it.
///
/// A constant rather than a measured cap height, and that is not a shortcut: `text::cap_h` opens
/// the font through SDL2_ttf, which the host suite cannot LINK, so a measured band would make
/// every route's vertical flow ungradeable off-device — the boundary `ui/CLAUDE.md` records as
/// stopping the whole suite linking rather than skipping one test.
const CRUMB_BAND: f32 = CRUMB_MARK;

/// Where the crumb's word starts so that its CAP BAND is centred on the mark's centre `cy`.
///
/// `TextView` pins line 0's cap band to the frame's top, and the mark is centred in the band, so
/// handing the word the band's top put its cap centre a few px ABOVE the chevron's — the word read
/// as riding high off the line on every route that wears a crumb. The mark's ink is symmetric
/// about its box (`chevron-left.svg` spans y=6..18 of 24), so its centre is the box centre and
/// the two meet on `cy`. Takes the cap height as a PARAMETER for the reason `CRUMB_BAND` is a
/// constant: measuring it opens the font through SDL2_ttf, which the host suite cannot link.
fn crumb_label_top(cy: f32, cap_h: f32) -> f32 {
    cy - cap_h * 0.5
}
const PUSH_K: f32 = 200.0;
const PARENT_TRAVEL: f32 = 0.35;
const CHILD_LEAD: f32 = 0.22;

/// Density of the Settings-family ambient ground.
///
/// The source is already an UltraBlur envelope (or four broad framebuffer means), so increasing
/// this number does not make it *more blurred*; it only lets more source light through.  Reusing
/// the shared ground weight keeps bright green/yellow artwork below the section-label contrast
/// floor while preserving its hue.
const GROUND_W: f32 = AmbientWash::GROUND_W;

/// The fixed ground under a Settings-family route.
///
/// It deliberately stores a four-corner colour envelope rather than a downsampled screenshot.
/// That is effectively a blur with a support wider than the screen: title glyphs, faces and poster
/// edges cannot survive it, but the host artwork's light still does.  Once latched it never samples
/// again; nested push/back transitions therefore move content over one stationary ground.
#[derive(Clone, Copy)]
pub(crate) struct RouteGround {
    wash: AmbientWash,
    key: [f32; 3],
    latched: bool,
}

impl RouteGround {
    pub(crate) const fn new() -> Self {
        Self {
            wash: AmbientWash::flat(theme::SURFACE_APP),
            key: [
                theme::SURFACE_APP[0],
                theme::SURFACE_APP[1],
                theme::SURFACE_APP[2],
            ],
            latched: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.wash.jump([theme::SURFACE_APP; 4]);
        self.key = [
            theme::SURFACE_APP[0],
            theme::SURFACE_APP[1],
            theme::SURFACE_APP[2],
        ];
        self.latched = false;
    }

    fn latch(&mut self, corners: [[f32; 3]; 4], key: [f32; 3]) {
        if self.latched {
            return;
        }
        self.wash.jump(AmbientWash::keyed(corners, [GROUND_W; 4]));
        self.key = key;
        self.latched = true;
    }

    fn latch_target(&mut self, target: [[f32; 4]; 4]) {
        if self.latched {
            return;
        }
        self.wash.jump(target);
        self.key = mean_target_key(target);
        self.latched = true;
    }

    /// Freeze the page that was already drawn this frame. Its one caller is the Settings modal,
    /// which opens over Home; first-run consent takes [`Self::draw_home`] instead, because since
    /// the consent question moved ahead of the profile picker it usually has no rendered host to
    /// sample at all.
    pub(crate) fn draw_host(&mut self, p: Painter) {
        if !self.latched {
            let sample = crate::gfx::sample_modal_ambient();
            self.latch(sample.corners, sample.key);
        }
        self.wash.draw(p, Rect::FULL);
    }

    /// Seed a pre-Home route from the same hero metadata Home will use when it appears. Shared
    /// Sources has no rendered host to sample yet, so this is the semantic equivalent of freezing
    /// Home after an infinitely broad blur.
    ///
    /// **Three tiers, in order, and each one only reachable when the one before it has nothing.**
    /// (1) Home's OWN hero, when this boot has already fetched one — the ordinary case for Settings
    /// and Legal, opened well after Home exists. (2) Failing that, the LAST hero envelope this
    /// television ever showed (`plex::session::last_hero`) — the case that motivated this: since
    /// the device consent question moved ahead of the profile picker, its usual host is the picker
    /// with no hub fetched yet, so tier 1 is empty on almost every ordinary boot, not only a fresh
    /// device's first one. (3) Only a genuinely fresh television — signed in for the first time,
    /// never having rendered a hero at all — falls all the way to the design system's authored
    /// atmosphere (`theme::ROUTE_GROUND_FALLBACK`). Recording the seed for tier 2 is this
    /// function's other job whenever tier 1 succeeds — see `plex::session::record_last_hero`.
    pub(crate) fn draw_home(&mut self, p: Painter) {
        if !self.latched {
            if let Some(hero) = crate::ui::home::hero_item().filter(|m| m.has_blur) {
                self.latch(hero.blur, mean_key(hero.blur));
                crate::plex::session::record_last_hero(hero.blur);
            } else if let Some(blur) = crate::plex::session::last_hero() {
                self.latch(blur, mean_key(blur));
            } else {
                self.latch_target(theme::ROUTE_GROUND_FALLBACK);
            }
        }
        self.wash.draw(p, Rect::FULL);
    }

    /// Draw a pre-content route on the product's authored fallback atmosphere.
    ///
    /// Login and the profile ceremony have no Home frame and no media item to sample.  They still
    /// belong to the same route family, so they take the same broad graphite/amber envelope as an
    /// artwork-less first-run screen instead of inventing another flat background locally.
    pub(crate) fn draw_default(&mut self, p: Painter) {
        if !self.latched {
            self.latch_target(theme::ROUTE_GROUND_FALLBACK);
        }
        self.wash.draw(p, Rect::FULL);
    }

    pub(crate) fn palette(&self) -> ControlPalette {
        ControlPalette::ambient(self.key)
    }

    pub(crate) fn is_latched(&self) -> bool {
        self.latched
    }
}

fn mean_key(corners: [[f32; 3]; 4]) -> [f32; 3] {
    let mut key = [0.0; 3];
    for corner in corners {
        for channel in 0..3 {
            key[channel] += corner[channel] * 0.25;
        }
    }
    key
}

fn mean_target_key(corners: [[f32; 4]; 4]) -> [f32; 3] {
    mean_key(corners.map(|c| [c[0], c[1], c[2]]))
}

/// The one nested-route transition used by Settings documents.
///
/// Only content moves: the host ground is drawn outside these painters and therefore remains
/// fixed. The parent exits left while fading; the child leads from the right while appearing.
/// Reversing `open` produces the exact inverse spring, so BACK never invents a second motion.
pub(crate) struct RoutePush {
    progress: Spring,
}

impl RoutePush {
    pub(crate) const fn new() -> Self {
        Self {
            progress: Spring::at(0.0),
        }
    }

    pub(crate) fn jump(&mut self, open: bool) {
        self.progress.jump(if open { 1.0 } else { 0.0 });
    }

    pub(crate) fn update(&mut self, open: bool, dt: f32) {
        self.progress.step(if open { 1.0 } else { 0.0 }, PUSH_K, dt);
    }

    pub(crate) fn amount(&self) -> f32 {
        self.progress.pos.clamp(0.0, 1.0)
    }

    /// Seed this push's spring — position AND velocity, not just [`amount`](Self::amount)'s
    /// clamped read-out — from ANOTHER push's current state. For a screen that gets its OWN
    /// dedicated `RoutePush` split out of a shared one so a sibling's fade cannot bleed into it
    /// (`settings::HOME_PUSH` off `settings::CHILD`, 2026-09-04), a caller that starts the new
    /// push cold at rest whenever it starts driving it is right in the ordinary case — both begin
    /// at 0 together — but wrong the moment the shared push is NOT at rest, because a sibling
    /// (Privacy/Legal) was open a moment ago and is still reversing out of it: the shared root
    /// would resume mid-fade while the freshly-opened child painted itself from a cold zero,
    /// which is two supposedly-complementary halves of one crossfade disagreeing about how far it
    /// has travelled (Codex review, 2026-09-04). Call this once, on the RISING EDGE of the new
    /// push starting to drive toward open — detected from that flag's own transition, never from
    /// `self.amount() == 0.0`: this type's spring only APPROACHES its target (see [`amount`]'s
    /// own doc), so a settled-closed push sits at some tiny positive residual, not exactly zero,
    /// and an equality guard reads as "already synced" forever after the first open (Codex review,
    /// 2026-09-04, round 3 — the first version of this call site's guard made exactly that
    /// mistake). Unconditional on the edge is deliberately simpler than gating on `other` being
    /// away from rest: when `other` already IS at rest the copy is a harmless no-op.
    pub(crate) fn sync_to(&mut self, other: &RoutePush) {
        self.progress = other.progress;
    }

    /// **Is the push parked at the endpoint `open` names?** Rule 11's timing guard.
    ///
    /// A key press acts on the LOGICAL state and is right to: `DOCUMENT_OPEN` goes false the
    /// instant BACK is pressed, and the next UP belongs to the list underneath whatever the
    /// animation is still showing. A pointer hit cannot borrow that reasoning, because it is
    /// POSITIONAL — it means "the thing I can see at these coordinates", and mid-push the thing at
    /// those coordinates is somewhere else, half-transparent, or both. The rects an `ActionRow`
    /// records and the frame a `TableView` is hit-tested against are both FINAL-position, so every
    /// pointer path in this family refuses until the layer it belongs to has arrived.
    pub(crate) fn settled(&self, open: bool) -> bool {
        let t = self.amount();
        if open {
            t > 0.999
        } else {
            t < 0.001
        }
    }

    pub(crate) fn parent(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(1.0 - t)
            .translate(-PARENT_TRAVEL * Rect::FULL.w * t, 0.0)
    }

    pub(crate) fn child(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(t)
            .translate(CHILD_LEAD * Rect::FULL.w * (1.0 - t), 0.0)
    }
}

/// **Where a press came from**, which decides what may cancel it.
///
/// `ui::press` holds ONE press at a time and assumes focus cannot move while it is in flight. The
/// nav keys pay that by cancelling; the pointer has to pay it differently, and the difference is
/// this enum. A press that began under the POINTER is bound to the coordinates it began on — the
/// pointer leaving that control, dead space included, is the person taking the click back. A press
/// that began on the OK KEY is bound to the focus stop instead: hover that moves the ring off it
/// cancels, but hover across dead space is not a retraction of a key the user is still holding.
/// Conflating them cancelled every key press the moment the cursor twitched over nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PressFrom {
    Key,
    Pointer,
}

/// A focus stop on a route screen — see the module doc's rule list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Stop {
    /// The content column: a row of the table, or the document reader.
    Content,
    /// Control `i` of the bottom action band, counted from its leading (left) edge.
    Band(usize),
}

/// What a screen must DO once a shared rule has decided. The [`RouteFocus`] has already moved by
/// the time one of these comes back; everything else is the screen's own to perform, because only
/// it knows whether its content column is a table or a document and what its rows open.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RouteStep {
    /// A wall: nothing moved and nothing happens.
    Wall,
    /// Focus moved. Repaint; there is nothing else to do.
    Moved,
    /// The content column takes this vertical delta — a table moves its selection, a document
    /// scrolls.
    Scroll(i32),
    /// Activate the focused row: enter the nested content behind it, exactly as OK does.
    Enter,
    /// Leave this screen, exactly as BACK does.
    Back,
}

/// What the shared rules need to know about a route screen at the moment a key arrives. A screen
/// answers it from live state every time rather than caching it, so a list that fills in on a
/// worker (the Home editor's roster) or a band that appears with the first edit (Privacy & data's
/// Done) cannot leave the rules reasoning about a shape that is no longer on screen.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RouteShape {
    /// How many controls the bottom action band is drawing. `0` = this screen has no band.
    pub(crate) band: usize,
    /// Whether the content column has a focusable row. `false` for a document reader, and for a
    /// table whose rows have not landed yet.
    pub(crate) rows: bool,
    /// Whether the table's selection is on its LAST focusable row (rule 2).
    pub(crate) at_last_row: bool,
    /// Whether the focused row opens nested content — a chevron row (rule 8).
    pub(crate) opens: bool,
    /// **Is leaving on LEFT UNPROVEN to be lossless?** Rule 9's only guard, and it is answered by
    /// the screen rather than inferred from the band, because the two are not the same question:
    /// the Home editor's `Try again` band commits nothing, and first run's two answers record
    /// nothing until OK. A screen that answers `true` never leaves on LEFT.
    ///
    /// **The negative framing is deliberate and it is not the same as "an edit exists."** A screen
    /// can also be unable to TELL — the Home editor's baseline is captured once and its roster
    /// lands on a worker, so an editor opened before discovery answered cannot say whether a row
    /// that arrived afterwards has been touched. `true` there is a refusal to trust an answer, not
    /// a claim about an edit; erring toward the wall costs one press of BACK, and erring the other
    /// way loses the edit silently. Read it as "BACK is not proven lossless" (Codex review,
    /// 2026-09-04).
    pub(crate) uncommitted: bool,
}

impl RouteShape {
    /// The shape of a document route: no band, no rows, nothing to enter. Named because three
    /// screens push the same reader and none of them should be spelling four fields out.
    pub(crate) const fn document() -> Self {
        Self {
            band: 0,
            rows: false,
            at_last_row: false,
            opens: false,
            // A document is a READ. There is nothing in it to lose, on any of the three routes
            // that push one, so LEFT always walks back out of it.
            uncommitted: false,
        }
    }
}

/// **Where focus is on a route screen, and the whole of how it moves.** The module doc's eleven
/// rules are implemented here and nowhere else; a screen owns the words, the geometry and what its
/// rows DO, and delegates every "which stop is focused now" question to this.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct RouteFocus {
    on_band: bool,
    /// The band control the ring returns to. Remembered while focus is in the content column, so
    /// rules 2 and 5 both land back where the band was left rather than snapping to its leading
    /// control.
    cursor: usize,
}

impl RouteFocus {
    /// Open in the content column — a Settings child arrives on its list.
    pub(crate) const fn content() -> Self {
        Self {
            on_band: false,
            cursor: 0,
        }
    }
    /// Open on the band's leading control — first run arrives on its answer, because the defaults
    /// are already an answer and the screen opens on the way out of itself.
    pub(crate) const fn band() -> Self {
        Self {
            on_band: true,
            cursor: 0,
        }
    }

    pub(crate) fn stop(self) -> Stop {
        if self.on_band {
            Stop::Band(self.cursor)
        } else {
            Stop::Content
        }
    }
    pub(crate) fn on_content(self) -> bool {
        !self.on_band
    }
    /// The band control holding focus, or `None` when focus is in the content column. This is the
    /// value an [`ActionRow::step`] wants, and the one `focus_is_ctl` is built on.
    pub(crate) fn band_index(self) -> Option<usize> {
        self.on_band.then_some(self.cursor)
    }
    /// The band control the ring would RETURN to, focused or not — what a draw uses to decide
    /// which of two answers wears the ring.
    pub(crate) fn cursor(self) -> usize {
        self.cursor
    }

    pub(crate) fn to_content(&mut self) {
        self.on_band = false;
    }
    pub(crate) fn to_band(&mut self, i: usize) {
        self.on_band = true;
        self.cursor = i;
    }

    /// **Rule 10: focus may never rest on nothing.** Run after any rebuild and before any rule, so
    /// a band that has just gone away hands focus back to the list and a list with no rows yet
    /// hands it to the band. Reports whether it had to move anything, which is what a caller
    /// invalidates on.
    pub(crate) fn settle(&mut self, s: RouteShape) -> bool {
        let before = *self;
        if self.on_band {
            if s.band == 0 {
                // The control focus was on is gone. The content column is the honest home even
                // when it has no rows either: that state is a list still loading, and the next
                // settle puts focus back on the band if one appears first.
                self.on_band = false;
            } else if self.cursor >= s.band {
                self.cursor = s.band - 1;
            }
        } else if !s.rows && s.band > 0 {
            self.to_band(self.cursor.min(s.band - 1));
        }
        *self != before
    }

    /// Rules 1–4.
    pub(crate) fn updown(&mut self, s: RouteShape, delta: i32) -> RouteStep {
        self.settle(s);
        if self.on_band {
            if delta < 0 && s.rows {
                self.to_content();
                RouteStep::Moved
            } else {
                RouteStep::Wall
            }
        } else if s.rows && delta > 0 && s.band > 0 && s.at_last_row {
            self.to_band(self.cursor.min(s.band - 1));
            RouteStep::Moved
        } else {
            RouteStep::Scroll(delta)
        }
    }

    /// Rules 5, 6 and 9.
    pub(crate) fn left(&mut self, s: RouteShape) -> RouteStep {
        self.settle(s);
        if self.on_band {
            if self.cursor > 0 {
                self.to_band(self.cursor - 1);
                RouteStep::Moved
            } else {
                // Off the band's leading edge there is nothing further left INSIDE the screen, so
                // rule 9's one guard decides: leave, unless leaving would discard an edit.
                self.out(s)
            }
        } else if s.band > 0 {
            self.to_band(self.cursor.min(s.band - 1));
            RouteStep::Moved
        } else {
            self.out(s)
        }
    }

    /// Rule 9's guard alone: LEFT off the left-most stop is BACK unless the screen says it is
    /// holding an edit that BACK would throw away.
    fn out(&self, s: RouteShape) -> RouteStep {
        if s.uncommitted {
            RouteStep::Wall
        } else {
            RouteStep::Back
        }
    }

    /// Rules 7 and 8.
    pub(crate) fn right(&mut self, s: RouteShape) -> RouteStep {
        self.settle(s);
        if self.on_band {
            if self.cursor + 1 < s.band {
                self.to_band(self.cursor + 1);
                RouteStep::Moved
            } else if s.rows {
                self.to_content();
                RouteStep::Moved
            } else {
                RouteStep::Wall
            }
        } else if s.opens {
            RouteStep::Enter
        } else {
            RouteStep::Wall
        }
    }
}

/// **The shared PRESS SURFACE for a route's bottom action row** — every control
/// [`RouteLayout::action_pair`] (or a single-control action band) lays out owns one of these,
/// rather than a private [`widgets::CtlPop`] field of its own.
///
/// It exists because a control face is not a table row: focus arriving on it must grow a real
/// [`widgets::CtlPop`] pop, and a click must dip and ring back exactly like every other control
/// face in the app (`ui/press.rs`'s `begin_ctl`/`take_commit`, folded into [`Self::scale`] for
/// free — `CtlPop::scale` already applies `press::scale()` to whichever index is focused). Before
/// this existed, `consent.rs`'s two answers drew through `Button::focused()` alone — no pop, no
/// dip — while `onboard.rs`'s single action pill built its own private `CtlPop<1>` to get both.
/// One name for the thing every action row needs is what makes the second screen a two-line
/// change instead of a second hand-rolled spring.
///
/// `N` is the row's own control count — `2` for consent's Share/Don't-share, `1` for a lone Done
/// or Start-watching pill. Nothing here draws a button: a caller still builds its own
/// [`widgets::Button`]/[`widgets::CircleButton`] at its own rect and label, and passes
/// [`Self::scale`] to it — the geometry and the words stay the screen's, only the press machinery
/// is shared.
pub(crate) struct ActionRow<const N: usize> {
    pop: crate::ui::widgets::CtlPop<N>,
    /// Each control's drawn frame, recorded at draw for the pointer — the `TOOL_RECTS` idiom.
    /// Parked OFF the panel rather than at the origin, because [`Rect::contains`] is inclusive and
    /// a zero-size rect at (0,0) would "contain" a click at exactly (0,0).
    rects: [Rect; N],
}

/// Where a control's frame sits while it is not drawn — see [`ActionRow::rects`].
const OFF_PANEL: Rect = Rect::new(-1.0, -1.0, 0.0, 0.0);

impl<const N: usize> ActionRow<N> {
    pub(crate) const fn new() -> Self {
        Self {
            pop: crate::ui::widgets::CtlPop::new(),
            rects: [OFF_PANEL; N],
        }
    }

    /// Record control `i`'s drawn frame, from the same place the button itself is drawn. Rule 11:
    /// this is the whole of what makes an action band hoverable and clickable, and doing it at
    /// draw is what keeps the hit rect and the painted pill the same object.
    pub(crate) fn place(&mut self, i: usize, r: Rect) {
        if let Some(slot) = self.rects.get_mut(i) {
            *slot = r;
        }
    }

    /// Forget every frame — call this on the branch that does NOT draw the band, so a control that
    /// is not on screen is not hit-testable either. (Settings' Done exists only while the draft
    /// differs from the stored answer; a stale rect would leave a live click target where it used
    /// to be.)
    pub(crate) fn clear(&mut self) {
        self.rects = [OFF_PANEL; N];
    }

    /// Which control is under the pointer, or `None` for dead space.
    pub(crate) fn hit(&self, mx: f32, my: f32) -> Option<usize> {
        self.rects
            .iter()
            .position(|r| r.w > 0.0 && r.h > 0.0 && r.contains(mx, my))
    }

    /// Advance every control's pop toward its target for this frame. `focused` is the index of the
    /// control CURRENTLY holding focus in this row, or `None` when nothing in it does — the row is
    /// hidden, or the route's focus is elsewhere (the list, a different band).
    pub(crate) fn step(&mut self, focused: Option<usize>, dt: f32) {
        self.pop.step(focused, dt);
    }

    /// Control `i`'s drawn scale this frame — the focus pop, and, once focused and only once the
    /// caller has armed [`crate::ui::press::begin_ctl`] on its OK-down, the tvOS press dip/ring on
    /// top of it (`CtlPop::scale`'s own fold). Pass straight to `Button::scale`; never draw an
    /// action-row control without it, or it arrives at focus with no pop and clicks with no dip.
    pub(crate) fn scale(&self, i: usize) -> f32 {
        self.pop.scale(i)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct RouteLayout {
    pub(crate) narrative: Rect,
    pub(crate) content: Rect,
    pub(crate) action: Rect,
}

impl RouteLayout {
    pub(crate) fn screen() -> Self {
        let top = SAFE.y + TOP_INSET;
        let bottom = SAFE.y + SAFE.h;
        let narrative = Rect::new(SAFE.x, top, NARRATIVE_W, bottom - top);
        let content_x = narrative.x + narrative.w + COLUMN_GAP;
        let content = Rect::new(content_x, top, SAFE.x + SAFE.w - content_x, bottom - top);
        let action = Rect::new(narrative.x, bottom - ACTION_H, narrative.w, ACTION_H);
        Self {
            narrative,
            content,
            action,
        }
    }

    /// Frame for a table whose first section has a label.
    ///
    /// [`TableView`](crate::ui::table::TableView) owns internal breathing room above that label.
    /// The route owns the external alignment contract: the label's cap-top sits on the same anchor
    /// as the narrative title.  Expanding the frame upward by the table-owned inset joins those
    /// two facts without either component copying the other's padding.
    pub(crate) fn sectioned_table(self) -> Rect {
        let inset = crate::ui::table::FIRST_HEADER_CAP_OFFSET;
        Rect::new(
            self.content.x,
            self.content.y - inset,
            self.content.w,
            self.content.h + inset,
        )
    }

    /// Frame for a content column that is PLAIN TEXT rather than a table.
    ///
    /// **A document hangs from the TITLE's anchor, not from the crumb's**, and that is the whole
    /// of the fix for "on Settings screens that contain plain text rather than a table, the title
    /// appears too high and is aligned roughly with the Back navigation control". A table's frame
    /// is lifted by [`Self::sectioned_table`] so its first section LABEL lands on the shared top
    /// guide — the guide the crumb also sits on — and the first thing that reads as content, the
    /// first row's own label, therefore lands well below it, beside the narrative title. A
    /// document has no section label to spend that band on, so drawn at the bare `content` rect
    /// its first line — which on every document here is a bold all-caps heading, i.e. exactly what
    /// a reader takes for the screen's title — landed ON the crumb's line, a whole title block
    /// above the title it belongs to.
    ///
    /// So: one geometry rule in the layout, not an offset per screen. Both anchors are now used by
    /// both columns — the guide carries the crumb and a table's first label, the title anchor
    /// carries the narrative title and a document's first line.
    ///
    /// `has_crumb` for the same reason [`Self::narrative_top`] takes it: the title anchor moves by
    /// exactly the crumb's band, and a document on a crumbless route must follow it up rather than
    /// leaving a gap nothing explains.
    pub(crate) fn document(self, has_crumb: bool) -> Rect {
        let top = self.narrative_top(has_crumb);
        Rect::new(
            self.content.x,
            top,
            self.content.w,
            (self.content.y + self.content.h - top).max(0.0),
        )
    }

    /// Place two controls on one action row, as ONE GROUP.
    ///
    /// Their relationship belongs here: the leading one starts on the shared margin, the trailing
    /// one follows by [`widgets::CONTROL_GAP`], and both inherit the action band's Y/height. A
    /// screen supplies measured widths, never a second pair of coordinates.
    ///
    /// **Its one caller is first-run consent's two answers, and they are EQUALS** — no primary,
    /// no secondary, no danger face. This doc used to describe a primary beside a BACK
    /// affordance (`Start watching` + `Press [BACK] to return`), which is the pairing the crumb
    /// deleted from this family; there is no BACK affordance in an action band any more. The gap
    /// followed that change: it was `space::LG` 40, the REGION rung for a primary standing beside
    /// a separate hint, and two peer answers at that distance read as two unrelated controls
    /// sharing a row rather than one question's two faces.
    pub(crate) fn action_pair(self, leading_w: f32, trailing_w: f32) -> (Rect, Rect) {
        let leading = Rect::new(self.action.x, self.action.y, leading_w, self.action.h);
        let trailing = Rect::new(
            leading.x + leading.w + crate::ui::widgets::CONTROL_GAP,
            self.action.y,
            trailing_w,
            self.action.h,
        );
        debug_assert!(trailing.x + trailing.w <= self.action.x + self.action.w);
        (leading, trailing)
    }

    /// Draw the return crumb — `‹ <where BACK goes>` — in the band whose top is at `top`.
    ///
    /// The gap between mark and word is measured to the chevron's INK, not to its box: the asset
    /// carries about a third of its width as bearing, so a box-to-text gap on a spacing rung comes
    /// out visibly loose and the pair stops reading as one object. Same rule the player HUD's
    /// transport slot uses, and [`icons::ink_x`] is where the numbers live.
    /// The y the TITLE starts at, given whether a crumb precedes it.
    ///
    /// Pure and separate from [`Self::draw_narrative`] because it is the whole of the crumb's
    /// layout cost, and a test that cannot call the draw (it measures through SDL2_ttf) can still
    /// grade it. The first version of this test compared `RouteLayout::screen().content.y` with
    /// its own `l.content.y` — a value against itself, which cannot fail.
    pub(crate) fn narrative_top(self, has_crumb: bool) -> f32 {
        if has_crumb {
            self.narrative.y + CRUMB_BAND + theme::space::SM
        } else {
            self.narrative.y
        }
    }

    fn draw_crumb(self, p: Painter, top: f32, back_to: &str) {
        let h = CRUMB_BAND;
        let cy = top + h * 0.5;
        icons::draw(
            p,
            Icon::ChevronLeft,
            Rect::new(
                self.narrative.x,
                cy - CRUMB_MARK * 0.5,
                CRUMB_MARK,
                CRUMB_MARK,
            ),
            theme::TEXT_TERTIARY,
        );
        let (_, ink_r) = icons::ink_x(Icon::ChevronLeft);
        let x = self.narrative.x + CRUMB_MARK * ink_r + theme::space::XS;
        let ty = crumb_label_top(cy, crate::text::cap_h(theme::size::CAPTION, 0));
        TextView::new(back_to, theme::size::CAPTION, theme::TEXT_TERTIARY)
            .max_lines(1)
            .draw(p, Rect::new(x, ty, self.narrative.x + self.narrative.w - x, h));
    }

    /// Draw a measured crumb→title→copy flow.  Each block begins after the previous one's actual
    /// wrapped height, never after a screen-specific y offset, and the copy stops before the
    /// shared action slot.
    ///
    /// `back_to` is `None` exactly when BACK does not go anywhere inside the app — see the module
    /// doc for which routes those are and how to census them.  Passing it here rather than letting
    /// screens draw their own line is what makes that a contract instead of a convention: a new
    /// route cannot forget to answer without deleting an argument.
    pub(crate) fn draw_narrative(
        self,
        p: Painter,
        back_to: Option<&str>,
        title: &str,
        copy: &str,
        copy_size: std::os::raw::c_int,
    ) {
        let top = self.narrative_top(back_to.is_some());
        if let Some(back_to) = back_to {
            self.draw_crumb(p, self.narrative.y, back_to);
        }

        let title = TextView::new(title, theme::size::HERO, theme::TEXT_HEADING)
            .bold()
            .max_lines(2);
        let title_h = title.measure_h(self.narrative.w);
        title.draw(p, Rect::new(self.narrative.x, top, self.narrative.w, title_h));

        let copy_top = top + title_h + theme::space::MD;
        let copy_bottom = self.action.y - theme::space::XL;
        TextView::new(copy, copy_size, theme::TEXT_READING)
            .leading(copy_size as f32 + theme::space::XS)
            .max_lines(12)
            .draw(
                p,
                Rect::new(
                    self.narrative.x,
                    copy_top,
                    self.narrative.w,
                    (copy_bottom - copy_top).max(0.0),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::inside_safe;

    #[test]
    fn every_required_route_region_is_inside_the_safe_area() {
        let l = RouteLayout::screen();
        assert!(inside_safe(l.narrative));
        assert!(inside_safe(l.content));
        assert!(inside_safe(l.action));
    }

    #[test]
    fn columns_are_related_objects_not_overlapping_coordinates() {
        let l = RouteLayout::screen();
        assert!(l.narrative.x + l.narrative.w < l.content.x);
        assert_eq!(l.narrative.y, l.content.y);
        assert_eq!(l.narrative.y + l.narrative.h, l.content.y + l.content.h);
        assert_eq!(l.action.x, l.narrative.x);
        assert_eq!(l.action.y + l.action.h, SAFE.y + SAFE.h);
    }

    #[test]
    fn first_table_section_and_narrative_share_one_top_anchor() {
        let l = RouteLayout::screen();
        let table = l.sectioned_table();
        assert_eq!(
            table.y + crate::ui::table::FIRST_HEADER_CAP_OFFSET,
            l.narrative.y
        );
        assert_eq!(table.y + table.h, l.content.y + l.content.h);
    }

    #[test]
    fn simultaneous_actions_flow_in_one_shared_bottom_row() {
        let l = RouteLayout::screen();
        let (leading, trailing) = l.action_pair(280.0, 240.0);
        assert_eq!(leading.x, l.action.x);
        assert_eq!(leading.y, trailing.y);
        assert_eq!(leading.h, trailing.h);
        assert_eq!(
            trailing.x - (leading.x + leading.w),
            crate::ui::widgets::CONTROL_GAP,
            "two peer answers are one control GROUP, not two blocks on a spacing rung"
        );
        assert!(
            crate::ui::widgets::CONTROL_GAP < theme::space::MD,
            "and a control-group gap is tighter than any rung of the block ladder"
        );
        assert!(inside_safe(trailing));
    }

    /// **The crumb costs the TITLE its own height and nothing else pays.** The content column is
    /// anchored at `--route-top` whether or not the narrative carries a crumb — a crumb that
    /// pushed nothing would overprint the title, and one that moved the CONTENT would break the
    /// alignment contract `sectioned_table`'s lift exists to hold.
    #[test]
    fn a_crumb_costs_the_title_its_own_height_and_moves_nothing_else() {
        let l = RouteLayout::screen();
        assert_eq!(l.narrative_top(false), l.narrative.y, "no crumb, no cost");
        assert_eq!(
            l.narrative_top(true) - l.narrative_top(false),
            CRUMB_BAND + theme::space::SM,
            "a crumb pushes the title down by exactly its own band plus one rung"
        );
        // Its neighbours are anchored independently of it, so both columns still hang from the one
        // top guide however the narrative begins.
        assert_eq!(l.content.y, l.narrative.y);
        assert_eq!(
            l.sectioned_table().y + crate::ui::table::FIRST_HEADER_CAP_OFFSET,
            l.narrative.y
        );
        assert_eq!(l.action.y + l.action.h, SAFE.y + SAFE.h);
        // And a crumbed narrative still has room for a two-line title and copy above the band.
        assert!(l.narrative_top(true) + theme::size::HERO as f32 * 2.0 + theme::space::MD
            < l.action.y - theme::space::XL);
    }

    /// The crumb's word sits ON the mark's line: its cap band is centred where the chevron is,
    /// not pinned to the band's top. The cap height is a stand-in (a CAPTION cap band measures
    /// ~17 px on the shipped face; measuring it here would link SDL2_ttf into the host suite).
    #[test]
    fn the_crumbs_word_centres_its_cap_band_on_the_mark() {
        let l = RouteLayout::screen();
        let top = l.narrative.y;
        let cy = top + CRUMB_BAND * 0.5;
        let cap = 17.0;
        let ty = crumb_label_top(cy, cap);
        assert!((ty + cap * 0.5 - cy).abs() < 0.01, "cap centre {} vs mark centre {}", ty + cap * 0.5, cy);
        assert!(ty > top, "a CAPTION cap band is shorter than the mark's box, so it starts inside the band");
        assert!(ty + cap < top + CRUMB_BAND, "…and ends inside it");
    }

    /// The crumb's mark and its word are ONE object, so the gap between them is measured to the
    /// chevron's ink rather than to its box — the asset is a third bearing, and a box-to-text gap
    /// on a spacing rung reads as two separate things.
    #[test]
    fn the_crumb_measures_its_gap_to_the_marks_ink() {
        let (_, ink_r) = crate::ui::icons::ink_x(Icon::ChevronLeft);
        assert!(ink_r < 1.0, "the chevron carries a right bearing to absorb");
        let l = RouteLayout::screen();
        let label_x = l.narrative.x + CRUMB_MARK * ink_r + theme::space::XS;
        assert!(
            label_x < l.narrative.x + CRUMB_MARK + theme::space::XS,
            "measuring to ink puts the word CLOSER than a box-to-text gap would"
        );
        assert!(label_x > l.narrative.x + CRUMB_MARK * 0.5, "…but not over it");
    }

    /// **The whole reason `ActionRow` exists**: a control holding row focus pops, an unfocused
    /// sibling in the same row does not, and both reach a settled rest — the same shape
    /// `CtlPop`'s own doc promises, pinned again here because this is the type every action-row
    /// caller is meant to reach for instead of a private field.
    #[test]
    fn action_row_pops_the_focused_control_and_leaves_its_sibling_at_rest() {
        // `CtlPop::scale` reads the crate-global `press` machine for whichever index is focused —
        // see `[[test-suite-global-pollution]]` — so this holds the same lock every `press.rs`
        // test does for its own body.
        let _g = crate::testlock::serial();
        let mut row: ActionRow<2> = ActionRow::new();
        for _ in 0..300 {
            row.step(Some(0), 1.0 / 60.0);
        }
        assert!(
            row.scale(0) > 1.0,
            "the focused control must be visibly popped, got {}",
            row.scale(0)
        );
        assert!(
            (row.scale(1) - 1.0).abs() < 0.001,
            "the unfocused sibling must not move, got {}",
            row.scale(1)
        );
        for _ in 0..300 {
            row.step(None, 1.0 / 60.0);
        }
        assert!(
            (row.scale(0) - 1.0).abs() < 0.001,
            "focus leaving the row must settle every control back to rest"
        );
    }

    // ---- the shared focus model ---------------------------------------------------------------
    //
    // Eleven rules, graded here as pure state transitions with no screen, no font and no GL. The
    // per-screen tests (`consent`, `onboard`, `legal`) grade that each screen ROUTES its keys
    // through these; this block grades that the rules themselves say what the module doc says.

    /// A table with a `band`-control band, holding nothing uncommitted.
    fn table_shape(band: usize, at_last_row: bool) -> RouteShape {
        RouteShape {
            band,
            rows: true,
            at_last_row,
            opens: false,
            uncommitted: false,
        }
    }

    /// **Rules 2 and 3: the band and the last row are one vertical walk, and it round-trips.**
    #[test]
    fn down_off_the_last_row_enters_the_band_and_up_comes_straight_back() {
        let mut f = RouteFocus::content();
        assert_eq!(
            f.updown(table_shape(1, false), 1),
            RouteStep::Scroll(1),
            "a middle row just moves the selection"
        );
        assert!(f.on_content());
        assert_eq!(f.updown(table_shape(1, true), 1), RouteStep::Moved);
        assert_eq!(f.stop(), Stop::Band(0), "DOWN off the last row enters the band");
        assert_eq!(f.updown(table_shape(1, true), -1), RouteStep::Moved);
        assert_eq!(f.stop(), Stop::Content, "…and UP comes straight back");
    }

    /// **Rule 4: the band is the floor**, and **rule 2 does not fire without a band** — a screen
    /// with none must keep handing DOWN to its list rather than inventing a stop.
    #[test]
    fn the_band_is_the_floor_and_a_bandless_screen_has_no_band_to_fall_into() {
        let mut f = RouteFocus::band();
        assert_eq!(f.updown(table_shape(1, true), 1), RouteStep::Wall);
        assert_eq!(f.stop(), Stop::Band(0));
        let mut g = RouteFocus::content();
        assert_eq!(g.updown(table_shape(0, true), 1), RouteStep::Scroll(1));
        assert!(g.on_content());
    }

    /// **Rules 5, 6 and 7: LEFT and RIGHT are one horizontal walk across the whole screen.** Two
    /// peer answers plus a list is three stops, and every step is reversible — which is exactly
    /// what first-run consent lacked (RIGHT off the trailing answer went nowhere) and what
    /// Privacy & data lacked in both directions.
    #[test]
    fn left_and_right_walk_the_band_and_the_list_as_one_row_of_stops() {
        // Two peer answers over a screen that is holding an edit, so the leading control is a wall
        // (rule 9's guard) and the walk's own ends are visible. The guard itself is graded by
        // `left_leaves_unless_the_screen_is_holding_an_edit_that_back_would_discard`.
        let s = RouteShape {
            uncommitted: true,
            ..table_shape(2, false)
        };
        let mut f = RouteFocus::content();
        assert_eq!(f.left(s), RouteStep::Moved);
        assert_eq!(f.stop(), Stop::Band(0), "LEFT from the list enters the band");
        assert_eq!(f.left(s), RouteStep::Wall, "…and stops at its leading control");
        assert_eq!(f.right(s), RouteStep::Moved);
        assert_eq!(f.stop(), Stop::Band(1));
        assert_eq!(f.right(s), RouteStep::Moved);
        assert_eq!(f.stop(), Stop::Content, "RIGHT off the trailing control returns to the list");
        // …and coming back in lands on the control it left, not on the band's first.
        assert_eq!(f.left(s), RouteStep::Moved);
        assert_eq!(f.stop(), Stop::Band(1));
    }

    /// **Rule 9, and its one guard.** LEFT off the left-most stop leaves the screen — unless the
    /// screen says it is holding an uncommitted edit, which BACK would discard.
    ///
    /// The guard is `uncommitted`, NOT "the screen has a band", and this test exists because the
    /// rule's first justification made exactly that inference and it is false in both directions:
    /// the Home editor's `Try again` is a band with nothing behind it, and first run's two answers
    /// record nothing until OK. So a band is not a wall on its own — a DRAFT is.
    #[test]
    fn left_leaves_unless_the_screen_is_holding_an_edit_that_back_would_discard() {
        let mut bandless = RouteFocus::content();
        assert_eq!(bandless.left(table_shape(0, false)), RouteStep::Back);
        assert_eq!(
            bandless.left(RouteShape::document()),
            RouteStep::Back,
            "a document is a read: nothing to its left and nothing to lose"
        );

        // A band with nothing behind it is NOT a wall — this is the Home editor's `Try again`, and
        // first run's two answers, both of which record nothing until OK.
        let mut clean_band = RouteFocus::band();
        assert_eq!(clean_band.left(table_shape(1, false)), RouteStep::Back);

        // A draft IS. Overshooting Done by one press must not throw an edit away.
        let dirty = RouteShape {
            uncommitted: true,
            ..table_shape(1, false)
        };
        let mut on_done = RouteFocus::band();
        assert_eq!(on_done.left(dirty), RouteStep::Wall);
        assert_eq!(on_done.stop(), Stop::Band(0), "…and focus does not wander either");
        // …and the guard reaches the bandless case too: a screen holding an edit with no band to
        // step into still may not be left by a directional key.
        let mut on_list = RouteFocus::content();
        assert_eq!(
            on_list.left(RouteShape {
                uncommitted: true,
                ..table_shape(0, false)
            }),
            RouteStep::Wall
        );
    }

    /// **Rule 11's timing guard.** A key acts on the LOGICAL state; a positional hit cannot, so
    /// every pointer path in the family refuses until its layer has arrived at an endpoint.
    #[test]
    fn a_push_is_settled_only_at_the_endpoint_its_state_names() {
        let mut push = RoutePush::new();
        assert!(push.settled(false), "at rest, closed");
        assert!(!push.settled(true));
        push.update(true, 1.0 / 60.0);
        assert!(
            !push.settled(true) && !push.settled(false),
            "mid-flight is settled for NEITHER state — which is the whole point"
        );
        for _ in 0..600 {
            push.update(true, 1.0 / 60.0);
        }
        assert!(push.settled(true) && !push.settled(false));
    }

    /// **Rule 8: RIGHT enters a chevron row and nothing else.** A toggle row changes a value in
    /// place, so there is nothing to its right to go to.
    #[test]
    fn right_enters_only_a_row_that_opens_something() {
        let mut f = RouteFocus::content();
        let opens = RouteShape {
            opens: true,
            ..table_shape(0, false)
        };
        assert_eq!(f.right(opens), RouteStep::Enter);
        assert!(f.on_content(), "entering is the SCREEN's job; focus stays put");
        assert_eq!(f.right(table_shape(0, false)), RouteStep::Wall);
    }

    /// **Rule 1: a document scrolls.** It has no rows, so no vertical press may be interpreted as
    /// falling off the end of a list.
    #[test]
    fn a_document_scrolls_on_up_and_down_however_far_it_is_driven() {
        let mut f = RouteFocus::content();
        for _ in 0..5 {
            assert_eq!(f.updown(RouteShape::document(), 1), RouteStep::Scroll(1));
        }
        assert_eq!(f.updown(RouteShape::document(), -1), RouteStep::Scroll(-1));
        assert!(f.on_content());
    }

    /// **Rule 10: focus may never rest on nothing** — the reported "toggling the value back can
    /// cause focus to disappear completely", as a pure transition. A band that goes away hands
    /// focus back to the list; a list with no rows yet hands it to the band; and a cursor left
    /// past the end of a shrunken band is pulled back inside it.
    #[test]
    fn a_shape_that_removes_the_focused_stop_re_seats_focus_rather_than_losing_it() {
        let mut on_done = RouteFocus::band();
        assert!(
            on_done.settle(table_shape(0, false)),
            "removing the band has to move focus"
        );
        assert_eq!(on_done.stop(), Stop::Content);

        let mut on_empty_list = RouteFocus::content();
        assert!(on_empty_list.settle(RouteShape {
            band: 1,
            rows: false,
            at_last_row: false,
            opens: false,
            uncommitted: false,
        }));
        assert_eq!(
            on_empty_list.stop(),
            Stop::Band(0),
            "a list that has not landed yet cannot hold the ring"
        );

        let mut past_the_end = RouteFocus::band();
        past_the_end.to_band(1);
        assert!(past_the_end.settle(table_shape(1, false)));
        assert_eq!(past_the_end.stop(), Stop::Band(0));

        let mut settled = RouteFocus::content();
        assert!(
            !settled.settle(table_shape(1, false)),
            "a shape that still holds the focused stop reports no move"
        );
    }

    /// Every rule re-settles before it runs, so a key that arrives on the frame a rebuild removed
    /// the focused stop cannot act on a stop that is no longer there.
    #[test]
    fn every_rule_settles_before_it_decides() {
        for step in [
            RouteFocus::band().updown(table_shape(0, true), -1),
            RouteFocus::band().left(table_shape(0, false)),
            RouteFocus::band().right(table_shape(0, false)),
        ] {
            assert!(
                !matches!(step, RouteStep::Moved),
                "a band that is gone must not answer as though focus were still on it: {step:?}"
            );
        }
        // …and the LEFT case is the one that matters most: settled onto the list of a bandless
        // screen, LEFT is the way out of it rather than a step to a control that is not drawn.
        assert_eq!(
            RouteFocus::band().left(table_shape(0, false)),
            RouteStep::Back
        );
    }

    /// **Rule 11's geometry half**: only a control DRAWN this frame is hit-testable, and dead
    /// space parks nothing. `clear` is what retires a Done that an undo has just removed — without
    /// it the band leaves a live click target where it used to be.
    #[test]
    fn only_a_placed_action_control_can_be_hit_and_dead_space_hits_nothing() {
        let mut row: ActionRow<2> = ActionRow::new();
        assert_eq!(row.hit(0.0, 0.0), None, "an unplaced row has no target anywhere");
        row.place(0, Rect::new(100.0, 900.0, 200.0, 60.0));
        row.place(1, Rect::new(320.0, 900.0, 180.0, 60.0));
        assert_eq!(row.hit(150.0, 930.0), Some(0));
        assert_eq!(row.hit(400.0, 930.0), Some(1));
        assert_eq!(row.hit(310.0, 930.0), None, "the gap between them is dead space");
        assert_eq!(row.hit(150.0, 400.0), None, "and so is the rest of the screen");
        row.clear();
        assert_eq!(row.hit(150.0, 930.0), None, "a band that is not drawn is not clickable");
    }

    /// **Issue 2's geometry rule.** A text-only content column hangs from the TITLE's anchor, not
    /// from the crumb's — so its first line is level with the title beside it instead of sitting up
    /// on the return chevron's line. Written against the two anchors rather than against a number,
    /// because the number is `narrative_top`'s and must not be transcribed twice.
    #[test]
    fn a_text_only_column_starts_on_the_title_line_not_on_the_crumbs() {
        let l = RouteLayout::screen();
        for crumbed in [true, false] {
            let doc = l.document(crumbed);
            assert_eq!(
                doc.y,
                l.narrative_top(crumbed),
                "a document's first line shares the narrative title's cap-top anchor"
            );
            assert_eq!(doc.x, l.content.x, "…in the content column, unmoved sideways");
            assert_eq!(doc.w, l.content.w);
            assert_eq!(
                doc.y + doc.h,
                l.content.y + l.content.h,
                "…and it still ends where every other content column does"
            );
            assert!(inside_safe(doc));
        }
        assert!(
            l.document(true).y > l.sectioned_table().y,
            "a table is LIFTED so its first section label lands on the guide; a document, which \
             has no such label, must not be"
        );
        assert!(
            l.document(true).y > l.narrative.y,
            "…and specifically it must not start on the crumb's own line"
        );
    }

    #[test]
    fn nested_route_uses_one_reversible_spring() {
        let mut push = RoutePush::new();
        push.update(true, 1.0 / 60.0);
        assert!(push.amount() > 0.0 && push.amount() < 1.0);
        for _ in 0..600 {
            push.update(true, 1.0 / 60.0);
        }
        assert!((push.amount() - 1.0).abs() < 0.001);
        for _ in 0..600 {
            push.update(false, 1.0 / 60.0);
        }
        assert!(push.amount() < 0.001);
    }

    #[test]
    fn route_ground_latches_once_instead_of_following_child_screens() {
        let mut ground = RouteGround::new();
        let first = [[0.1, 0.2, 0.3]; 4];
        let second = [[0.8, 0.7, 0.6]; 4];
        ground.latch(first, mean_key(first));
        let palette = ground.palette();
        ground.latch(second, mean_key(second));
        assert_eq!(ground.palette(), palette);
        assert!(ground.is_latched());
    }

    #[test]
    fn route_ground_key_is_the_whole_envelope_not_one_loud_corner() {
        assert_eq!(
            mean_key([
                [0.0, 0.2, 0.4],
                [0.2, 0.4, 0.6],
                [0.4, 0.6, 0.8],
                [0.6, 0.8, 1.0],
            ]),
            [0.3, 0.5, 0.7]
        );
    }

    #[test]
    fn pre_home_fallback_is_an_envelope_not_a_flat_grey() {
        let c = theme::ROUTE_GROUND_FALLBACK;
        assert!(c.windows(2).any(|pair| pair[0] != pair[1]));
        assert!(c.iter().any(|corner| {
            (corner[0] - corner[1]).abs() > 0.001 || (corner[1] - corner[2]).abs() > 0.001
        }));
    }

    /// WCAG relative luminance and contrast, duplicated in miniature from `widgets.rs`'s private
    /// `rel_luma`/`contrast` — this module cannot see those (`widgets`' are not `pub(crate)`) and a
    /// third home for the two formulas is worse than one small, obviously-correct copy that a host
    /// test can run with no font or GL loaded.
    fn linearize(c: f32) -> f32 {
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    fn rel_luma(c: [f32; 4]) -> f32 {
        0.2126 * linearize(c[0]) + 0.7152 * linearize(c[1]) + 0.0722 * linearize(c[2])
    }
    fn contrast(a: [f32; 4], b: [f32; 4]) -> f32 {
        let (x, y) = (rel_luma(a), rel_luma(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// **The regression this fallback shipped with**: a device capture of first-run consent read as
    /// "no ambient light" because three of the four authored stops sat within ~15 8-bit codes of
    /// `theme::SURFACE_APP` in both hue and luminance — a wash with almost no wash in it. A real
    /// keyed hero ground gets its contrast from an actual photograph; this one has to author its
    /// own, so it is pinned directly: the brightest and darkest corners must differ by a real
    /// multiple (not `AmbientWash`'s own ~1.8x floor, which a near-uniform fallback could still
    /// clear), and every corner must still hold the same legibility floors
    /// `a_ground_never_outshines_the_fine_print_that_sits_on_it` holds a real keyed hero to, since
    /// the crumb caption and the copy paragraph both read in `TEXT_TERTIARY`/`TEXT_READING` over
    /// this exact ground with no further dimming.
    #[test]
    fn the_pre_home_fallback_reads_as_a_directional_wash() {
        let c = theme::ROUTE_GROUND_FALLBACK;
        let luma: Vec<f32> = c.iter().map(|corner| rel_luma(*corner)).collect();
        let (lo, hi) = (
            luma.iter().cloned().fold(f32::INFINITY, f32::min),
            luma.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        );
        assert!(
            hi / lo.max(1e-6) >= 2.5,
            "corners span only {lo:.4}..{hi:.4} — too close in luminance to read as directional \
             light on a television"
        );
        for (i, corner) in c.iter().enumerate() {
            let t = contrast(theme::TEXT_TERTIARY, *corner);
            assert!(
                t >= 3.0,
                "corner {i}: TEXT_TERTIARY at {t:.2}:1, under the 3:1 floor the crumb caption reads at"
            );
            let p = contrast(theme::TEXT_PRIMARY, *corner);
            assert!(p >= 7.0, "corner {i}: TEXT_PRIMARY at {p:.2}:1");
        }
    }

    /// The fallback must not merely be brighter — it must stay materially distinct from a flat
    /// `SURFACE_APP` fill, which is the specific failure mode the correction fixed (three of four
    /// stops used to sit within ~15 8-bit codes of it in both hue AND luminance). Measured as raw
    /// mean per-channel distance (display-encoded, not WCAG-linearized) rather than luminance
    /// alone: WCAG luminance compresses the shadow end sharply, so a corner that is UNMISTAKABLY
    /// darker at the near-black end (this fallback's own `ATMOS_CHARCOAL`) can read as luminance-
    /// close to the surface while an 8-bit-code comparison — closer to how banding-free darks
    /// actually read on a panel — shows it plainly is not.
    #[test]
    fn every_fallback_corner_is_visibly_apart_from_the_app_surface() {
        for (i, corner) in theme::ROUTE_GROUND_FALLBACK.iter().enumerate() {
            let d: f32 = (0..3)
                .map(|ch| (corner[ch] - theme::SURFACE_APP[ch]).abs())
                .sum::<f32>()
                / 3.0;
            assert!(
                d > 0.06,
                "corner {i} sits only {d:.4} from SURFACE_APP's own raw channel values — that \
                 reads as the app's flat ground, not atmosphere"
            );
        }
    }

    #[test]
    fn default_route_ground_latches_the_auth_fallback_once() {
        let mut ground = RouteGround::new();
        ground.latch_target(theme::ROUTE_GROUND_FALLBACK);
        let palette = ground.palette();
        ground.latch_target([[1.0, 0.0, 0.0, 1.0]; 4]);
        assert_eq!(ground.palette(), palette);
        assert!(ground.is_latched());
    }
}
