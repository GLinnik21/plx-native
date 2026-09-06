//! **The person page's FILMOGRAPHY route** (`Person Screen.dc.html` §FILMOGRAPHY) — every credit
//! plex.tv has for the person, which department it belongs to, and which of them you actually hold.
//!
//! # It is a composition, and it was not always
//!
//! This screen shipped once as a **bespoke row list**: its own `draw_row`, its own scroll spring
//! and reveal rule, its own eliding, its own crumb and title, and a hit test that was written and
//! then never wired to anything. Every one of those already existed in this directory, and the
//! owner's report was the whole bill for re-writing them — no mouse anywhere, no press-and-hold, a
//! click landing on the department pills whatever you aimed at, no posters, and a title a rung off
//! the rest of the route family. It is now what it should have been:
//!
//! * [`RouteLayout`] for the crumb, the `size::HERO` title and the two-column geometry — the same
//!   call `settings.rs` makes, which is what puts this title on the family's own scale;
//! * [`TableView`] for the rows — which brings the selection pill, two-line rows, the trailing
//!   read-out, the chevron, scrolling, eliding **and `hit_row`**, i.e. the mouse, for free;
//! * [`TabStrip`]/[`TabPill`] for the department filter, with its spans recorded at draw so the
//!   pointer can reach a pill;
//! * `person::filmography` for the model — folding, sorting and the availability join, all pure.
//!
//! A leading POSTER column was tried on the shared widget ([`crate::ui::table::Row`]) and then
//! rejected against the canvas: a poster at row height read as a grey rectangle 222 times over, so
//! the canvas settled on pure-text rows plus the ONE preview poster this screen draws itself (see
//! [`step_preview`]). The rule this module is still the cautionary tale for is the same either
//! way: extend the component, then compose it — never draw a bespoke row list beside one that
//! already exists.
//!
//! # What the rows are
//!
//! Discover items (`{DISCOVER}/library/people/{tagKey}/credits`), so a row's identity is a plex.tv
//! catalog ID and its artwork an absolute URL on `image.tmdb.org` or `metadata-static.plex.tv`.
//! Most of them open NOTHING, and that is the screen's whole reason to exist.
//!
//! **Availability is a join on that catalog ID** against `person::Src::matches`, the UNCAPPED index
//! a `/media` landing fills — deliberately not the shelves, which cap at `SHELF_MAX` and would
//! silently un-own a prolific actor's back catalogue. A matched row carries the server's name and
//! the trailing chevron (the same gate as its own OK, so the mark and the press cannot disagree);
//! an unmatched row is drawn at **full strength** with neither — dimming is what an unavailable
//! CONTROL does, and here it would grey out most of the screen to restate what the annotation
//! column already says.
//!
//! It is a screen-owned MODAL, not a `Route`: `app.rs` has no route arm for it and needs none. It
//! does need two POINTER arms, which is the part the first version forgot — see [`pointer_focus`]
//! and [`click`].

use super::consts::*;
use super::route_screen::{RouteGround, RouteLayout};
use super::table::{Row as TRow, Section, TableView};
use super::widgets::{self, SelMark, TabGround, TabPill, TabStrip};
use super::{theme, Painter, Rect, Spring, View};
use crate::person::{Credit, Department};
use crate::plex::ServerId;
use std::ffi::CString;
use std::os::raw::c_int;
use std::os::raw::c_uint;
use std::ptr::addr_of_mut;

// ---- geometry ----------------------------------------------------------------------------------
// Everything positional here comes from `RouteLayout`; the only numbers this screen owns are the
// department strip's own band, because the family has no token for "a filter above a table".

/// **The narrative column's measure for THIS route** — `--route-copy-w`, retuned from the family's
/// default 660 because a filmography's copy is a CAPTION and its list is the screen, where
/// Settings' copy IS the screen. The content column takes the 180 back (884 → 1064) and the region
/// gap does not move; see `RouteLayout::screen_with_copy_w`. Owner report, 2026-09-06: titles were
/// truncating while half the frame held a static heading.
const COPY_W: f32 = 480.0;

/// The department strip's height — the route family's own control height, so a pill here is the
/// same object as a pill in an action row.
const PILL_H: f32 = 60.0;
/// Vertical margin the tab strip's clip rect carries beyond [`PILL_H`] on top AND bottom — see the
/// clip call's own doc for why a flush rect clipped the focused capsule's rounded caps square.
/// `space::LG`: comfortably past the ~2px the focus scale itself needs, with room for the focused
/// capsule's own cast shadow (`theme::CONTROL_CAST_FOCUS`) to fall off before it hits the edge,
/// while still short of the STRIP_BAND gap below or the title row above — an existing token rather
/// than a value solved from the shadow's own blur radius, since this only has to be GENEROUS, not
/// exact: the clip's one job is to stop X-axis bleed, never Y.
const CLIP_VPAD: f32 = theme::space::LG;
/// **How far the strip's left cut is dissolved back into the ground** — `space::XL` (64), a hair
/// under a third of a pill, which is enough that no glyph is ever half-drawn at full strength and
/// short enough that the fade never reaches the pill the cursor is on.
///
/// The cut itself cannot go: the strip shares this row with the narrative column immediately to its
/// left, so a pill sliding off HAS to stop at [`RouteLayout::content`]'s edge or it paints over the
/// screen's own title. What can go is the HARSHNESS — a capsule sliced mid-glyph at a flat vertical
/// line (owner report, 2026-09-06). The right end takes no fade because it takes no cut any more:
/// see [`draw`]'s clip.
const TAB_FADE_W: f32 = theme::space::XL;
/// **The strip plus its region gap take the first 128 of the content column**, which is what makes
/// the list below it exactly eight rows: the column runs 114 → 1026 (912), and 912 − 128 = 784 =
/// 8 × `table::ROW_H_ART`. The design states the 128 and the eight together; keeping the constant
/// spelled as the whole band rather than as a gap is what stops the two drifting apart.
const STRIP_BAND: f32 = 128.0;
/// **The selection PREVIEW** — one poster at poster proportions, in the space the narrative
/// column's caption leaves. `Person Screen.dc.html`, 2026-09-06:
///
/// > Every row carried a 54x81 chip of it and none of them was legible — a poster at that size is
/// > a grey rectangle 222 times over — while the column beside the list stood empty below three
/// > lines of caption. So the art is one object at poster proportions, in the space the caption
/// > leaves, and the rows are pure text.
///
/// It reads as what the narrative column is FOR on this route: the list is the screen, and this is
/// what the list is pointing at.
/// The canvas states `top:330px` flat; **the owner's own device look moved it one small spacing-
/// token step down** (2026-09-06: "the credit count sits too close to the poster… keeping the
/// heading and count in place"), i.e. the crumb/title/copy stack is UNCHANGED and only the artwork
/// drops — which is why the step is added HERE rather than to `draw_narrative`'s copy gap.
const PV: Rect = Rect::new(MARGIN_X, 330.0 + theme::space::SM, 420.0, 630.0);
/// **How long the selection must be STILL before the poster follows it**, in seconds.
///
/// A held D-pad repeats faster than this, which is the whole point: the poster waits for the list
/// to stop rather than following every row it passes through, so scrolling a 222-row career does
/// not strobe two hundred images. It is a PREVIEW — it takes no focus stop and it never races the
/// list.
const PV_SETTLE: f32 = 0.180;
/// The rows' own content inset. What the eye reads as this column's left edge is the poster, not
/// the invisible focus pill behind it, so the first tab pill starts on that guide rather than on
/// the pill's.
const STRIP_INSET: f32 = 20.0;
const TAB_PAD: f32 = 26.0;
/// Air between two department pills — the WIDE rung, shared with the Library's library row (owner
/// call, and see [`widgets::STRIP_GAP_WIDE`] for why the two rows want it and the season strip does
/// not).
const TAB_GAP: f32 = widgets::STRIP_GAP_WIDE;

