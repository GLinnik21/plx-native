//! **Also available** — the same film held by more than one source, as a list you can walk into.
//!
//! Deliverable E of the Shared Sources design (`docs/shared-servers.md` §6): when a second pinned
//! source also holds the item on screen, the detail page's actions row grows an *Also available*
//! pill with a trailing chevron, and it opens this panel — a `TableView` on a glass ground, the
//! same object the Library's Sort and Filter chips open one page over.
//!
//! **A `Style::Compact` surface on the container tree** since restructure phase 10 (§6.2): the
//! anchored menu the Library's chips already are. The container owns its PHASE and its appear
//! spring; the surface owns input while it is `Opening | Open`; and its ANCHOR — the drawn rect of
//! the pill that opened it — rides on its [`AltSourcesArg`] exactly as `LibraryMenuArg`'s does,
//! which is what retired a `static mut ANCHOR: PanelAnchor` holding a `Rect` (a geometry static is
//! never an allowlistable render cache, §15.2).
//!
//! Its rows use all FOUR of that cell's content places — the leading mark, the label over its
//! sub-line, the trailing read-out, and the right-aligned badge:
//!
//! | mark | label | sub-line | read-out | badge |
//! |---|---|---|---|---|
//! | ✓ | `Movies`    | `This account` | `1 hr 57 min` | `1080p` |
//! |   | `Film Club` | `friend`       | `1 hr 57 min` | `4K`    |
//!
//! # The four calls this module implements, and why each is not the obvious thing
//!
//! **A CONTROL in the actions row, not a section of the page.** An item on several servers is a
//! rare fact; a permanent band would spend a whole row on nothing on every other page in the
//! library. So the list is behind a button, and the button itself is drawn ONLY when a second
//! pinned source holds this item — the same "with one source, none of this is drawn" rule the
//! shelf heading's annotation implements as absence rather than as a branch that draws nothing
//! visible. **That gate is `metadata::alt_available`, asked by the PAGE about its own item**, and
//! it is deliberately not a question about this panel: a screen must never have to mount a surface
//! to find out how many columns its own footer has.
//!
//! **The tick marks the copy you are on now**, and the badge is the RESOLUTION class — the
//! vocabulary the hero's badges row already speaks (`ui::fmt::resolution`, one spelling of "4K" in
//! the product). A row is `[library] [owner · runtime] [class]`: the label answers *which library*,
//! the sub-line *whose*, the badge *what you would get*.
//!
//! **OK NAVIGATES.** It opens that server's own page for the film; it does not swap the copy under
//! you. That is also what settles the per-server progress problem: **a RESUME POSITION does not
//! travel between servers** — it is about a file you are streaming from one host — so a design that
//! switched the copy in place would have had to explain a resume position that jumped. Navigating
//! makes it self-evident: you are on a different page, and it says where you are in *that* copy.
//! Like every other menu here this surface only REPORTS the choice; it delivers
//! [`AppMsg::AltSourceOpen`] to the Detail instance named by its argument and the PAGE performs the
//! navigation, which is `LibraryMenu`'s shape and keeps one owner of "what a press means here".
//!
//! This paragraph said "watch state does not travel between servers" flat, and half of that stopped
//! being true when [`crate::viewstate`] began fanning a Mark as Watched out over every source
//! holding the guid. The two halves are not the same claim and the split is the whole point: the
//! **watched flag follows the TITLE** (having seen a film is not a fact about a host), while the
//! **offset stays with the copy**. Nothing here changed — this panel still navigates rather than
//! swaps, for the reason above, which is about the offset.
//!
//! **Ordering is the copy that PLAYS first**, then what a viewer would prefer: higher quality
//! first, and at equal quality yours before a friend's, because yours cannot go offline mid-film.
//! The design's own example pins the first clause against the second — a 1080p copy you are
//! standing on sorts above a friend's 4K one.
//!
//! Explicitly rejected, and not to be re-introduced: merging the copies invisibly and playing the
//! best one (it is what makes a film disappear when a friend goes offline, with no way to see why);
//! a permanent section on the page; switching the copy silently on OK.
//!
//! # Where the copies come from
//!
//! **The identity is the `guid` (`plex://movie/…`), never the `ratingKey`.** Every item-shaped
//! integer in Plex is server-local and starts at 1, so both servers in this household have a
//! `ratingKey` 4 and a section 1 (`docs/shared-servers.md` §1). Matching on one would confidently
//! offer a different film — which is why the STORE is ADDRESSED on the `(server, ratingKey)` PAIR:
//! a resolve outliving the page that asked for it is the normal case, not the exotic one. The
//! store, the cross-source resolve that fills it and the headless `/tmp/plxnative-shared` stand-in
//! all live in `crate::metadata`, beside the detail fetch whose landing kicks them; this module
//! ORDERS, MARKS and DRAWS and owns nothing else.
//!
//! **Rows are a SNAPSHOT, refreshed against a stamp.** `TableView` is built once and drawn from,
//! so a correction reaching the store — a re-described source's credit, a revoked share — has to
//! be carried the last step by rebuilding. The stamp is the row list itself, so a rebuild happens
//! exactly when the drawn content would differ, and the ORDER moves with the text (the credit is
//! the own-before-a-friend's tiebreak, so a corrected panel can legitimately swap two of its rows).
//! The cursor is KEPT across a rebuild rather than re-seated on the tick: the rows can re-order
//! beneath it, which is a smaller surprise than the cursor jumping mid-interaction.