/// The slide-in. Shares `K_SCROLL` with the page's own scroll for the reason the design system
/// gives for the tab capsules: two things answering one press at two rates read as two objects.
const K_PUSH: f32 = K_SCROLL;

// ---- state -------------------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Tabs,
    List,
}

/// What a press on this screen asks `app.rs` to do. Reported, never performed — `alt_sources`' rule
/// and the reason this module needs no route arm.
pub(crate) enum Action {
    None,
    /// Open that server's detail page for the held copy of this credit.
    Open(ServerId, String),
}

struct Scene {
    open: bool,
    /// 0 closed (parked one frame width right), 1 open. ONE spring drives the slide and the fade,
    /// so the two cannot be mid-transition in different places.
    push: Spring,
    focus: Focus,
    /// the department pill showing, clamped to the model on every rebuild and every move
    tab: usize,
    tabs: TabStrip,
    /// **Horizontal scroll of the department strip**, so a person with enough departments to
    /// overflow the content column (a prolific "Others" bucket, or simply many roles) can still
    /// reach every pill by keeping the focused one on screen — the same minimal scroll-into-view
    /// rule `detail.rs`'s season strip uses (`card_row::reveal`), and for the same reason: the
    /// strip owns one motion, not a spring per pill, and a pill NOT focused holds the scroll rather
    /// than gliding back, or leaving a tab would read as the row "jumping away". [`sc.spans`] stays
    /// in unscrolled content coordinates — this offset is applied only at draw (a translate) and at
    /// the pointer hit test (added back in), never baked into the recorded layout.
    tab_hscroll: Spring,
    /// the focused pill's control-face pop (`TabGround::Plated`) — one, because the strip has one
    /// focused pill and its capsule travels between them.
    pop: widgets::CtlPop<1>,
    /// **The rows, and the widget that owns their focus, scroll and hit test.** The screen keeps no
    /// selection index of its own: `TableView::sel` is the one answer, so the drawn pill, the
    /// keyboard cursor and `hit_row` can never be three different rows.
    table: TableView,
    /// **The display model, rebuilt only when the store changes.** [`crate::person::filmography`]
    /// folds, sorts and JOINS a few hundred rows against every source's guid index; doing that per
    /// frame would be exactly the allocation the shelves on the page below were written to avoid.
    model: Vec<Department>,
    dirty: bool,
    /// The pill labels, NUL-terminated with the model — `TabPill` holds a non-owning pointer, so
    /// these must outlive the draw, and building them per frame would defeat the cache above.
    tab_c: Vec<CString>,
    /// Recorded AT DRAW, for the pointer: the strip's pill spans and the table's frame. A hit test
    /// derived from constants instead would be a second layout, free to disagree with the drawn one
    /// — which is the bug class `library.rs` records for its own chips.
    spans: Vec<(f32, f32)>,
    pill_y: f32,
    /// The strip's own left CUT, in screen space, recorded beside `pill_y` from the same
    /// `l.content.x` [`draw`] hands `Painter::clip`. [`pill_at`]'s doc argues why the hit test
    /// needs it. Starts at `f32::MAX` so a pointer event arriving before the first draw hits
    /// nothing, which is the same "no spans recorded yet" answer the search below it gives.
    clip_x: f32,
    table_frame: Rect,
    /// `(tab, row)` the preview is SHOWING — not what is selected. See [`PV_SETTLE`].
    pv: Option<(usize, usize)>,
    /// The candidate [`step_preview`]'s dwell is currently timing — distinct from `pv`, which is
    /// what the poster is SHOWING. Without the two being separate, `pv_still` cannot tell "the
    /// cursor has been still for 180ms" from "the poster has been wrong for 180ms".
    pv_want: Option<(usize, usize)>,
    /// seconds the live selection has been still
    pv_still: f32,
    /// the app's ONE arrival, not a dissolve of this screen's own: out on `OUT_MS`, in on `IN_MS`
    pv_fade: crate::ui::xfade::Xfade,
}

impl Scene {
    fn new() -> Self {
        Scene {
            open: false,
            push: Spring::at(0.0),
            focus: Focus::Tabs,
            tab: 0,
            tabs: TabStrip::new(),
            tab_hscroll: Spring::at(0.0),
            pop: widgets::CtlPop::new(),
            table: TableView::new(),
            model: Vec::new(),
            dirty: true,
            tab_c: Vec::new(),
            spans: Vec::new(),
            pill_y: 0.0,
            clip_x: f32::MAX,
            table_frame: Rect::new(0.0, 0.0, 0.0, 0.0),
            pv: None,
            pv_want: None,
            pv_still: 0.0,
            pv_fade: crate::ui::xfade::Xfade::new(),
        }
    }
    /// The department showing, or None while the model is empty.
    fn dept(&self) -> Option<&Department> {
        self.model.get(self.tab)
    }
    fn rows(&self) -> &[Credit] {
        self.dept().map(|d| d.rows.as_slice()).unwrap_or(&[])
    }
    /// The credit under the table's own cursor.
    fn cur(&self) -> Option<&Credit> {
        usize::try_from(self.table.sel)
            .ok()
            .and_then(|i| self.rows().get(i))
    }
}

static mut SCENE: Option<Scene> = None;
fn scene() -> &'static mut Scene {
    unsafe { (*addr_of_mut!(SCENE)).get_or_insert_with(Scene::new) }
}

// ---- open / close ------------------------------------------------------------------------------

pub(crate) fn is_open() -> bool {
    scene().open
}

/// Open the route on the department the provider leads with, at its first row.
///
/// It does NOT restore where you were last time, and that is the same call the person page makes
/// about its own header: a screen re-entered from a control is a fresh look at a list, not a
/// resumed position, and a list that opened part-scrolled would be describing a session nobody
/// remembers being in.
///
/// **The preview has to be told the same thing.** [`Scene::pv`] is not part of that reset above it
/// — a screen re-entered after selecting row 12 last visit left `pv` pointing at row 12 while focus
/// itself came back to the tabs, so the preview showed a poster nothing on screen was selecting.
/// [`step_preview`]'s own rule ("on the strip: hold whatever the rows last chose") is right for
/// walking the strip WITHIN one visit; it was never meant to survive a close.
///
/// **So does the strip's own scroll.** A visit that walked right into an overflowing strip left
/// [`Scene::tab_hscroll`] parked at whatever offset revealed the tab it was on; without a reset here
/// a fresh open (department reset to 0, focus reset to the strip) would GLIDE back from that stale
/// offset instead of resting there — tab 0, nominally focused, rendering off to the left for a beat,
/// or on a narrower-stripped person the whole row briefly off-screen. `jump`, not a target: this is
/// a mount, not a motion anything should be seen travelling.
pub(crate) fn open() {
    let sc = scene();
    sc.open = true;
    sc.focus = Focus::Tabs;
    sc.tab = 0;
    sc.pv = None;
    sc.pv_want = None;
    sc.pv_still = 0.0;
    sc.tab_hscroll.jump(0.0);
    sc.dirty = true;
    crate::ui::idle::invalidate();
}

pub(crate) fn close() {
    let sc = scene();
    if !sc.open {
        return;
    }
    sc.open = false;
    // the next opening samples the page underneath afresh — a latch kept across a close would
    // ground the NEXT person's filmography on the last one's colours
    unsafe { (*addr_of_mut!(GROUND)).reset() };
    crate::ui::idle::invalidate();
}

/// Tear the route down with NO animation — for the page itself going away (`person::leave`), and
/// for a SECOND person mounting under it (`person::reopen`). A modal sliding out over whatever
/// arrives next is a frame of the previous person's filmography on somebody else's page.
///
/// **Resets the same focus/preview/scroll state [`open`] does**, even though today's only callers
/// go on to mount a fresh [`Scene`] anyway (`person::reopen`'s `*sc = Scene::new()`) or never call
/// `open` again this session (`person::leave`) — so nothing currently observes skipping it. It is
/// reset here anyway rather than left as an implicit "only `open` clears this" invariant: the two
/// functions tear down the same modal, and a future caller of `hide` that did NOT immediately
/// remount would otherwise inherit a stale `tab`/`focus`/`pv`/`tab_hscroll` with nothing to say so.
pub(crate) fn hide() {
    let sc = scene();
    sc.open = false;
    sc.push.jump(0.0);
    sc.focus = Focus::Tabs;
    sc.tab = 0;
    sc.pv = None;
    sc.pv_want = None;
    sc.pv_still = 0.0;
    sc.tab_hscroll.jump(0.0);
    sc.model.clear();
    sc.tab_c.clear();
    sc.spans.clear();
    sc.dirty = true;
    unsafe { (*addr_of_mut!(GROUND)).reset() };
}

/// The store has changed under us — rebuild the model on the next update. Called from
/// `person::update` on the frames the pump reports a landing: a share answering five seconds in
/// must turn "no server behind this" into a real annotation without anybody remembering to
/// invalidate a cache.
pub(crate) fn mark_dirty() {
    scene().dirty = true;
}

// ---- the model ---------------------------------------------------------------------------------

fn rebuild(sc: &mut Scene) {
    sc.dirty = false;
    sc.model = crate::person::current()
        .map(crate::person::filmography)
        .unwrap_or_default();
    sc.tab = sc.tab.min(sc.model.len().saturating_sub(1));
    sc.tab_c = sc
        .model
        .iter()
        .map(|d| {
            CString::new(format!("{} · {}", d.title, d.total)).unwrap_or_default()
        })
        .collect();
    // **A rebuild must not rewind the reader.** `mark_dirty` fires on every frame the person pump
    // reports a landing — a profile, a roles batch, a `/media` answer, a friend's share arriving
    // five seconds in — and `slide=false` JUMPS the widget's scroll to zero. So somebody 60 rows
    // into a 222-row career was thrown back to the top by a response that usually changes nothing
    // but an availability annotation. Snap only when the LIST is not what holds focus: `open` and
    // `hide` both set `focus = Focus::Tabs` before marking dirty, so a mount still lands on row 0.
    let reading = sc.open && sc.focus == Focus::List;
    sync_rows(sc, reading);
}

/// Push the showing department's credits into the shared widget. **This is the whole of what used
/// to be `draw_row`** — the poster, the title over the character, the year, the availability chip
/// and the chevron are four builder calls on a `table::Row`.
fn sync_rows(sc: &mut Scene, slide: bool) {
    let rows: Vec<TRow> = sc
        .rows()
        .iter()
        .map(|c| {
            // **Availability is a MIDDOT FACT on the role line, not a column of its own.** With
            // the content column at 1064 the design spends the width on TITLES — which were
            // truncating — and joins the server to the role: `Wallace (voice) · Home Server`. It
            // was a 320-wide `Badge` chip here, which is a chip's job (a short, closed vocabulary
            // like FORCED/SDH) and not a machine name's.
            //
            // It is still the only place a title you HAVE is distinguished from a bare provider
            // credit, and it is still stated only on a MATCHED row — an unmatched row is unknown,
            // not absent, which is also why nothing dims.
            let sub = match c.local.as_ref().and_then(|(sid, _)| {
                crate::plex::server_facts(*sid).map(|f| f.name.clone())
            }) {
                Some(name) if c.role.is_empty() => name,
                Some(name) => format!("{} · {}", c.role, name),
                None => c.role.clone(),
            };
            TRow::new(c.title.clone())
                // The role (and, above, the server) as the row's sub-line — which is what makes
                // this a TALL row, so the pair is centred by `table.rs`'s own `ROW_SUB_GAP` rather
                // than by a gap this screen tuned by eye.
                .detail(sub)
                // The year is the sort key made visible, so it is a COLUMN and not a middot fact.
                // An undated credit reads as an em dash and sorts to the bottom: the wire carries
                // one numeric year where 0 is the only absent value, so "announced" and "the
                // metadata is missing" are the same fact and neither may be claimed as the other.
                .value(match c.year {
                    0 => "—".to_string(),
                    y => y.to_string(),
                })
                .value_dim(true)
                // The chevron marks a row that GOES somewhere — the same gate as its own OK, so
                // the mark and the press cannot disagree — and the column is RESERVED on every row
                // so the year column stays on one guide whichever rows happen to be matched.
                .chevron(c.local.is_some())
                .ticon_slot(true)
        })
        .collect();
    let sel = if sc.focus == Focus::List { sc.table.sel } else { 0 };
    // the CATALOG measure — 98, not a settings table's 92; see `table::TableView::tall_rows`
    sc.table.tall_rows(true);
    let mut sec = Section::new("");
    sec.rows = rows;
    sc.table.set_sections(vec![sec], sel.max(0), slide);
    sc.table.list_focused = sc.focus == Focus::List;
}