use std::borrow::Cow;

use crate::metadata::AltCopy;
use crate::plex::ServerId;
use crate::screens::registry::{AppLike, AppMsg, ContentArg, PageMemory};
use crate::ui::consts::{SCR_H, SCR_W};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind,
    InstanceId, Key, LogicalState, Machine, MachineId, NavOp,
};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind,
    GroupSpec, Hover, HitSource, Placed, RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step,
    Stop,
};
use crate::ui::table::{Badge, Row, Section, TableView};
use crate::ui::widgets::{Glass, GlassState};
use crate::ui::{theme, Rect};

/// The fields [`AltSourcesScreen::write`] canonicalises, for the recorder's shape pin (§5.4). The
/// selected ROW is in it deliberately: this panel's UP/DOWN changes nothing else in the app, so
/// without it a replay grades the panel opening and closing and nothing between.
pub(crate) const SHAPE: [&str; 2] = [
    "AltSourcesScreen{arg:AltSourcesArg{host:u32,sid:u32,rk:str,anchor:[u32;4]},rows:[{sid:u32,rk:str,label:str,detail:str,value:opt<str>,badge:opt<str>,checked:bool}],sel:i32,table:TableViewMotion}",
    TableView::MOTION_SHAPE,
];

/// What the container is asked to present.
///
/// `host` is the Detail instance the choice is reported back to; it rides on the argument rather
/// than being looked up, for `LibraryMenuArg`'s reason — the entry outlives any one frame's idea of
/// which page is on top. `sid`/`rk` are the copy the page is STANDING ON: the tick's other half,
/// and the pair the addressed store is read with.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct AltSourcesArg {
    pub(crate) host: InstanceId,
    pub(crate) sid: ServerId,
    pub(crate) rk: String,
    /// Bit-preserving rest rectangle of the pill that opened it; valid in canonical arguments
    /// without float equality.
    pub(crate) anchor: [u32; 4],
}