// ---- focus -------------------------------------------------------------------------------------

/// The nav keys, while this route is up. The person page forwards every one of them here and draws
/// nothing focusable of its own — `person::move_focus`'s trap, `alt_sources`' shape.
pub(crate) fn move_focus(sym: c_uint) {
    let sc = scene();
    match sym {
        SDLK_LEFT | SDLK_RIGHT => {
            if sc.focus == Focus::Tabs {
                let d: isize = if sym == SDLK_RIGHT { 1 } else { -1 };
                let n = sc.model.len() as isize;
                if n > 0 {
                    let want = (sc.tab as isize + d).clamp(0, n - 1) as usize;
                    if want != sc.tab {
                        pick_tab(sc, want);
                    }
                }
            }
        }
        SDLK_DOWN => {
            if sc.focus == Focus::Tabs {
                if sc.table.n_rows() > 0 {
                    sc.focus = Focus::List;
                    sc.table.list_focused = true;
                    sc.table.sel = 0;
                }
            } else {
                sc.table.move_sel(1);
            }
        }
        SDLK_UP => {
            if sc.focus == Focus::List {
                if sc.table.sel == 0 {
                    sc.focus = Focus::Tabs;
                    sc.table.list_focused = false;
                } else {
                    sc.table.move_sel(-1);
                }
            }
        }
        _ => {}
    }
    crate::ui::idle::invalidate();
}

/// Switch department. The list RESETS to its first row rather than remembering a position per tab:
/// a filter re-pointing itself is a new list, and restoring a cursor into one the user has not seen
/// would scroll a screen they just arrived at.
fn pick_tab(sc: &mut Scene, i: usize) {
    sc.tab = i;
    sc.table.sel = 0;
    // `slide=false`, which is `set_sections`' own prescription for "the whole list changed" — and
    // here it is load-bearing rather than tidy. With `true` the widget KEEPS its scroll, so
    // switching from a deeply-scrolled long department to a short one drew the new list at the old
    // offset: every row clipped away above the frame, a blank content column for ~0.25s, then the
    // three rows sweeping down from nowhere. Only the POINTER could produce it — reaching the strip
    // by key leaves `sel == 0`, so the scroll target was already 0 — which is exactly the kind of
    // path a keyboard-driven test never walks.
    sync_rows(sc, false);
    crate::ui::idle::invalidate();
}

/// OK. Only a HELD credit does anything — see the module doc on why the unheld majority is drawn at
/// full strength and simply does not answer.
pub(crate) fn on_ok() -> Action {
    let sc = scene();
    if sc.focus == Focus::Tabs {
        // the pill is already showing its department; there is nothing else for OK to do here, and
        // dropping focus into the list on OK would make DOWN and OK the same key
        return Action::None;
    }
    match sc.cur().and_then(|c| c.local.clone()) {
        Some((sid, rk)) => Action::Open(sid, rk),
        None => Action::None,
    }
}

/// **Does the focused thing carry a real catalog row** — i.e. may `app.rs` arm the tvOS press and,
/// on a long hold, the item context menu. A pill is not a card; neither is a credit nobody holds.
pub(crate) fn focus_is_card() -> bool {
    let sc = scene();
    sc.focus == Focus::List && sc.cur().is_some_and(|c| c.local.is_some())
}

/// Where OK goes, as the bare pair a Discover row can actually answer with.
pub(crate) fn focused_item() -> Option<(ServerId, String)> {
    let sc = scene();
    if sc.focus != Focus::List {
        return None;
    }
    sc.cur().and_then(|c| c.local.clone())
}

/// The focused row's band on screen, for the context menu's `Opener` anchor. `None` when focus is
/// on the strip, or when the row has scrolled out from under itself.
pub(crate) fn focused_row_rect() -> Rect {
    let sc = scene();
    if sc.focus != Focus::List {
        return Rect::new(0.0, 0.0, 0.0, 0.0);
    }
    sc.table
        .row_rect(sc.table_frame, sc.table.sel)
        .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0))
}

// ---- the pointer -------------------------------------------------------------------------------
// The half the first version was missing entirely. Both arms are called from `app.rs`'s pointer
// handlers ahead of the per-route ladder, because a modal that owns the screen owns the pointer.

/// Hover: move focus to whatever is under the cursor, exactly as the keys would. Returns true when
/// the pointer is over this screen at all — which is always, since it is opaque and full-frame, and
/// is what stops a hover reaching the person page underneath.
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    let sc = scene();
    if !sc.open {
        return false;
    }
    // **A pill does not answer HOVER — only a click** (owner directive, 2026-09-06). Merely moving
    // the cursor across the strip must not re-point the list: switching department RESETS the
    // selection and jumps the scroll, so a pointer crossing three pills on its way somewhere else
    // would rebuild the model three times and land the user in a department they never chose. It is
    // the same rule `tools/stream-screen.py` records for the top tab bar — hover deliberately not
    // forwarded, because it used to park focus on a pill so the next OK opened the wrong thing.
    //
    // Returning `true` still SWALLOWS the hover: the route is opaque and full-frame, so nothing
    // underneath may light up either. The pill simply does not take focus from a cursor.
    if pill_at(sc, mx, my).is_some() {
        return true;
    }
    if let Some(i) = sc.table.hit_row(sc.table_frame, mx, my) {
        if sc.focus != Focus::List || sc.table.sel != i {
            sc.focus = Focus::List;
            sc.table.list_focused = true;
            sc.table.sel = i;
            crate::ui::idle::invalidate();
        }
    }
    true
}

/// Click: focus it, then act. A row commits on the button-down like every row in the app; a pill
/// simply becomes the showing department.
///
/// Returns the action for `app.rs` to perform, and `true` in the second slot when the click landed
/// on something at all — a MISS is still swallowed, because a full-frame modal has no "outside".
pub(crate) fn click(mx: f32, my: f32) -> Action {
    let sc = scene();
    if !sc.open {
        return Action::None;
    }
    if let Some(i) = pill_at(sc, mx, my) {
        sc.focus = Focus::Tabs;
        sc.table.list_focused = false;
        if sc.tab != i {
            pick_tab(sc, i);
        }
        return Action::None;
    }
    if let Some(i) = sc.table.hit_row(sc.table_frame, mx, my) {
        sc.focus = Focus::List;
        sc.table.list_focused = true;
        sc.table.sel = i;
        crate::ui::idle::invalidate();
        return on_ok();
    }
    Action::None
}