impl LogicalState for AltSourcesArg {
    fn write(&self, c: &mut Canon) {
        c.u32(self.host.0).u32(u32::from(self.sid.raw())).str(&self.rk);
        for value in self.anchor {
            c.u32(value);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str("alt_sources_arg");
    }
}

// ---- geometry --------------------------------------------------------------------------------

/// The panel's width — the design's `altPanelW`. Wide because the row fills all four of the cell's
/// content places (library, owner, runtime, class) and the two on the right are fixed runs: the
/// label block gets whatever is left, and it is the library name that must not elide.
const PANEL_W: f32 = 700.0;
/// The pinned ~20px corner radius, the item menu's.
const PANEL_RAD: f32 = 20.0;
/// Air between the button that opened the panel and the panel itself — one `space` rung.
const BTN_GAP: f32 = theme::space::MD;
/// Keep-out from the screen edges, so a panel opened low on the page is still whole.
///
/// Per AXIS, for `item_menu::EDGE`'s reason: `space::XL` 64 clears `MARGIN_Y` vertically and misses
/// `MARGIN_X` horizontally by 32px.
const EDGE: f32 = theme::space::XL;
const EDGE_X: f32 = crate::ui::consts::MARGIN_X;
/// Scrim peak alpha — the Library toolbar chip menu's design, deliberately, because this
/// is the same object one page over: a chip-shaped control on a live page opening a list over it.
/// The page recedes; it is not blanked, and the hero behind stays readable.
const SCRIM_A: f32 = 0.45;
/// How far the panel rises into place, matching those same chip menus. The container's appear
/// spring drives it now (`DrawFrame::page_alpha` IS `Surface::motion.appear`), so the translate is
/// applied here rather than by `Popover::painter`; at rest it contributes nothing, which is what
/// makes a settled capture of this panel byte-identical across the conversion.
const RISE: f32 = crate::ui::popover::Popover::RISE;

/// Under the button when there is room, above it when there is not — never over it, so the control
/// that opened the panel stays readable beside its own list. Horizontally it hangs off the
/// button's left edge (the list and the label it came from share a margin), pulled inside the
/// screen's keep-out. Pure (anchor + measured height in, rect out), so the placement rules are
/// host-testable without mounting anything.
pub(crate) fn panel_at(a: Rect, content_h: f32) -> Rect {
    let h = content_h.clamp(120.0, SCR_H - 2.0 * EDGE);
    let below = a.y + a.h + BTN_GAP;
    let y = if below + h <= SCR_H - EDGE {
        below
    } else {
        (a.y - BTN_GAP - h).max(EDGE)
    };
    let x = a.x.clamp(EDGE_X, (SCR_W - EDGE_X - PANEL_W).max(EDGE_X));
    Rect::new(x, y, PANEL_W, h)
}

// ---- the model (pure) ------------------------------------------------------------------------

/// A copy's quality, as SCAN LINES — the ordering's second key.
///
/// It reads the same three fields `ui::fmt::resolution` badges the row with, in the same
/// precedence (the server's own class first, the stored frame size only as its fallback), so the
/// ladder the eye sees on the badges and the order the rows are in cannot disagree. Saturating,
/// because these are wire values through the lenient `de_i64` and a garbage 19-digit height must
/// not overflow into a top-of-list sort key.
fn scan_lines(c: &AltCopy) -> i64 {
    let r = c.res.trim().to_ascii_lowercase();
    match r.as_str() {
        "" => {}
        "8k" => return 4320,
        "4k" => return 2160,
        "sd" => return 480,
        _ if r.chars().all(|ch| ch.is_ascii_digit()) => return r.parse::<i64>().unwrap_or(0),
        _ => {}
    }
    if c.height > 0 {
        c.height
    } else {
        c.width.saturating_mul(9) / 16
    }
}

/// One drawn row, resolved: what it says and where OK goes. The panel builds its `TableView` from
/// these and nothing else, so the ordering, the tick and the destination map are ONE list.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AltRow {
    /// the library, on that source
    pub(crate) label: String,
    /// whose copy it is: `"This account"` / `"friend"`
    pub(crate) detail: String,
    /// the runtime, for the row's trailing read-out — `None` for a copy the server sent no
    /// duration for, which then says nothing rather than "0 min"
    pub(crate) value: Option<String>,
    /// the resolution class, or `None` for a copy the server described no video for
    pub(crate) badge: Option<String>,
    /// the copy the page is on NOW — the picker's tick
    pub(crate) checked: bool,
    pub(crate) sid: ServerId,
    pub(crate) rk: String,
}