/// Which department pill is under the pointer, from the spans RECORDED AT DRAW. Spans are content
/// coordinates; the strip's own scroll only ever moves what is DRAWN (see [`draw`]'s translate), so
/// a screen-space click is converted back by adding it before the search.
///
/// **It is bounded on X by the same edge the scissor cuts at, recorded at draw** (`clip_x`), and
/// that bound is not optional once the strip can scroll. Adding `tab_hscroll.pos` to an
/// unrestricted pointer x maps a point in the NARRATIVE column onto a pill that has scrolled under
/// the cut and is invisible: with seven departments (what `/tmp/plxnative-personcredits` seeds) the
/// strip settles around 500-670px of scroll, and a click on the return crumb or on the route's own
/// "Filmography" title then landed inside pill 0's span and switched department, resetting the row
/// cursor and the list under the reader. Recorded rather than re-derived for the reason `spans` and
/// `pill_y` are: a bound computed here could disagree with the one `draw` actually cut at.
fn pill_at(sc: &Scene, mx: f32, my: f32) -> Option<usize> {
    if my < sc.pill_y || my > sc.pill_y + PILL_H || mx < sc.clip_x {
        return None;
    }
    let mx = mx + sc.tab_hscroll.pos;
    sc.spans
        .iter()
        .position(|(x, w)| mx >= *x && mx < *x + *w)
}

/// Department strip's horizontal scroll target — the shared `card_row::reveal` minimal-scroll-
/// into-view rule `detail.rs`'s season strip runs on the exact same component, so a person with
/// enough departments to overflow the content column (a big "Others" bucket, or simply many roles)
/// still keeps the focused pill reachable. Holds while focus has left the strip, for the same
/// reason the season strip holds: gliding back to the start the moment DOWN leaves it would read
/// as the row jumping away under a focus that is no longer even on it.
fn tab_hscroll_target(sc: &Scene, content: Rect) -> f32 {
    if sc.focus != Focus::Tabs {
        return sc.tab_hscroll.pos;
    }
    let Some(&(x, w)) = sc.spans.get(sc.tab) else {
        return sc.tab_hscroll.pos;
    };
    // The RIGHT bound stays the design's content column even though the clip now runs to the panel
    // (see [`draw`]): the column is the measure this route is laid out on, so a focused pill belongs
    // inside it, and the 96px beyond is where the pills it has scrolled past bleed off.
    let lo = x + w + TAB_GAP - (content.x + content.w); // pill's right edge (+ context) on screen
    // …and the LEFT context is [`TAB_FADE_W`], not `TAB_GAP`, because that end is not a clean edge
    // any more: a pill revealed to `content.x + TAB_GAP` would come to rest UNDER the dissolve and
    // be drawn half-transparent — the focused one, at that. Revealing past the band's full width is
    // the only place this number belongs; it also leaves the pill behind it visible through the
    // fade, which is what says the row continues that way.
    let hi = x - TAB_FADE_W - content.x; // pill's left edge (− the fade's own width) on screen
    crate::ui::card_row::reveal(sc.tab_hscroll.pos, lo, hi, f32::MAX) // no content-max: `hi` bounds it
}

// ---- update ------------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    let sc = scene();
    if sc.dirty {
        rebuild(sc);
    }
    sc.push.step(if sc.open { 1.0 } else { 0.0 }, K_PUSH, dt);
    // Everything below is motion INSIDE the screen; a closed and settled one steps nothing, the
    // same contract every popover's `update` carries.
    if !sc.open && sc.push.pos < 0.001 {
        return;
    }
    sc.pop.step((sc.focus == Focus::Tabs).then_some(0), dt);
    let sel = sc.tab as c_int;
    let foc = match sc.focus {
        Focus::Tabs => sel,
        Focus::List => -1,
    };
    let spans = sc.spans.clone();
    sc.tabs.update(
        sel,
        foc,
        |i| spans.get(i).copied(),
        // The strip TRAVELS: stepping across departments is a filter re-pointing itself, which is
        // the case `SelMark::Travels` is for.
        SelMark::Travels,
        dt,
    );
    let content = RouteLayout::screen_with_copy_w(COPY_W).content;
    let tht = tab_hscroll_target(sc, content);
    sc.tab_hscroll.step(tht, 240.0, dt); // same rate as detail.rs's season-tab glide
    let fh = sc.table_frame.h;
    sc.table.update(dt, fh);
    step_preview(sc, dt);
}

/// **The preview follows the selection, but only once it has STOPPED.**
///
/// Three rules, all the canvas's. It waits [`PV_SETTLE`] of stillness before swapping, so a held
/// D-pad — which repeats faster than that — leaves it alone instead of strobing every row it
/// passes. It swaps on the app's ONE arrival (`Xfade`: out on `OUT_MS`, in on `IN_MS`), not a
/// dissolve of its own. And **the TABS do not re-point it**: they choose which list you are in,
/// while the poster answers "what is selected", so walking the strip keeps whatever the rows last
/// pointed at and it only re-points once focus goes back down into them.
fn step_preview(sc: &mut Scene, dt: f32) {
    let want = (sc.focus == Focus::List)
        .then(|| usize::try_from(sc.table.sel).ok().map(|i| (sc.tab, i)))
        .flatten()
        .or(sc.pv); // on the strip: hold whatever the rows last chose
    // **The dwell restarts whenever the CANDIDATE moves, not only when it catches up with the
    // poster.** Without this line `pv_still` measured time-since-the-poster-disagreed, which climbs
    // straight through a held D-pad: the repeat lands every 110ms (`app.rs`'s hold arm) against a
    // 180ms `PV_SETTLE`, so the timer never reset, the swap fired 180ms after each commit, and the
    // 420x630 poster faded fully OUT and back IN roughly three times a second for the whole hold —
    // the exact strobing this gate exists to prevent, at a third of the row rate, and landing on
    // whichever row happened to be current (often a placeholder whose art had not arrived).
    if want != sc.pv_want {
        sc.pv_want = want;
        sc.pv_still = 0.0;
    }
    if want != sc.pv {
        sc.pv_still += dt;
        if sc.pv_still >= PV_SETTLE {
            sc.pv_fade.reload();
        }
    } else {
        sc.pv_still = 0.0;
    }
    if sc.pv_fade.tick(dt, true) {
        sc.pv = want;
        sc.pv_still = 0.0;
    }
}

// ---- draw --------------------------------------------------------------------------------------