/// What the sub-line calls a source with no owner: the signed-in account's own server. A PERSON in
/// every case, which is the design's rule for every browsing surface — the machine name never
/// appears outside the Sources list.
const OWN_ACCOUNT: &str = "This account";

/// Build the panel's rows from `list`, given the copy the page is standing on (`here_sid` +
/// `here_rk`). PURE — every ordering and marking decision in this module is here, and the host
/// suite drives it directly.
///
/// **Exactly one row is ticked**, by construction: the mark is placed on the FIRST copy matching
/// the page's own (server, ratingKey), so a producer that listed the same copy twice still yields
/// one "you are here" rather than two. A list with no such copy is ticked nowhere at all — which
/// is the honest answer while a landing is being reconciled, and never two.
///
/// The order is the design's: the copy that plays first, then higher quality, then yours before a
/// friend's, then by library name so the list cannot reshuffle between two frames that resolved
/// the same facts.
pub(crate) fn rows(list: &[AltCopy], here_sid: ServerId, here_rk: &str) -> Vec<AltRow> {
    // `same_item`, not a hand-rolled `&&`: it is the one place the pair rule lives, and its own doc
    // promises every stored-item lookup routes through it. These two sites were the last that did
    // not — and they are the ones that decide which row wears the tick and which press does nothing.
    let here = list
        .iter()
        .position(|c| crate::plex::same_item((c.sid, &c.rk), (here_sid, here_rk)));
    let mut idx: Vec<usize> = (0..list.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ca, cb) = (&list[a], &list[b]);
        // `false` sorts before `true`, so each key is written as "the one that loses"
        (
            here != Some(a),
            std::cmp::Reverse(scan_lines(ca)),
            ca.owner.is_some(),
            &ca.library,
        )
            .cmp(&(
                here != Some(b),
                std::cmp::Reverse(scan_lines(cb)),
                cb.owner.is_some(),
                &cb.library,
            ))
    });
    idx.into_iter()
        .map(|i| {
            let c = &list[i];
            let who = c.owner.as_deref().unwrap_or(OWN_ACCOUNT);
            AltRow {
                label: c.library.clone(),
                detail: who.to_string(),
                // a copy the server sent no duration for leaves the read-out slot EMPTY rather
                // than claiming "0 min" — the same rule the sub-line's dangling separator followed
                // while the runtime was part of it
                value: (c.dur_ms > 0).then(|| crate::ui::fmt::dur_long(c.dur_ms)),
                badge: crate::ui::fmt::resolution(&c.res, c.width, c.height),
                checked: here == Some(i),
                sid: c.sid,
                rk: c.rk.clone(),
            }
        })
        .collect()
}

/// What OK on the highlighted row does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    None,
    /// Open `rk` on server `sid` — that copy's own page. Never a swap of the copy in place.
    Open { sid: ServerId, rk: String },
}

/// The index→destination mapping, pure so the "the row you are on is not a destination" rule is
/// host-testable. A selection outside the list is [`Action::None`] rather than whatever happens to
/// sit at that index in some other row set (`sel` survives a rebuild).
pub(crate) fn action_at(rows: &[AltRow], sel: i32, here_sid: ServerId, here_rk: &str) -> Action {
    let Some(row) = usize::try_from(sel).ok().and_then(|i| rows.get(i)) else {
        return Action::None;
    };
    if crate::plex::same_item((row.sid, &row.rk), (here_sid, here_rk)) || row.rk.is_empty() {
        return Action::None;
    }
    Action::Open {
        sid: row.sid,
        rk: row.rk.clone(),
    }
}

// ---- the surface -----------------------------------------------------------------------------

/// Where the selection lands when the table is rebuilt.
enum Sel {
    /// Opening: on the copy you are on — the row the tick is against — because that is where the
    /// eye already is, and one DOWN from it is the alternative.
    OnTheCopyYouAreOn,
    /// Rebuilding under an OPEN panel: leave the cursor where the user put it.
    Keep,
}

pub(crate) struct AltSourcesScreen {
    entry: EntryId,
    arg: AltSourcesArg,
    /// The rows the `TableView` and `dests` were built from — the rebuild stamp (module doc).
    rows: Vec<AltRow>,
    pub(crate) table: TableView,
    glass: GlassState,
}

impl AltSourcesScreen {
    pub(crate) fn new(entry: EntryId, arg: AltSourcesArg) -> Self {
        let mut screen = Self {
            entry,
            arg,
            rows: Vec::new(),
            table: TableView::new(),
            glass: GlassState::new(),
        };
        screen.rebuild(Sel::OnTheCopyYouAreOn);
        screen
    }

    /// The rows the store would produce right now, marked against **the server the open PAGE
    /// belongs to** and the item it is showing.
    ///
    /// The page's server, not `plex::current_server()`. Those were the same thing only while
    /// browsing a shared library also re-pointed `current`; once that stopped, opening a borrowed
    /// film left `current` on our own server, no copy matched the pair, and the panel drew **no
    /// tick at all** — owner-reported. The tick answers "which of these am I looking at", and only
    /// the page knows; since phase 10 it says so on the argument.
    fn live_rows(&self) -> Vec<AltRow> {
        rows(
            crate::metadata::alt_copies(self.arg.sid, &self.arg.rk),
            self.arg.sid,
            &self.arg.rk,
        )
    }

    /// Materialise the store into the table, when and only when the drawn content would differ.
    fn refresh(&mut self) -> bool {
        let next = self.live_rows();
        if next == self.rows {
            return false;
        }
        self.rows = next;
        self.rebuild(Sel::Keep);
        true
    }

    fn rebuild(&mut self, sel_mode: Sel) {
        if matches!(sel_mode, Sel::OnTheCopyYouAreOn) {
            self.rows = self.live_rows();
        }
        let mut sec = Section::new("");
        let mut sel = match sel_mode {
            Sel::Keep => self.table.sel.max(0),
            Sel::OnTheCopyYouAreOn => 0,
        };
        for (i, r) in self.rows.iter().enumerate() {
            // all four of the cell's content places, which is what the row has to say: the tick is
            // WHICH copy you are on, the label WHICH LIBRARY, the sub-line WHOSE, the read-out HOW
            // LONG and the badge WHAT YOU GET. The runtime used to be a dotted second half of the
            // sub-line, which left the read-out slot empty and made the one fact that lines up down
            // the panel the one buried mid-string.
            let mut row = Row::new(r.label.clone())
                .detail(r.detail.clone())
                .checked(r.checked);
            if let Some(v) = &r.value {
                row = row.value(v.clone());
            }
            if let Some(b) = &r.badge {
                row = row.badge(Badge::Text(b.clone()));
            }
            sec = sec.row(row);
            if r.checked && matches!(sel_mode, Sel::OnTheCopyYouAreOn) {
                sel = i as i32;
            }
        }
        sel = sel.min((self.rows.len() as i32 - 1).max(0));
        // rows carry a sub-line — the track menu's size class, not the account popover's
        self.table.compact = false;
        self.table.set_sections(vec![sec], sel, false);
    }

    pub(crate) fn frame(&self) -> Rect {
        let [x, y, w, h] = self.arg.anchor.map(f32::from_bits);
        panel_at(Rect::new(x, y, w, h), self.table.measured_height())
    }