/// Draw the route over whatever the person page just drew. Nothing is drawn while it is fully
/// parked — including its own opaque ground, which would otherwise blank the page behind it.
pub(crate) fn draw() {
    let sc = scene();
    let e = sc.push.pos;
    if e < 0.001 {
        return;
    }
    let p = Painter::root()
        .translate((1.0 - e) * SCR_W, 0.0)
        .alpha(e.min(1.0));
    // **The route family's own frozen four-corner ground**, not the flat app grey this drew at
    // first. `draw_host` samples the page underneath ONCE, on the frame the route opens, and holds
    // it — which is what "frozen" means here, and why the person page may keep moving without the
    // ground breathing under it. A flat `SURFACE_APP` fill is the BROWSING screens' answer, and it
    // made this read as a different family of screen from Settings and Legal, which it belongs to.
    unsafe { (*addr_of_mut!(GROUND)).draw_host(p) };

    // The family's two columns, with the narrative one at THIS route's measure — see `COPY_W`.
    let l = RouteLayout::screen_with_copy_w(COPY_W);
    // Crumb, `size::HERO` title, one line of copy: the same call `settings.rs` makes, which is what
    // puts this title on the family's own scale.
    //
    // **The copy states the SHAPE of the list and nothing else.** Availability is a per-ROW fact,
    // so the rows carry it — a server name beside the role, and the chevron only where OK actually
    // goes somewhere. Saying it up here meant explaining the wire instead of naming the list.
    let name = crate::person::current().map(|pp| pp.name.clone()).unwrap_or_default();
    let total: usize = sc.model.iter().map(|d| d.total).sum();
    l.draw_narrative(
        p,
        Some(&name),
        "Filmography",
        &format!("{total} credits · Newest first."),
        theme::size::LABEL,
    );

    // …the department strip at the head of the content column, on the NARRATIVE TITLE's own guide
    // (`--route-top`) rather than the crumb's. The content REGION starts one table-lift above that
    // — which is where a table's first section label would go — and the strip stands `--route-
    // table-lift` into it, so the crumb keeps the top line to itself and the strip reads as level
    // with the title beside it. Putting the strip on the region's own top instead floated it ABOVE
    // the crumb, which reads as chrome belonging to no column.
    //
    // It is inset to the ROWS' content edge: what the eye takes for this column's left edge is the
    // poster, not the invisible focus pill behind it.
    let table_top = l.sectioned_table().y;
    let pill_y = l.content.y;
    sc.pill_y = pill_y;
    sc.clip_x = l.content.x; // the edge the scissor below cuts at — see `pill_at`
    sc.spans = tab_spans(sc, l.content.x + STRIP_INSET);
    if !sc.tab_c.is_empty() {
        // The strip's own scroll (see `tab_hscroll_target`) only ever moves what is DRAWN: `spans`
        // stays in unscrolled content coordinates, recorded that way for `pill_at`, and everything
        // below is offset by one translate instead. Clipped at the content column's LEFT edge —
        // unlike `detail.rs`'s season strip, which spans nearly the full screen and simply CULLS an
        // off-screen pill, this strip shares the row with the narrative column immediately beside
        // it, so a pill sliding off would otherwise paint straight over that column's title.
        let sx = sc.tab_hscroll.pos;
        let pt = p.translate(-sx, 0.0);
        // **Clipped through `p`, never `pt`.** `Painter::clip`'s own doc says the rect is taken "in
        // this painter's space — the cascade translate is folded in", so a clip issued through the
        // ALREADY-TRANSLATED painter would scroll the clip boundary itself by the same `-sx`, moving
        // it off the fixed guide it is meant to hold — the exact bug a device screenshot caught: at a
        // nonzero scroll the whole boundary slid with the content, so the strip stopped being cut
        // where the column ends at all. The rect is fixed in SCREEN space, so it clips through `p`.
        //
        // **Vertically padded by [`CLIP_VPAD`], not flush to `PILL_H`.** The clip only needs to
        // exist on X — it is the scrolled pill's LEFT/RIGHT edges bleeding into the narrative column
        // this guards against, per the doc above — but a bare `PILL_H`-tall rect clips Y too, and the
        // focused capsule is NOT `PILL_H` tall: `TabStrip::draw`'s `cap()` scales the plated focus
        // capsule about its own centre (`Rect::scaled`) by `CTRL_FOCUS_SCALE`, and — since this
        // strip's focus capsule casts now (owner correction, 2026-09-06) — `control_cast` paints a
        // two-stop shadow (`theme::CONTROL_CAST_FOCUS`, blur up to 32px) OUTSIDE the capsule's own
        // box on top of that. A flush clip cropped both to a flat line at `pill_y`/`pill_y+PILL_H`,
        // shaving the rounded caps off square exactly where the row's vertical bound sat — the owner
        // caught it live, focused, mid-animation. `CLIP_VPAD` clears the scale growth
        // (`PILL_H*(CTRL_FOCUS_SCALE-1)/2` ≈ 2px) with room to spare for the shadow's own falloff,
        // and it is symmetric so growing OR shrinking `PILL_H` cannot silently reopen the gap.
        //
        // **And it is bounded on the LEFT only** (owner correction, 2026-09-06): the right edge runs
        // to the panel, not to `l.content.w`. The column's right bound had nothing to protect —
        // there is no second column over there, only this route's own 96px margin — so all it did
        // was slice a pill at an invisible line 96px short of the screen. A strip that runs off the
        // panel edge is this app's own idiom (every shelf on Home does it) and it is also the honest
        // signal that the row continues.
        let clip_top = pill_y - CLIP_VPAD;
        let clip_h = PILL_H + 2.0 * CLIP_VPAD;
        p.clip(Rect::new(
            l.content.x,
            clip_top,
            SCR_W - l.content.x,
            clip_h,
        ));
        // capsules first: they are the pills' ground, and a label under an opaque focus capsule is
        // a label nobody can read
        sc.tabs.draw(
            pt,
            pill_y,
            PILL_H,
            TabGround::Plated {
                pop: sc.pop.scale(0),
            },
        );
        let env = super::Env::inert();
        for (c, (x, w)) in sc.tab_c.iter().zip(sc.spans.iter().copied()) {
            let (fm, sm) = sc.tabs.mixes((x, w));
            TabPill::new(c.as_ptr(), theme::size::BODY, Rect::new(x, pill_y, w, PILL_H))
                .plated()
                .mix(fm, sm)
                .draw(&env, pt);
        }
        p.clip_clear();

        // **The cut, dissolved.** A scissor is a hard line and nothing about it can be softened
        // from inside — there is no fade shader for arbitrary geometry here, and `TextView`'s two
        // (`fade_last`, `edge_fade`) run on a text RUN, which a capsule is not. So the band is laid
        // OVER the cut in the exact colour of the ground beneath it, at a falling alpha: opaque on
        // the clip line, gone `TAB_FADE_W` later. `RouteGround::sample` is what makes that exact
        // rather than approximate — this ground is keyed to the host page's artwork, so the colour
        // is different on every person's page and no authored token could stand in for it.
        //
        // Drawn only while something is actually scrolled off. At rest the first pill sits on the
        // column edge with nothing behind it, and a band there would still be invisible (ground over
        // ground) but would cost a full-alpha quad every frame for nothing.
        if sx > 0.5 {
            let fade = Rect::new(l.content.x, clip_top, TAB_FADE_W, clip_h);
            let g = unsafe { &*addr_of_mut!(GROUND) };
            let corner = |x: f32, y: f32, a: f32| {
                let c = g.sample(x, y);
                [c[0], c[1], c[2], a]
            };
            p.grad4(
                fade,
                [
                    corner(fade.x, fade.y, 1.0),
                    corner(fade.x + fade.w, fade.y, 0.0),
                    corner(fade.x + fade.w, fade.y + fade.h, 0.0),
                    corner(fade.x, fade.y + fade.h, 1.0),
                ],
            );
        }
    }

    // …and the table under it, which owns everything else. `STRIP_BAND` is the whole first band of
    // the content column, so the viewport below it is a whole number of rows by construction.
    // …discounted by the widget's OWN top pad, so the first ROW — not the frame — lands on the
    // design's 242 guide. `sectioned_table` makes that adjustment for a table whose first section
    // has a LABEL to put there; this one has no header (the pill above already names the
    // department), so the air would otherwise sit empty above the first row and push all eight
    // down. The frame grows by the same amount, which is what keeps the viewport a whole number
    // of rows.
    // **Both pads, not just the top one.** `TableView::update` measures its scroll viewport as
    // `frame.h - TOP_PAD - BOT_PAD`, so cancelling only `TOP_PAD` left the widget believing the
    // viewport was 764 where the design says 784 — eight rows of 98. The 20px shortfall invented a
    // `max_scroll` of 20 on a department whose rows ALL already fit: focusing the seventh row
    // sprang the whole list up 20px and pressing back sprang it down again, for nothing, and the
    // scroll rail drew permanently — a 6px bar filling 97.6% of a track that cannot travel.
    let top = table_top + STRIP_BAND - crate::ui::table::TOP_PAD;
    sc.table_frame = Rect::new(
        l.content.x,
        top,
        l.content.w,
        (l.content.y + l.content.h - top + crate::ui::table::BOT_PAD).max(0.0),
    );
    sc.table.draw(p, sc.table_frame);

    // …and the preview, in the space the caption leaves in the narrative column. It is drawn AFTER
    // the list because it is the thing the list points at, and its own arrival alpha rides on top
    // of the route's push alpha rather than replacing it.
    if let Some(thumb) = sc.pv.and_then(|(t, i)| {
        sc.model
            .get(t)
            .and_then(|d| d.rows.get(i))
            .map(|c| c.thumb.clone())
    }) {
        if !thumb.is_empty() {
            // Proxied by whichever server is CURRENT: a Discover thumb is an absolute URL on
            // another host, and `/photo/:/transcode?url=…` fetches it server-side, so any reachable
            // server can serve it. The MATCHED server is about navigation, not pictures — binding
            // the art to it would blank the preview the moment one share went quiet.
            let pp = p.alpha(sc.pv_fade.alpha());
            widgets::card(
                pp,
                PV,
                widgets::Art::Thumb {
                    sid: crate::plex::current_server(),
                    key: &thumb,
                    res: (PV.w as c_int, PV.h as c_int),
                },
                theme::CARD_RING_RAD,
                false,
                1.0,
                0.0,
            );
        }
    }

    // The rail, on the column's own right edge and clear of the focus pill's run. It draws NOTHING
    // when the list fits, which is the design's `railOpacity: 0` — a rail on a list that does not
    // scroll is a control that cannot move.
    let content_h = sc.table.measured_height();
    widgets::continuous_scroll_rail(
        p,
        Rect::new(
            sc.table_frame.x + sc.table_frame.w - widgets::RAIL_W,
            sc.table_frame.y,
            widgets::RAIL_W,
            sc.table_frame.h,
        ),
        sc.table.scroll_pos(),
        content_h,
        sc.table_frame.h,
    );
}