    fn dismiss<H: AppLike>(&self, fx: &mut Effects<'_, H>) {
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// Commit row `elem` and close. The row you are ALREADY on reports nothing — there is nowhere
    /// to navigate to, and the tick has already answered the question the press was asking.
    ///
    /// Takes the elem directly rather than reading `self.table.sel`: the engine's OK arm reads it
    /// off the current focus key and a pointer click's `Activate` names the STOP that was clicked,
    /// which need not be the row the cursor was already resting on — the same distinction
    /// `AccountMenuScreen::activate` draws.
    fn commit<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let action = action_at(&self.rows, elem as i32, self.arg.sid, &self.arg.rk);
        self.dismiss(fx);
        if let Action::Open { sid, rk } = action {
            // The PAGE navigates. This surface names the destination and nothing else — the
            // `LibraryMenu` shape, and what keeps "what a press means on the Detail page" in one
            // place instead of two.
            fx.push(Fx::Deliver(
                MachineId::Instance(self.arg.host),
                Delivery::Screen(ScreenEvent::App(AppMsg::AltSourceOpen(
                    ContentArg::Detail { sid, rk },
                ))),
            ));
        }
    }
}

impl<H: AppLike<Memory = PageMemory>> Machine<H> for AltSourcesScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount | ScreenEvent::Enter(_) => {
                self.refresh();
                Handled::Yes
            }
            ScreenEvent::StoreChanged(ord, _) => {
                if *ord == crate::stores::StoreId::Metadata.ord() && self.refresh() {
                    fx.invalidate(crate::ui::present::Provenance::Landing(fx.from()));
                }
                Handled::Yes
            }
            ScreenEvent::Tick(tick) => {
                // Re-derive the drawn selection from the engine's own tracked focus every tick,
                // exactly as `AccountMenuScreen::step` does — `FocusMoved` already keeps the two
                // in step, this is the belt to its suspenders for a focus change this screen was
                // not told about directly (a remembered-cursor seat on `Enter`, say).
                self.table.sel = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .map(|key| key.elem as i32)
                    .unwrap_or(self.table.sel);
                self.table.update(tick.dt(), self.frame().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                self.table.sel = to.elem as i32;
                fx.invalidate(crate::ui::present::Provenance::Input);
                Handled::Yes
            }
            // The engine's OK arm, for a `Bare` element, delivers `Activate` directly rather than
            // arming a hold — see `Focusable::groups` below. A pointer click on a registered row
            // stop reaches the same arm through the hit map's own `Activate::Immediate` policy.
            ScreenEvent::Activate(elem) => {
                self.commit(*elem, fx);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.commit(key.elem, fx);
                }
                Handled::Yes
            }
            // UP/DOWN, OK and the pointer are no longer read here at all — the engine's own
            // direction/OK/hit-map machinery drives them through `Focusable` and the arms above,
            // exactly as it does for every other Engine screen (`AccountMenuScreen` is the
            // worked example this module follows). A click BESIDE every registered row stop is a
            // HIT-MAP MISS, and `Style::Compact`'s `on_miss` (`OnMiss::Dismiss`) is what closes
            // the panel — the same "click outside a popover dismisses it" rule this arm used to
            // implement by hand against raw pointer coordinates.
            ScreenEvent::Input(input) => match input.kind {
                InputKind::Key {
                    key: Key::Back,
                    edge: Edge::Down,
                    ..
                } => {
                    self.dismiss(fx);
                    Handled::Yes
                }
                _ => Handled::No,
            },
            _ => Handled::No,
        }
    }
}