/// The route's own frozen ground — see [`draw`]. A module singleton for the same reason
/// `settings.rs`'s is: the latch must survive every frame the route is up, and be reset when it
/// closes so the next person's page is sampled afresh.
static mut GROUND: RouteGround = RouteGround::new();

/// Content-space `(x, w)` of every department pill — the ONE layout the strip's capsules are placed
/// from, the pills are drawn at and [`pill_at`] hit-tests against, so a capsule can never come to
/// rest off a pill and a click can never land on a pill nobody sees.
fn tab_spans(sc: &Scene, x0: f32) -> Vec<(f32, f32)> {
    let mut x = x0;
    sc.tab_c
        .iter()
        .map(|c| {
            let w = crate::text::text_width(c.as_ptr(), theme::size::BODY, 1) + 2.0 * TAB_PAD;
            let span = (x, w);
            x += w + TAB_GAP;
            span
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The screen keeps NO selection of its own — the widget's `sel` is the one answer. This is the
    /// invariant that makes the mouse and the keys agree, and the first version failed it by
    /// construction: it had a `sel` field the hit test never wrote.
    #[test]
    fn the_cursor_lives_in_the_widget_and_nowhere_else() {
        let _serial = crate::testlock::serial();
        let sc = scene();
        *sc = Scene::new();
        sc.model = vec![Department {
            title: "Actor".into(),
            total: 3,
            rows: (0..3)
                .map(|i| Credit {
                    title: format!("Film {i}"),
                    role: format!("Character {i}"),
                    year: 2020 - i,
                    thumb: String::new(),
                    local: None,
                })
                .collect(),
        }];
        sc.tab_c = vec![CString::new("Actor · 3").unwrap()];
        sync_rows(sc, false);
        sc.focus = Focus::List;
        sc.table.list_focused = true;
        sc.table.sel = 0;

        move_focus(SDLK_DOWN);
        assert_eq!(scene().table.sel, 1, "DOWN must move the WIDGET's cursor");
        assert_eq!(
            scene().cur().map(|c| c.title.clone()),
            Some("Film 1".to_string()),
            "the credit read back must be the one the widget has focused"
        );
    }

    /// **The content column is the design's own arithmetic**, and it is worth a test because every
    /// number in it is derived rather than authored: the column runs from the table's lifted top
    /// (`--route-top` − `--route-table-lift` = 114) to the safe frame's bottom (1026), the strip
    /// band takes the first 128, and what is left is EXACTLY eight rows of `table::ROW_H_ART`.
    /// A retune of any rung — the safe margin, `TOP_INSET`, the table lift — shows up here as a
    /// fractional row rather than as a row clipped in half on a television.
    #[test]
    fn the_viewport_is_a_whole_number_of_rows() {
        let l = RouteLayout::screen_with_copy_w(COPY_W);
        // the split the design states for this route: narrative 480, content 1064 at x=760
        assert_eq!(l.narrative.w, 480.0);
        assert_eq!(l.content.x, 760.0);
        assert_eq!(l.content.w, 1064.0);

        let table_top = l.sectioned_table().y;
        // The FRAME `draw` builds, then the viewport `TableView::update` measures inside it —
        // which subtracts BOTH pads, not just the top one. Grading the frame alone is what let the
        // widget believe the viewport was 764: it invented a 20px `max_scroll` on a department
        // whose rows all fit, so focusing the seventh row sprang the list up and the scroll rail
        // drew permanently over a track that could not travel.
        let top = table_top + STRIP_BAND - crate::ui::table::TOP_PAD;
        let frame_h = l.content.y + l.content.h - top + crate::ui::table::BOT_PAD;
        let h = frame_h - crate::ui::table::TOP_PAD - crate::ui::table::BOT_PAD;
        assert_eq!(h, 784.0, "the list viewport is not the design's 784");
        // …divided by the row-height CONSTANT, never a literal: spelling 98 here means a retune of
        // `ROW_H_ART` leaves a credit clipped in half at the fold with this test still green, which
        // is the one thing its own failure message says it exists to prevent.
        assert_eq!(
            h / crate::ui::table::ROW_H_ART,
            8.0,
            "the viewport must hold a WHOLE number of rows — a half row at the fold is the one \
             thing this arithmetic exists to prevent"
        );
    }

    /// **A pointer left of the strip's own cut is over the NARRATIVE column, not over a pill.**
    ///
    /// `pill_at` converts a screen x back into content space by adding the strip's scroll. Without
    /// a left bound that conversion is unrestricted, so once the strip has scrolled — which needs
    /// only five departments — a click on the return crumb or on the route's own 72px "Filmography"
    /// title mapped onto a pill that had slid under the cut and was not on screen at all, switching
    /// department and resetting the row cursor under the reader.
    #[test]
    fn a_click_left_of_the_strips_cut_is_not_a_click_on_a_pill() {
        let _serial = crate::testlock::serial();
        let sc = scene();
        *sc = Scene::new();
        let l = RouteLayout::screen_with_copy_w(COPY_W);
        sc.pill_y = l.content.y;
        sc.clip_x = l.content.x;
        // Seven departments is what `/tmp/plxnative-personcredits` seeds and what makes the strip
        // overflow at all; walking to the far end settles the scroll in the hundreds. The first two
        // pills are then off screen to the LEFT of the cut, and the third is the one drawn at it.
        sc.spans = vec![
            (l.content.x + 20.0, 200.0),  // 780..980, drawn at 130..330 — under the narrative column
            (l.content.x + 236.0, 200.0), // 996..1196, drawn at 346..546 — likewise
            (l.content.x + 670.0, 200.0), // 1430..1630, drawn at 780..980 — the one on screen
        ];
        sc.tab_hscroll.jump(650.0);

        // The crumb and the title both live in the narrative column, on the strip's own y band —
        // and both map INTO a span once the scroll is added, which is the whole defect: without the
        // bound these are `Some(0)` and `Some(1)`, pills that are not on the screen.
        for (mx, what) in [(150.0_f32, "the return crumb"), (400.0, "the route title")] {
            assert_eq!(
                pill_at(sc, mx, sc.pill_y + PILL_H * 0.5),
                None,
                "a click at x={mx} is on {what}, not on a department pill"
            );
        }
        // …while a click on the pill actually drawn there still lands, or the bound has simply
        // eaten the control instead of bounding it.
        assert_eq!(
            pill_at(sc, l.content.x + 40.0, sc.pill_y + PILL_H * 0.5),
            Some(2),
            "a click inside the cut must still find the pill drawn there"
        );
    }

    /// **The preview's dwell measures STILLNESS.** It is reset when the candidate MOVES, not only
    /// when the poster catches up with it — otherwise a held D-pad, whose repeat is faster than
    /// `PV_SETTLE`, never resets it at all and the poster swaps on a fixed cadence forever, which
    /// is the strobing the gate exists to prevent.
    #[test]
    fn a_held_dpad_does_not_swap_the_preview() {
        let _serial = crate::testlock::serial();
        let sc = scene();
        *sc = Scene::new();
        sc.model = vec![Department {
            title: "Actor".into(),
            total: 40,
            rows: (0..40)
                .map(|i| Credit {
                    title: format!("Film {i}"),
                    role: String::new(),
                    year: 2020,
                    thumb: String::new(),
                    local: None,
                })
                .collect(),
        }];
        sc.tab_c = vec![CString::new("Actor · 40").unwrap()];
        sync_rows(sc, false);
        sc.open = true;
        sc.focus = Focus::List;
        sc.pv = Some((0, 0));
        sc.pv_want = Some((0, 0));

        // app.rs's hold arm repeats every 110ms; PV_SETTLE is 180ms. Two seconds of that.
        let dt = 1.0 / 60.0;
        let mut since_rep = 0.0_f32;
        let mut swaps = 0;
        let mut last = sc.pv;
        for _ in 0..120 {
            since_rep += dt;
            if since_rep >= 0.110 {
                since_rep = 0.0;
                sc.table.sel = (sc.table.sel + 1).min(39);
            }
            step_preview(sc, dt);
            if sc.pv != last {
                swaps += 1;
                last = sc.pv;
            }
        }
        assert_eq!(
            swaps, 0,
            "the poster must hold still through a hold; it swapped {swaps} time(s)"
        );

        // …and it does swap once the cursor stops, or the gate has simply frozen the preview.
        for _ in 0..60 {
            step_preview(sc, dt);
        }
        assert_eq!(
            sc.pv,
            Some((0, sc.table.sel as usize)),
            "once the list is still, the preview must follow it"
        );
    }

    /// A credit nobody holds is not a card: no press may be armed on it, and OK must do nothing.
    /// The mark and the press read the same answer, which is what stops a row that looks openable
    /// from being inert.
    #[test]
    fn only_a_held_credit_is_a_card() {
        let _serial = crate::testlock::serial();
        let sc = scene();
        *sc = Scene::new();
        sc.model = vec![Department {
            title: "Actor".into(),
            total: 2,
            rows: vec![
                Credit {
                    title: "Held".into(),
                    role: String::new(),
                    year: 2020,
                    thumb: String::new(),
                    local: Some((ServerId::UNSET, "42".into())),
                },
                Credit {
                    title: "Not held".into(),
                    role: String::new(),
                    year: 2019,
                    thumb: String::new(),
                    local: None,
                },
            ],
        }];
        sc.tab_c = vec![CString::new("Actor · 2").unwrap()];
        sync_rows(sc, false);
        sc.focus = Focus::List;
        sc.table.list_focused = true;

        sc.table.sel = 0;
        assert!(focus_is_card());
        assert!(matches!(on_ok(), Action::Open(_, ref rk) if rk == "42"));

        scene().table.sel = 1;
        assert!(!focus_is_card());
        assert!(matches!(on_ok(), Action::None));
    }

}