/// **A single-column `Focusable` over the table's own cursor** (phase 12, D2) — the same shape
/// `AccountMenuScreen::groups` uses for its own `TableView`: one `GroupKind::Column` of
/// `self.rows.len()` `Bare` elements (OK/click activates on the down edge, never arms a hold —
/// there is nothing here to hold), `EdgeRule::Stop` on every side since this is a standalone
/// surface with no page to escape onto, and `place`/`neighbour` read straight off `TableView`'s
/// own row geometry (`row_frame`, `next_selectable`) rather than a second copy of it.
impl<H: AppLike<Memory = PageMemory>> Focusable<H> for AltSourcesScreen {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(),
            len: self.rows.len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        ((*elem as usize) < self.rows.len()).then_some(GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        let next = match dir {
            Dir::Up => self.table.next_selectable(key.elem as i32, -1),
            Dir::Down => self.table.next_selectable(key.elem as i32, 1),
            _ => None,
        };
        match next {
            Some(i) => Step::Move(FocusKey {
                entry: self.entry,
                elem: i as u32,
            }),
            None => Step::Edge,
        }
    }
    fn place(&self, elem: &u32, _cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let rect = self.table.row_frame(self.frame(), *elem as i32)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: self.frame(),
            index: Some(*elem),
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            FocusKey {
                entry: self.entry,
                elem: 0,
            }
        }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self.table.sel.max(0) as u32,
        }
    }
}

impl LogicalState for AltSourcesScreen {
    fn write(&self, c: &mut Canon) {
        self.arg.write(c);
        c.seq(self.rows.len());
        for r in &self.rows {
            c.u32(u32::from(r.sid.raw()))
                .str(&r.rk)
                .str(&r.label)
                .str(&r.detail)
                .option(r.value.as_deref(), |c, v| {
                    c.str(v);
                })
                .option(r.badge.as_deref(), |c, b| {
                    c.str(b);
                })
                .bool(r.checked);
        }
        c.u32(self.table.sel as u32);
        self.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("alt");
    }
}

impl<H: AppLike<Memory = PageMemory>> Screen<H> for AltSourcesScreen {
    fn name(&self) -> &'static str {
        "alt"
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {
        Glass::CACHED.prepare(&mut self.glass, false);
    }
    /// The modal dim, asked for rather than drawn. Nothing is lifted back out of it: this picker
    /// hangs under the detail page's Source chip, and the chip is a control that says which copy
    /// is playing rather than the subject of the panel — unlike the card menu's tile or the
    /// profile menu's chip, there is nothing here whose dimming contradicts what the panel is
    /// about.
    ///
    /// **The ordering this replaces was load-bearing and is now the container's** (§16.3): a
    /// `Glass::CACHED` ground samples the default framebuffer as it stands, so the dim has to be
    /// down before the panel's backdrop is taken or the frosted ground reads brighter than the
    /// dimmed screen around it. `ModalStack::draw_scrims` draws it at the end of the PAGE pass,
    /// which is strictly earlier than the surface pass this `draw` runs in, and multiplies by the
    /// appear spring and by `nav::page_alpha` — the second of which the in-`draw` version could
    /// not reach, `DrawFrame::page_alpha` being the surface's own spring alone.
    fn scrim(&self) -> Scrim {
        Scrim::dim(SCRIM_A)
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let appear = f.page_alpha;
        let r = self.frame();
        let p = f.painter.alpha(appear).translate(0.0, RISE * (1.0 - appear));
        let measure = f.measure;
        // Named for `/tmp/plxnative-cpuprof` beside the page's own phases, so a slow frame while
        // this panel is up can be read as the PANEL or as the host under it.
        crate::ui::profile::phase("dt.alt", || {
            Glass::CACHED.panel(p, r, RISE * (1.0 - appear), PANEL_RAD);
            self.table.draw(p, r, measure);
        });
        // The hit map's stops are registered against the SETTLED geometry (`self.frame()`, what
        // `Focusable::place` answers) rather than the transient slide `p` draws with — at rest
        // (`appear == 1`) the two coincide exactly (`RISE * (1.0 - appear)` is 0), and only during
        // the open/close fade would they differ; `ui/hit.rs`'s own module doc already carries the
        // wider version of this gap (no container feeds a fading surface's alpha into the map
        // yet), so this keeps the one known edge rather than inventing a second.
        let hit_p = f.painter.alpha(appear);
        for elem in 0..self.rows.len() as u32 {
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                f.stop(
                    hit_p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem,
                        },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "alt_sources_tests.rs"]
mod tests;
