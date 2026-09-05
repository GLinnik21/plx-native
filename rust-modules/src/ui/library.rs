//! The Library — **ONE VERTICAL DOCUMENT**, from the library's own published shelves down to the
//! A–Z poster wall, with server-driven sort and filter inside the same scroll.
//!
//! Layout, top to bottom (`Library Screens.dc.html` D, since 2026-09-05): the shared top bar
//! (profile chip leading at the margin + CENTERED tab pills — [`draw_tab_row`] is shared with Home
//! and Search) is the ONLY chrome standing over content; everything else scrolls. The document is
//! the library chip at its head, then this library's own server-published shelves
//! ([`crate::browse::section_hubs`], one [`card_row::CardRow`] each, in the owner's *Manage →
//! Libraries* order), then the grid's heading with its Sort/Filter row a rung under it, then the
//! 6-across grid of [`card_row`] leaf tiles fed by the `browse` paged store. There is no fixed
//! toolbar and no top-chrome scrim any more — a control that scrolls away with the thing it acts
//! on needs no rule about when it is drawn. (`widgets::nav_scrim` was the shared element that
//! backed such a bar; Search became one document too on 2026-09-05 and it was deleted with its last
//! caller.)
//!
//! **[`Layout`] is the one projection.** Every document or grid coordinate comes out of it —
//! `shelf_origin`, `grid_block_top`, `row_y`, `doc_to_grid`, `visible_rows`, `max_scroll` — and the
//! review rule is that none is derived from `SCROLL` by hand outside it. A second rule joined it
//! on 2026-09-05, when the label band learned to collapse: a shelf's pitch is no longer fixed, so
//! there are TWO documents each frame — [`layout`], where the bands are right now (the draw, the
//! pointer, the visible-row window) and [`layout_settled`], where they are going (every scroll
//! target, and nothing else). The grid's paging window
//! and its row culling ask the same `visible_rows` on the same frame's document, which is what
//! stops them disagreeing about which rows matter.
//!
//! Menus are Popover + TableView (the track-menu components), their contents entirely
//! server-driven (`browse::sorts()` from `includeMeta=1`; genres from the value list).
//!
//! Focus model: **a projection, not a ladder.** [`zones`] lists the zones a given [`Layout`]
//! actually DRAWS, in order, and UP, DOWN, BACK, pointer reachability and the async clamp are all
//! derived from that one list — which is why an empty library cannot park focus on a Sort chip it
//! does not draw. Eight [`Area`]s; the shelf RUN is one rung in the list with its own cursor, as
//! the top bar's two stops are. **BACK from anywhere below the head returns to the head of the
//! document** — a deep shelf is as far from the top as a deep grid row — and a second BACK leaves
//! for Home (`app.rs`), which is the Netflix-2025 escape rule with the old "walk to the tab bar
//! first" step retired: the track never hides here, so UP already reaches it in one press.
//! Focus/scroll per section persist in the browse store.
//!
//! **An EPISODE shelf is a shelf of landscape stills and it labels itself differently**
//! (`Library Screens.dc.html` E). A poster is a TITLED object — the show's name is printed inside
//! the artwork, which is why every other tile in the app hides its caption until focus. A still is
//! UNTITLED, so hiding the caption there gives a row of anonymous frames of television. The rule is
//! therefore *whatever the poster prints, the still prints too*: the SHOW goes inside the artwork on
//! every tile through `widgets::still_line` (which also carries the watch glyph, and suppresses the
//! watched disc — one mark per tile), and FOCUS reveals the EPISODE below it, its title over its
//! `S3 · E4` address, never the show again. One fact, one place.
//!
//! Reload choreography: every path that REPLACES the item set (tab, sort, unwatched, genre, and a
//! profile switch wiping the store from underneath) is deferred through [`Xfade`] — fade the
//! outgoing wall out, swap at the floor, fade the new one in — because `browse::requery` empties
//! the store synchronously on the key press, so there is nothing left to fade unless the animation
//! SCHEDULES the swap. The queued action is the typed [`Pending`]; the top chrome reads it through
//! the `view_*` resolvers so a chip relabels on the press frame while the grid is still dissolving.
//!
//! Perf discipline (32-bit A53 + Mali): the grid steps TWO scale springs total (focused +
//! shrinking previous), culls rows with `on_axis`, and only visible cells call `resolve_tex`
//! (the 64-slot poster LRU must never see off-screen requests).
#![allow(non_upper_case_globals)]
use crate::pms::PmsMovie;
use crate::ui::card_row::{self, RowStyle};
use crate::ui::consts::*;
// The Sources ROW MODEL lives one module over, because it is drawn on TWO surfaces: this
// panel, and the first-run route that asks the same question before Home (`ui::onboard`).
use crate::ui::icons::Icon;
use crate::ui::popover::{Opener, Popover};
use crate::ui::source_list::{self, Level, SrcAction, Tail};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::theme;
use crate::ui::widgets::{Art, PageGround, Pill, Spinner, StatusKind};
use crate::ui::xfade::Xfade;
use crate::ui::{on_axis, Env, Painter, Rect, Spring, View};
use std::ffi::CString;
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

// ---- geometry -------------------------------------------------------------------------------
const COLS: usize = 6;
/// **The A–Z rail's own band, reserved on the grid's right whether or not the rail is drawn.**
///
/// The rail used to hang in the OUTER margin, at `SCR_W - 64` — 32px past the 5% overscan frame, the
/// worst horizontal violation in the app, and unfixable by moving the rail alone: with the grid
/// filling `SCR_W - 2*MARGIN_X` exactly there is no room inside the frame for a 44px control to its
/// right. So the grid gives the band up instead.
///
/// **Reserved unconditionally**, which costs 48px on a section whose letters the server never sent.
/// The alternative — widening the grid when [`rail_drawn`] is false — moves every column between two
/// sections of one library, and a grid that reflows as you change tab is a worse answer than a
/// slightly narrower one that never does. It is the same argument [`draw_rail`] makes for
/// top-anchoring the rail rather than centring it.
const RAIL_BAND: f32 = RAIL_TRACK_W + theme::space::XS;
/// The grid's right edge — the rail's band inside the safe area's own right edge.
const GRID_R: f32 = SCR_W - MARGIN_X - RAIL_BAND;
/// 6×250 + 5×LGAP fills the space between the left margin and [`GRID_R`] exactly.
const LGAP: f32 = (GRID_R - MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);
/// **The poster grid's own row style.** `RowStyle::HOME`'s tile, motion and label budget — the grid
/// is Home's shelf laid out in rows rather than a second kind of card — with the one thing that is
/// this screen's alone: everything right of [`GRID_R`] is spoken for, so a focused card's label may
/// not run into it. Every other screen's content edge IS the panel's, which is why the shared clamp
/// assumed it and why this screen had to say otherwise.
///
/// **The reserve is `SCR_W - GRID_R` and not [`RAIL_BAND`]**, which is the same distinction the
/// grid itself already makes: `GRID_R` gives up the rail's band AND the right margin, and the rail
/// TRACK sits inside the first of those — so reserving the band alone still let a long caption
/// reach 1852 against a track that starts at 1780. What is off-limits is the whole column to the
/// right of the content, not the one element standing in it.
///
/// The asymmetry with the LEFT edge is deliberate and is what "reserve" means: a label may breathe
/// into empty margin (`card_row`'s `EDGE_PAD` is the only bound there, well past `MARGIN_X`) and
/// may not breathe into occupied margin.
const GRID_STYLE: RowStyle = RowStyle::HOME.with_right_reserve(SCR_W - GRID_R);
// TOOL_Y/GRID_TOP each moved down 18px on 2026-08-23 with `widgets::TOP_BAR_Y`, which dropped so the
// tab track clears `consts::MARGIN_Y`. They are clearances under that bar (22px and, after the
// chips, 28px), so they follow it — holding them still would have left the Sort chip 4px under the
// track instead of 22.
const TOOL_Y: f32 = 152.0;
const TOOL_H: f32 = 52.0;
pub(crate) const GRID_TOP: f32 = 232.0;
// The top-chrome scrim's own numbers used to live here — a knee at 188, its .40 alpha, and a 56px
// scroll-linked appear. They moved to `widgets::nav_scrim`, the shared treatment for a column
// scrolling under a FIXED bar, and they are gone with it: this screen stopped drawing a bar on
// 2026-09-05 and Search followed the same day, which left that element with no caller at all. The
// design system's `--scrim-in` / `--scrim-knee-a` / per-route content line go stale with them.
//
// What that shape buys, kept here because it is this screen's own measurement: the knee keeps the
// toolbar chips (bottom y≈186) solidly backed while the tail crosses the RESTING popped card's top
// (`GRID_TOP - CARD_H*0.045 ≈ 197`) at ≤~26% — a soft entering-the-shadow veil, not a wash. One
// short linear ramp instead of the knee read as a harsh bar shadow with a visible bottom edge.
/// Row pitch: card + the focused under-label band (title + caption) + air before the next row —
/// item 11. Used to read `CARD_H + 96.0`: a two-line label block (`card_row::UNDER_LABEL_H` = 92)
/// left only 4px of air before the next row's card, glued to it, while Home's shelves left 22px
/// (`consts::UNDER_LABEL_AIR`) under the identical block. Both screens now derive their pitch from
/// the same two named constants instead of each hand-authoring its own air.
const PITCH: f32 = CARD_H + crate::ui::card_row::UNDER_LABEL_H + crate::ui::consts::UNDER_LABEL_AIR;

/// **The document's top edge on screen.** The whole column — chip, shelves, grid heading, grid —
/// is drawn at `CONTENT_TOP - scroll`, so this is the one place a scroll offset becomes a y.
///
/// It is `consts::GRID_TOP_Y`, shared with Home's first shelf, because the two screens hang the
/// same kind of column under the same tab track and a difference would read as the bar moving
/// between them. The prototype's canvas says 176; the code constant wins (it moved to 194 on
/// 2026-08-23 with `widgets::TOP_BAR_Y`, and the canvas did not follow).
pub(crate) const CONTENT_TOP: f32 = crate::ui::consts::GRID_TOP_Y;

/// The library CHIP's own block at the head of the scroll: the chip, then a region gap, then the
/// first shelf heading's own lead. Zero when no chip is drawn — the shelves start at the top.
///
/// **The air is [`consts::CARD_DY`](crate::ui::consts::CARD_DY), the same rung the grid block uses
/// under ITS control row** — so the chip stands off its first shelf exactly as Sort and Filter
/// stand off the first grid row, and the two halves of one document are on one rhythm.
///
/// `Library Interactive.dc.html` spells the block as `52 + 62`, and 62px was taken from the canvas
/// via the nearest `theme::space` rung (`XL` 64). On the panel it reads as a hole: the chip is a
/// control row like any other, and every other control row in this document is followed by 26px.
/// The canvas authored this block before the grid's own heading row existed to be consistent with.
const HEADER_H: f32 = TOOL_H + crate::ui::consts::CARD_DY + crate::ui::consts::TITLE_DY;

/// The grid block's heading band: its title, air, the Sort/Filter control row, air. Everything
/// above the first grid row and below the last shelf.
///
/// Sort and Filter live INSIDE the scroll now, under the grid's own heading, which is what deleted
/// the visibility rule the old fixed toolbar spent two revisions on: a control that scrolls away
/// with the thing it acts on needs no rule about when to show it.
const GRID_HEAD: f32 =
    crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY + TOOL_H + crate::ui::consts::CARD_DY;

/// **THE projection: document scroll → every drawn coordinate.** Computed once per frame and
/// passed to the draw, the hit test, the focus walk and the scroll rule alike.
///
/// The review rule is **"no grid-coordinate derivation from `SCROLL` outside `Layout`"** — not "no
/// `SCROLL` read", which would be wrong: the spring step, the draw translate and view persistence
/// all legitimately read the scroll value.
///
/// It exists because of one sharp bug in the first draft of this screen. `browse::want` computed
/// its page window as `SCROLL / PITCH`, i.e. assuming scroll 0 is grid row 0. Under a document that
/// also contains a header and N shelves, a 12-shelf library would ask `browse` for pages ~13 rows
/// into the catalog while row 0's own slots sat unloaded — a grid that is permanently missing its
/// own top. Every consumer of a grid coordinate goes through [`Layout::doc_to_grid`] for that
/// reason: `want`, row culling, `cell_at`, `rail_jump`, `page`, `save_view`/`restore_view`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Layout {
    /// Is the library chip drawn at the head?
    chip: bool,
    /// How many shelves the COMMITTED set holds. A staged landing is not in here — that is the
    /// whole point of `section_hubs`' publication axis.
    shelves: usize,
    /// Grid rows, from the section's item total.
    rows: usize,
    /// Does the grid draw its heading and its Sort/Filter control row? True over a grid that has
    /// items — the design's rule that a control acting on an empty grid is not drawn — and ALSO
    /// mid-reload, while a grid-scoped `Xfade` is in flight ([`grid_block_reloading`]), where the
    /// store is momentarily empty but the control row is the thing the user just pressed.
    ///
    /// **It does not answer for the rows or the rail.** [`zones`] gates `Area::Grid` and
    /// `Area::Rail` on `rows > 0` on top of this, so `grid_head` can mean heading + control row
    /// with no grid and no rail under them. Those two used to be one test, which was safe only
    /// while `grid_head` implied `total() > 0`.
    grid_head: bool,
    /// Does the failure read-out stand where the grid would? The one zone that is neither a shelf
    /// nor a grid control, and the reason this struct carries it: [`zones`] must be a pure function
    /// of the layout, or a test can hand it a shape and get an answer about the live store.
    status: bool,
    /// **Each shelf's own vertical pitch.** Not one constant, on two axes. SHAPE: a poster shelf is
    /// `consts::ROW_PITCH` and an EPISODE shelf is that less the difference between a poster's
    /// height and a landscape still's. And BAND (2026-09-05): the focused label block exists only
    /// while its shelf holds focus, so an unfocused shelf gives that room back
    /// (`card_row::under_band`) and the shelves under it move up by the difference. Only the first
    /// [`Layout::shelves`] entries mean anything.
    pitch: [f32; MAX_SHELVES],
}

impl Layout {
    /// Build from the screen's current state. Cheap and pure over its four inputs, so a test can
    /// construct any shape without a section table.
    fn new(chip: bool, shelves: usize, rows: usize, grid_head: bool) -> Layout {
        Layout {
            chip,
            shelves,
            pitch: [crate::ui::consts::ROW_PITCH; MAX_SHELVES],
            rows,
            grid_head,
            status: false,
        }
    }
    /// …with a PITCH per shelf, which is what a document holding rows of two shapes needs. A
    /// landscape episode row is 236 tall where a poster row is 375, so a uniform pitch would leave
    /// a 139px hole under every episode shelf. `new` keeps meaning "N poster rows", which is what
    /// every geometry test wants to say.
    fn with_pitches(chip: bool, pitches: &[f32], rows: usize, grid_head: bool) -> Layout {
        let mut lay = Layout::new(chip, pitches.len().min(MAX_SHELVES), rows, grid_head);
        for (slot, p) in lay.pitch.iter_mut().zip(pitches.iter()) {
            *slot = *p;
        }
        lay
    }
    /// …the same, with the failure read-out standing in the content region.
    fn failed(chip: bool, shelves: usize) -> Layout {
        Layout {
            status: true,
            ..Layout::new(chip, shelves, 0, false)
        }
    }

    fn header_h(&self) -> f32 {
        if self.chip {
            HEADER_H
        } else {
            0.0
        }
    }

    /// Document y of shelf `i`'s ORIGIN — the same origin `home.rs` hangs a shelf from, so the
    /// heading draws at `origin - TITLE_DY` and the cards at `origin + CARD_DY`. Sharing the origin
    /// rather than the heading's y is what lets `card_row` draw here unchanged.
    pub(crate) fn shelf_origin(&self, i: usize) -> f32 {
        self.header_h() + self.pitch[..i.min(self.shelves)].iter().sum::<f32>()
    }
    /// Shelf `i`'s own pitch — its whole band, heading lead included.
    pub(crate) fn shelf_pitch(&self, i: usize) -> f32 {
        self.pitch
            .get(i)
            .copied()
            .unwrap_or(crate::ui::consts::ROW_PITCH)
    }

    /// Document y where the grid BLOCK starts — its heading's own lead, not its first row.
    pub(crate) fn grid_block_top(&self) -> f32 {
        self.header_h() + self.pitch[..self.shelves].iter().sum::<f32>()
    }

    /// Document y of grid row 0's card top. The grid's own origin.
    pub(crate) fn grid_top(&self) -> f32 {
        self.grid_block_top()
            + if self.grid_head { GRID_HEAD } else { 0.0 }
    }

    /// **Document scroll → GRID-LOCAL scroll**, the projection everything that thinks in rows must
    /// go through. Negative while the viewport is still above the grid, which callers clamp; a
    /// caller that wants "which rows are visible" wants this, never `SCROLL` itself.
    pub(crate) fn doc_to_grid(&self, scroll: f32) -> f32 {
        scroll - self.grid_top()
    }

    /// The document's full height, including the bottom margin a focused row's caption needs.
    pub(crate) fn doc_h(&self) -> f32 {
        self.grid_top() + self.rows as f32 * PITCH
    }

    /// The furthest the document may scroll.
    ///
    /// **This and [`Layout::row_reveal`] are ONE invariant and a test holds them to it**: if they
    /// disagree the last row's caption sits off-panel, which is the bug the old
    /// `max_y`/`lo` pair carried a paragraph about. Both are expressed here now, from one document
    /// height, so they cannot drift.
    pub(crate) fn max_scroll(&self) -> f32 {
        (self.doc_h() - (SCR_H - CONTENT_TOP) + MARGIN_Y).max(0.0)
    }

    /// **Row snapping**: a focused grid row's scroll target is its OWN TOP EDGE, clamped to the
    /// document. It replaces `card_row::reveal` for this grid — `reveal` keeps a row minimally
    /// on screen, which is right for a shelf inside a page and wrong for the row you are walking
    /// down a wall of posters. `card_row::reveal` is untouched and Home still uses it.
    pub(crate) fn row_reveal(&self, row: usize) -> f32 {
        // **ROW 0 IS THE EXCEPTION, and it is the canvas's own** (`Library Interactive.dc.html`:
        // `f.i === 0 ? L.gridTop : …`). Every other row snaps to its own top edge, so the focused
        // row starts exactly at the content edge and the row above is entirely gone. Row 0 has the
        // grid's heading and its control row directly above it, which are that block's own head —
        // snapping past them would scroll away the count and the two chips the moment focus
        // entered the grid, and UP would have to bring them back.
        let top = if row == 0 {
            self.grid_block_top()
        } else {
            self.grid_top() + row as f32 * PITCH
        };
        // The clamp is what keeps the last row's caption on the panel, and it is the same
        // `max_scroll` the spring is bounded by — one number, one invariant.
        top.clamp(0.0, self.max_scroll())
    }

    /// The first zone that is CONTENT rather than chrome — where the head of the library is, and so
    /// what BACK returns to. The canvas's `firstContent`: the library chip if it is drawn, else the
    /// first shelf, else the grid's own heading block.
    ///
    /// Not the tab bar, which is the shared control ABOVE the page: focus resting there must leave
    /// the scroll where it was (a walk up to the strip is not a request to go to the top), so the
    /// bar cannot also be the thing BACK scrolls to.
    fn first_content(&self) -> Option<Area> {
        if self.chip {
            Some(Area::LibChip)
        } else if self.shelves > 0 {
            Some(Area::Shelf)
        } else if self.status {
            Some(Area::Status)
        } else if self.grid_head {
            Some(Area::Toolbar)
        } else {
            None
        }
    }

    /// **SCREEN y of grid row `row`** at a given document scroll — the ONE expression, shared by
    /// the row loop and by the FOCUSED card's own frame.
    ///
    /// They were two until the simulator showed the focused tile vanishing the instant focus
    /// entered the grid: `focused_card_base` still read the pre-rewrite constant `GRID_TOP`, which
    /// is where the grid used to start when it WAS the screen. Under a document that begins with a
    /// chip and N shelves the real origin is thousands of pixels further down, so the focused
    /// card's `on_axis` test put it off-panel and pass 2 drew nothing at all — a grid whose only
    /// visible tile disappeared when you focused it.
    pub(crate) fn row_y(&self, row: usize, scroll: f32) -> f32 {
        CONTENT_TOP + self.grid_top() + row as f32 * PITCH - scroll
    }

    /// A shelf's scroll target, on the same rule: its own origin's heading lead.
    pub(crate) fn shelf_reveal(&self, i: usize) -> f32 {
        (self.shelf_origin(i) - crate::ui::consts::TITLE_DY).clamp(0.0, self.max_scroll())
    }

    /// The head of the document — where BACK from anywhere below returns to.
    pub(crate) fn head(&self) -> f32 {
        0.0
    }

    /// **Which zone a RESTORED scroll was standing on** — the band's seat on re-entry, derived
    /// rather than stored.
    ///
    /// `browse` remembers a section's scroll and its GRID index, which is all there was to remember
    /// while the grid was the screen. Under one document those two can name different blocks: leave
    /// while standing on a shelf and the scroll comes back at that shelf while the grid index comes
    /// back as whatever row was last walked — so seating the band in the grid asked the spring for
    /// a target thousands of pixels below the page that had just been drawn, and the document slid
    /// out from under the viewer on the frame after re-entry.
    ///
    /// Derived, not stored, for a reason the probe made concrete: two of a library's hubs change
    /// their SUBJECT between requests and sometimes drop out entirely, so a shelf INDEX kept across
    /// a visit can name a different shelf or none. A scroll offset re-read through this frame's own
    /// `Layout` cannot go stale that way — it answers "what was the document showing", which is the
    /// question, through the same reveal targets the walk itself uses.
    ///
    /// `Grid` carries no index: the grid's own row survives in `browse`'s saved focus.
    fn seat_for_scroll(&self, scroll: f32, gr: usize) -> Option<(Area, usize)> {
        let first = self.first_content()?;
        // at the head, the head — the common case (a first visit restores `(0, 0)`), and the one
        // where a bare "nearest reveal" would be ambiguous with shelf 0, whose reveal is also 0
        if scroll <= self.head() + 0.5 {
            return Some((first, 0));
        }
        // otherwise the zone whose own reveal target is nearest what the document is showing
        let mut best = (f32::INFINITY, first, 0usize);
        for i in 0..self.shelves {
            let d = (self.shelf_reveal(i) - scroll).abs();
            if d < best.0 {
                best = (d, Area::Shelf, i);
            }
        }
        if self.grid_head {
            // **The grid's saved row, then its own head, and the winner carries its ROW.** Both
            // candidates matter and so does which one won: `row_reveal(0)` is `grid_block_top()`,
            // which is ALSO the Toolbar's own scroll target, so a visit that left from Sort/Filter
            // restores a scroll that names row 0 while `gr` still holds whatever deep row was last
            // walked. Seating `Grid` and leaving `gr` alone then asked the spring for that deep
            // row and slid the document thousands of pixels on the frame after re-entry — the same
            // defect this function was written to remove, surviving inside it.
            for r in [gr.min(self.rows.saturating_sub(1)), 0] {
                let d = (self.row_reveal(r) - scroll).abs();
                if d < best.0 {
                    best = (
                        d,
                        if self.rows > 0 { Area::Grid } else { Area::Toolbar },
                        r,
                    );
                }
            }
        }
        Some((best.1, best.2))
    }

    /// The grid rows a viewport at `scroll` can see, with one row of lookahead each way. The ONE
    /// answer behind both row culling and [`crate::browse::want`]'s page window, which is what
    /// stops those two from disagreeing about which rows matter.
    pub(crate) fn visible_rows(&self, scroll: f32) -> (usize, usize) {
        if self.rows == 0 {
            return (0, 0);
        }
        let g = self.doc_to_grid(scroll);
        let lo = ((g - CARD_H) / PITCH).floor().max(0.0) as usize;
        let hi = (((g + SCR_H - CONTENT_TOP) / PITCH).ceil().max(0.0) as usize + 1).min(self.rows);
        (lo.min(hi), hi)
    }
}

// ---- screen state (main-thread statics, same discipline as home.rs) -------------------------
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Area {
    /// The shared top bar's PROFILE CHIP, at the margin left of the pills. Not this screen's
    /// control — `widgets` owns its rect, its hit test and its unfurl — but this screen owns
    /// whether its own focus is standing on it, which is the half that used to be missing: the chip
    /// was drawn here and focusable only on Home.
    ///
    /// Reached the way it is on every screen that wears the bar: **LEFT off the first pill**, with
    /// RIGHT walking back onto it. The chip is the bar's leftmost stop, so that is the only walk
    /// the geometry admits; DOWN leaves the band exactly as it does from [`Area::Tabs`].
    Chip,
    Tabs,
    /// The **library chip** at the head of the scroll — `Library · <name> <handle> ▾`. It is the
    /// first thing in the document now rather than a fixed toolbar entry, so it scrolls away with
    /// everything else and is reachable by walking UP to the top of the page.
    LibChip,
    /// A tile on one of the library's own published shelves. `SHELF_F` is which shelf, `SHELF_C`
    /// the column inside it.
    Shelf,
    /// The grid's own control row — Sort and Filter, under the grid heading and inside the scroll.
    /// This was the fixed toolbar at `TOOL_Y`; the name survived the move.
    Toolbar,
    Grid,
    /// The A–Z letter rail on the right edge (RIGHT from the grid's last column). Jump, never
    /// filter: picking a letter moves focus/scroll to its first title (Emby semantics). Only
    /// reachable while [`rail_on`] — the unfiltered ascending-title listing.
    Rail,
    /// The failure read-out's **Try again**, which stands where the grid would be. Exactly one
    /// control, so it is a band rather than a cursor; [`sync_readout_focus`] owns when it is
    /// entered and left, because the failure can arrive (and clear) long after [`enter`].
    Status,
}

/// What the Library's CONTENT REGION shows in place of a grid — the projection the screen draws
/// from, and the whole read-out decision in one pure function ([`readout_of`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Readout {
    /// nothing: the grid speaks for itself (including a grid that is merely missing a later page)
    Grid,
    /// the first answer for this query is still out — the spinner
    Loading,
    /// this source did not answer and there is nothing to show for it: verdict, reason, Try again
    Failed,
    /// the source answered, and the answer is nothing
    Empty,
}

/// The read-out, from BOTH fetch states and the store. Pure, because it is the only part of this
/// screen a host can grade and because three of its four answers were previously spread across
/// three `else if`s in the middle of the draw.
///
/// **The SOURCE outranks the section, and both of its terminal answers do — while there is nothing
/// to show.** With no table there is no section to page, so `fetch` is the `unwrap_or(Loading)` of a
/// state that does not exist and `total` is its `unwrap_or(-1)` — i.e. the spinner, from two
/// defaults, which is the eternal spin this whole change is about. It is not enough to catch the
/// FAILED source: one that answered with nothing we browse (a music-only account) is `Ready` with
/// `n_sections == 0` and lands in the same two defaults one case over. That one is `Empty` — it
/// answered — and the read-out says so.
///
/// The `total < 0` gate on both is the ONE thing this rule gained from the multi-source table, and
/// it is a real decision rather than a mechanical one: a source that has stopped answering keeps
/// the sections it had learned and can still have a page store full of items, so "this source is
/// gone" and "there is nothing of it on screen" stopped being the same fact. The read-out wins only
/// when the second is true as well. What that costs is a grid you can still scroll but cannot play
/// from — the posters are cached, the server is not there — and the alternative costs a library
/// blanked to report a server the Sources list is already dimming. The page rule below has made the
/// same trade since before this branch; making the two layers disagree about it would be worse than
/// either answer.
///
/// Two per-section rules are the ones that keep being got wrong. A `Failed` fetch on a section that
/// already HAS items is `Grid`: a mid-scroll page failure must not blank a working screen — the
/// items are still there, the back-off is already counting, and `browse::pump`'s failure branch
/// exists precisely so the store survives. And a `Ready` empty listing is `Empty`, never `Failed`:
/// an empty library is an answer (`StatusKind::Empty`'s rule, stated in `browse::SecFetch` too).
fn readout_of(
    table: crate::browse::SecFetch,
    n_sections: usize,
    fetch: crate::browse::SecFetch,
    total: i64,
) -> Readout {
    use crate::browse::SecFetch;
    // Both of the TABLE's terminal answers, and both gated on there being NOTHING TO SHOW — the
    // same sentence the page rule below is gated on, and the merge is what made that matter. On the
    // branch this came from, a failed table meant no sections at all, so "failed" and "nothing to
    // show" were the same fact. They are not any more: a source that stops answering KEEPS every
    // section it had learned (`BrowseSource::reachable`'s rule, the one the Sources list dims a
    // group by), and its page store can still hold the items you were just looking at. Blanking
    // those to say "can't reach" is the bug the page rule exists to prevent, one layer up.
    if total < 0 {
        match table {
            SecFetch::Failed => return Readout::Failed,
            SecFetch::Ready if n_sections == 0 => return Readout::Empty,
            _ => {}
        }
    }
    match fetch {
        SecFetch::Loading => Readout::Loading,
        SecFetch::Failed if total < 0 => Readout::Failed,
        SecFetch::Failed => Readout::Grid,
        SecFetch::Ready if total == 0 => Readout::Empty,
        SecFetch::Ready => Readout::Grid,
    }
}

/// [`readout_of`] against the live store — or, on an automated boot, against [`FailTest`].
fn readout() -> Readout {
    match fail_test() {
        Some(FailTest::Dead) | Some(FailTest::Own) | Some(FailTest::Table) => {
            return Readout::Failed
        }
        Some(FailTest::Empty) => return Readout::Empty,
        None => {}
    }
    let kind = unsafe { WANTED_KIND };
    if crate::browse::tab_section(wanted_tab()).is_none() {
        return readout_of(
            crate::browse::kind_state(kind),
            0,
            crate::browse::SecFetch::Loading,
            -1,
        );
    }
    readout_of(
        crate::browse::cur_source_state(),
        crate::browse::section_count(),
        crate::browse::fetch_state(),
        crate::browse::total(),
    )
}

fn wanted_tab() -> usize {
    match unsafe { WANTED_KIND } {
        crate::browse::SecKind::Movie => 0,
        crate::browse::SecKind::Show => 1,
    }
}

/// `/tmp/plxnative-libfail` — force one read-out state, for the device pass.
///
/// This screen's failure is the one state in the app that **cannot be reached on purpose**: it
/// needs a server that refuses to answer. The honest way is `tools/netcond.py` scoped to the page
/// request, and that stays the real test — but it is three moving parts (a proxy, a recompiled
/// `PMS_PORT`, and a macOS firewall prompt that silently swallows the TV's connections until it is
/// allowed once), and every one of them fails looking exactly like "the app is fine". This makes
/// the READ-OUT reachable with a file, so a capture grades the drawing rather than the plumbing.
///
/// It forces the projection only. Nothing downstream is faked: `browse` keeps whatever state it
/// really has, Try again still re-kicks a real fetch, and the moment the file is gone the screen is
/// back on [`readout_of`]. Behind `devtriggers` like every other trigger, so it does not exist in a
/// `RELEASE=1` build.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FailTest {
    /// (empty file, or `dead`) a BORROWED source that did not answer — the design's case, with the
    /// reason line drawn. The owner is the REAL one whenever the browsed source has a handle; the
    /// stand-in is only the fallback for a one-server account, where the three-block read-out has
    /// no way to reach a panel at all and a capture is the only thing that grades its layout.
    Dead,
    /// `own` — the same failure on YOUR OWN server: verdict and action, no reason line, because
    /// there is no owner to name.
    Own,
    /// `table` — the failure one layer up, a section list that could not be fetched.
    Table,
    /// `empty` — reachable and empty: a statement, no Retry, no danger tint.
    Empty,
}

fn fail_test() -> Option<FailTest> {
    // Read ONCE. `readout()` is asked several times a frame, and a `stat` per call on a path in the
    // shared `/tmp` is not something to put in a draw loop.
    static ONCE: std::sync::OnceLock<Option<FailTest>> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        crate::dev::read("libfail").map(|v| match v.as_str() {
            "own" => FailTest::Own,
            "table" => FailTest::Table,
            "empty" => FailTest::Empty,
            _ => FailTest::Dead,
        })
    })
}
/// Which menu is open — **and, inside the variant, the key that menu was built from.**
///
/// The key belongs here because it means nothing anywhere else: it describes ONE panel, the one on
/// screen. [`close_menu`] assigns `Menu::None`, which drops the key with the panel it described, so
/// there is no state left over for a reader to pick up and no build-time record that can outlive
/// the rows it was recorded from. A menu that has no landing to wait for carries no key at all —
/// see [`Filter`](Menu::Filter).
///
/// Not `Copy` (the Sources panel's action list is a `Vec`), so [`menu`] hands out a borrow; the
/// discipline that comes with that is written there.
#[derive(Debug)]
enum Menu {
    None,
    /// The **Sources list** — a PICKER over the FAVOURITE libraries of the type being browsed
    /// (`browse::source_rows`). One level, `Level::Browse`; the *On Home* level it used to swap to
    /// belongs to the Favorite libraries route now (see [`Level`]).
    Source {
        /// The section-table generation ([`crate::browse::sections_gen`]) this panel was built
        /// from — its staleness key, and deliberately NOT a row count.
        ///
        /// Sort and Genre can key on a count because the list they are built from is the list they
        /// show. The Sources panel's rows are `browse::source_rows()`, which is **kind-filtered** —
        /// only the libraries of the tab's own type — while the obvious count beside it,
        /// `section_count()`, counts every kind. Those two are equal only on an account with a
        /// single kind of library, so on any ordinary one (Movies *and* TV Shows) the guard never
        /// converged and the panel rebuilt itself — `source_groups()` + `source_rows()` String
        /// clones and a full `set_sections()` — on EVERY frame it was open, which is exactly the
        /// state this screen's own instrumentation measured as fill-rate-bound (`menu.panel`
        /// 16–42 ms). A generation cannot be compared against the wrong quantity, because it is not
        /// a count of anything: `browse::append_sections` bumps it when a source's libraries land,
        /// and that landing is the whole reason this rebuild exists.
        gen: u32,
        /// One action per global row index (see [`SrcAction`]) — including the separator, which
        /// does nothing and can never be focused, but still occupies an index.
        acts: Vec<SrcAction>,
    },
    /// Sort by — the value list `browse::sorts()`.
    Sort {
        /// Row count this menu was built from (0 = the "Loading…" row). [`menu_stale`] rebuilds it
        /// in place when the live list stops matching, and [`menu_commit`] gates OK on it so a
        /// press on "Loading…" cannot resolve to a value that is not there.
        built: usize,
    },
    /// Filter — the unwatched switch and the way in to Genre.
    ///
    /// **No key**, because its rows are static: they are built from the `view_*` resolvers rather
    /// than from a server list, so there is no landing they could be stale against and nothing to
    /// rebuild in place ([`menu_stale`] says the same in its `false` arm).
    Filter,
    /// The Genre value list, drilled into from Filter.
    Genre {
        /// Row count this menu was built from — `Sort`'s key, over `browse::genres()`.
        built: usize,
    },
}

/// The toolbar's chips, as a TYPED list rather than positions.
///
/// The Source chip is drawn only when there is more than one source, so a chip's index is not its
/// identity: with one server the row is `[Sort, Filter]` and with two it is `[Source, Sort,
/// Filter]`. Every activation matches on the CHIP. A `_` catch-all here is the specific bug this
/// enum exists to prevent — inserting a chip at index 0 silently made Source open the Sort menu,
/// because "everything that isn't 0" was Filter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Chip {
    Source,
    Sort,
    Filter,
}

/// What an OK / click means for app.rs (everything else is consumed internally).
pub(crate) enum Action {
    None,
    /// The Home tab was picked — route back to Home.
    GoHome,
    /// The Search pill was picked. A separate arm rather than a `GoHome`-with-a-pill, because the
    /// strip's last pill is not a library and `switch_tab` would resolve it to a section that does
    /// not exist — silently, since `tab_section` clamps.
    GoSearch,
    /// A grid card was activated — open its detail page (app.rs owns routing).
    Card,
    /// **A SHELF tile was activated**, and it carries what the grid's `Card` never had to: the
    /// item itself, and whether the shelf it came from is the library's Continue Watching deck.
    ///
    /// Both halves are load-bearing and neither is derivable upstream. The item, because a shelf
    /// tile is not in the grid's paged store and `focused_item` cannot reach it. And the deck flag,
    /// because it decides two different things: a deck tile RESUMES rather than opening the detail
    /// page, and a press-and-hold on one must pass `from_deck=true` to `item_menu`, which is what
    /// makes *Remove from Continue Watching* appear at all. Library's activation hardcoded
    /// `from_deck=false` for as long as it had no deck to show.
    ShelfCard { from_deck: bool },
}

static mut AREA: Area = Area::Grid;
/// **The focused pill while `AREA == Tabs`, as an IDENTITY rather than a position.**
///
/// It was a `usize`, and that was safe for exactly as long as the strip was a permanent four. Once
/// the favourite switch can add or remove a type pill mid-session (`browse::tab_has_favorite`), a
/// stored index silently changes what it MEANS: with the strip at `[Home, Search]` and the cursor
/// on 1, a Movies library landing makes 1 mean Movies — the capsule stays where it is and OK
/// navigates somewhere the user never chose. Codex review, 2026-09-05; the plan called for this
/// audit and the first pass did only the `Pill::Section` half of it.
///
/// Never read raw: [`tab_idx`] resolves it against the strip that is DRAWN, so a pill that has
/// disappeared falls back to Home rather than to whatever now occupies its slot.
static mut TAB_P: Pill = Pill::Home;
/// Where the toolbar cursor was last WALKED to. Never read directly — the row it indexes changes
/// length without input, so every reader goes through [`tool_f`], which clamps it to the row that
/// is drawn. (It is not a chip identity either: the row is two chips long with one source and
/// three with two — see [`Chip`].)
static mut TOOL_F: usize = 0;
/// Which of the library's published shelves holds focus, and which column inside it. Index-based,
/// with [`shelf_focus_survives`] re-resolving them whenever the shelf SET changes underneath —
/// which on this endpoint is routine rather than exotic (`docs/pms-api.md` §3a: two hubs rotate
/// their subject between requests).
static mut SHELF_F: usize = 0;
static mut SHELF_C: usize = 0;
/// The identity of the tile focus is standing on — `(hub id, rating key)`, so a shelf reordering
/// under focus keeps the cursor on the same FILM rather than on the same slot.
static mut SHELF_ID: Option<(String, String)> = None;
/// One [`card_row::CardRow`] per shelf — the SAME component Home's shelves and the detail page's
/// Related row are built from, so the focus pop, the horizontal scroll spring, the heading lift and
/// the label band are the shared ones rather than a fourth hand-rolled copy.
static mut SHELF_ROWS: [card_row::CardRow; MAX_SHELVES] =
    [const { card_row::CardRow::new() }; MAX_SHELVES];
/// The document may draw at most this many shelves — [`crate::browse::section_hubs`]' own cap,
/// named here because the row array is sized by it.
const MAX_SHELVES: usize = 12;
static mut GR: usize = 0; // grid focus row
static mut GC: usize = 0; // grid focus col
static mut SCROLL: Spring = Spring::at(0.0);
static mut FOCUS_S: Spring = Spring::at(1.0); // focused cell pop
static mut PREV_S: Spring = Spring::at(1.0); // previously-focused cell shrinking back
static mut PREV_IDX: i64 = -1;
/// **The page ground** — one ambient wash keyed to the item under the ring, wherever the ring is.
/// [`focused_item`] already answers that across both halves of the document, so the shelves and the
/// A-Z wall key the same ground and there is nothing here that knows which one focus is standing in.
///
/// A tile with no `UltraBlurColors` envelope HOLDS the ground rather than clearing it — the rule is
/// [`PageGround`]'s, and it matters most here: a poster wall is where a single artless item sits
/// between two coloured ones, and a page that flashed grey on the way past would read as a fault.
static mut GROUND: PageGround = PageGround::new();
/// The open menu, **and its build-time key** — the two are one value, see [`Menu`].
static mut MENU: Menu = Menu::None;
/// The chip menus' one popover. `caching_host` since 2026-09-03: the grid under an open Sort/Filter/
/// Source menu is a full page of poster tiles, a glass toolbar and a scrim, and it was re-rendered
/// under the panel on every frame the menu's own selection moved — measured `fps:library-switch`
/// at a 16 fps median with the panel up against the profile menu's 60 over the same kind of page.
/// The three guards that go with it: `host::live` in [`draw`]'s menu block, `own_motion` around
/// the menu's springs in [`update`], `note_own_damage` beside its selection moves.
static mut POP: Popover = Popover::new().caching_host();
static mut TABLE: TableView = TableView::new();
static mut PHASE_MS: f32 = 0.0; // spinner phase accumulator
/// The failure read-out's Try again pill, recorded at draw for the pointer (the `TOOL_RECTS` /
/// `RAIL_RECTS` idiom). Parked OFF the panel rather than at the origin, because `Rect::contains`
/// is inclusive and a zero-size rect at (0,0) would "contain" a click at exactly (0,0).
static mut STATUS_BTN: Rect = Rect::new(-1.0, -1.0, 0.0, 0.0);
/// Chip rects for pointer hit-testing, in [`chips`] order. The row is two or three long, so the
/// unused slot stays a zero rect — and every scan tests `w > 0.5` first, because a zero rect at the
/// origin `contains` the origin.
static mut TOOL_RECTS: [Rect; 3] = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];

// ---- the grid's content cross-fade + its deferred reload -------------------------------------
/// A grid reload the user has asked for but which has NOT been applied to the store yet: the
/// fade-out is running and [`Xfade::tick`] will hand it back on the floor frame. Typed and `Copy`
/// rather than a boxed closure — one value, zero alloc, and the newest request simply overwrites
/// the one before it (a fast double tab-switch must commit ONCE, to the last section pressed;
/// mixing two DIFFERENT switches inside one 70 ms fade drops the earlier one, which is the honest
/// reading of "the user changed their mind mid-press").
///
/// Every variant that can be pressed twice while its menu stays open carries its TARGET, not a
/// verb: [`Pending::Unwatched`]'s two presses inside one fade must cancel out, and a bare "toggle"
/// marker cannot express that — the chip would relabel on the first press and then refuse to
/// relabel back. `Sort` and `Genre` close their menu on the press, so they cannot be re-pressed
/// inside the window and stay plain picks.
#[derive(Clone, PartialEq, Debug)]
struct Pending {
    /// The library to arrive in, when a page transition is queued.
    /// The section to switch to, WITH the table epoch it was chosen against. The epoch is not
    /// decoration: a bare index is not a name. `browse::reset` clears and renumbers the table on a
    /// profile switch, so a switch queued before one and flushed after it lands on whatever library
    /// now happens to hold that number — a different person's. The grid half carried its epoch from
    /// the start and this one did not, which is the asymmetry a Codex pass found.
    section: Option<SecReq>,
    /// **A GRID action, SCOPED to the section it will be applied to and stated SEMANTICALLY.**
    ///
    /// Both halves are load-bearing and both were wrong when this was a bare index. The Library
    /// keeps its grid controls live during a page transition — a control that ignores a press is
    /// worse than one that acts a beat later — so a Sort chosen in library A can commit against
    /// library B. Sort and genre menus are built per section from THAT section's own server
    /// vectors, so an index means a different value there, or silently nothing. The `sec` is what
    /// lets [`apply_pending`] refuse an action whose target is no longer the section it named, and
    /// the epoch beside it is what makes that refusal survive a `browse::reset` renumbering the
    /// table under both.
    grid: Option<GridReq>,
}

/// **The two are ONE transaction and both fields are held at once, which is what this used to get
/// wrong.** `Pending` was an enum, so a Sort pressed while a page transition was in flight
/// REPLACED it: the section was never applied, and the grid action was then rejected too, because
/// its target was not the section that (still) held the cursor. Both operations were silently lost
/// by one press. The section commits first and the action is applied to the library it named.
/// A queued SECTION switch and the table it was chosen against. See [`Pending::section`].
#[derive(Clone, Copy, PartialEq, Debug)]
struct SecReq {
    sec: usize,
    epoch: u32,
}

#[derive(Clone, PartialEq, Debug)]
struct GridReq {
    sec: usize,
    /// [`crate::browse::table_epoch`] — the table's IDENTITY, not `query_gen`. It was the latter,
    /// which moves on every ordinary re-query, so it could not be checked on its own; the guard
    /// read `epoch != … && sec != cur()` and the epoch half was therefore dead. A `browse::reset`
    /// renumbers the table, so index 0 after one is a DIFFERENT profile's library.
    epoch: u32,
    act: GridAct,
}

impl Pending {
    const NONE: Pending = Pending {
        section: None,
        grid: None,
    };
    fn is_none(&self) -> bool {
        self.section.is_none() && self.grid.is_none()
    }
}

/// A grid action in the terms the SERVER uses, so it survives being carried to another section.
#[derive(Clone, PartialEq, Debug)]
enum GridAct {
    /// the stable sort KEY plus the direction the user chose — never an index, and never a toggle
    Sort { key: String, desc: bool },
    /// the desired value, never a flip: two presses inside one fade must collapse to one answer
    Unwatched(bool),
    /// the stable tag ID (`None` = All genres) — never an index
    Genre(Option<String>),
}
static mut PENDING: Pending = Pending::NONE;
/// The grid's content cross-fade. The grid, its focused label, the empty line, the item count and
/// the A–Z rail all draw under its alpha; the top chrome (scrim, chip, pills, toolbar, spinner,
/// menus) does NOT — it is what persists across the swap.
static mut XF: Xfade = Xfade::new();
/// Last `browse::query_gen()` we have accounted for — the watchdog that catches a store replaced
/// by something OTHER than this screen (`browse::reset()` on a profile switch), so that is faded
/// rather than cut and the fader can never be left parked at 0 by a swap it never scheduled.
static mut EPOCH: u32 = 0;
static mut WANTED_KIND: crate::browse::SecKind = crate::browse::SecKind::Movie;

fn xf() -> &'static mut Xfade {
    unsafe { &mut *addr_of_mut!(XF) }
}
/// The queued action, **CLONED**.
///
/// It was `addr_of!(PENDING).read()`, a bitwise copy, which was correct for exactly as long as
/// `Pending` was `Copy`. It owns a `String` now (the stable sort key and genre id a scoped action
/// carries), so a bitwise read duplicates ownership and the second drop is a double free — a
/// SIGTRAP with no panic message, which is what this cost to find.
fn pending() -> Pending {
    unsafe { (*addr_of!(PENDING)).clone() }
}

/// **What a queued transition actually REPLACES** — the partial-update axis, and the reason a Sort
/// no longer rebuilds the whole page.
///
/// This screen is two independent blocks stacked in one document: the library's own published
/// shelves (`browse::section_hubs`, above) and its A–Z grid (`browse`'s paged store, below). A
/// SECTION change replaces both. A sort, a genre or the unwatched switch replaces only the grid —
/// the chip is the same library and the shelves are the same shelves — and fading them out and back
/// in said otherwise: the whole column dissolved and rebuilt for a change that touched one block,
/// which is what "sorting reloads the entire page" describes.
///
/// So the fader's alpha is applied per BLOCK rather than to the page, and which blocks it reaches
/// is the scope of the transaction that armed it. There is no second fader and no second `Pending`
/// — the scope is a property of the request that already exists, which is what stops the two from
/// ever disagreeing about what is being replaced.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    /// The whole document: a different library.
    Page,
    /// The grid block alone — its heading, its control row and its rows.
    Grid,
}

/// The scope of the fade CURRENTLY RUNNING. Not read off [`PENDING`], which is emptied at the
/// floor: the fade-in half has nothing queued and would otherwise read as `Grid` and pop the
/// shelves back to full strength halfway through a page transition. Set when a fade is armed and
/// left alone until the next one is.
static mut SCOPE: Scope = Scope::Page;

/// Arm the fader at `sc`. The one door, so a request cannot start a fade without saying what it
/// replaces.
fn fade(sc: Scope) {
    unsafe { SCOPE = sc };
    xf().reload();
}

/// Re-mount the fader for a whole-page replacement — route entry, a foreign `browse::reset`, a tab
/// press with no section behind it. Always [`Scope::Page`]: there is no partial answer to "the
/// store under this screen was replaced by something that is not this screen".
fn remount() {
    unsafe { SCOPE = Scope::Page };
    xf().mount();
}

/// The alpha a block of `sc` scope should be drawn at this frame: the fader's for the blocks being
/// replaced, full strength for the ones that are not.
fn block_alpha(sc: Scope) -> f32 {
    if sc == Scope::Grid || unsafe { addr_of!(SCOPE).read() } == Scope::Page {
        xf().alpha()
    } else {
        1.0
    }
}

/// Queue a reload and start the fade-out. The newest request WINS — see [`Pending`].
fn request_section(i: usize) {
    unsafe {
        // **A queued grid action does not travel to a NEW target.** `A → B`, queue `Sort(B)`, then
        // supersede with `C`: B's action is discarded rather than applied to B off-screen or
        // silently retargeted at C. An action already aimed at `i` survives — that is the ordinary
        // "press Sort while the page is still arriving" case, not a supersede.
        let keep = (*addr_of!(PENDING))
            .grid
            .clone()
            .filter(|g| g.sec == i);
        PENDING = Pending {
            section: Some(SecReq {
                sec: i,
                epoch: crate::browse::table_epoch(),
            }),
            grid: keep,
        };
    }
    fade(Scope::Page);
}

/// Queue a GRID action, scoped to the section it was chosen in. The one door for Sort, Filter and
/// the unwatched switch.
///
/// **The precedence rule lives here**, and it is the row of §4's table that was easiest to get
/// wrong: a grid action arriving while a SECTION transition is in flight does not supersede it. The
/// page commits first and the action is carried to the TARGET, which is safe only because the
/// action is semantic and scoped — a bare `Sort(2)` would have selected a different value there.
fn request_grid(act: GridAct) {
    // a page transition in flight: the action belongs to the library we are ARRIVING in, and the
    // transition itself is left standing beside it rather than replaced
    let target = pending().section.map(|r| r.sec).unwrap_or_else(crate::browse::cur);
    let page = pending().section.is_some();
    unsafe {
        (*addr_of_mut!(PENDING)).grid = Some(GridReq {
            sec: target,
            epoch: crate::browse::table_epoch(),
            act,
        });
    }
    // **The SCOPE of the fade is the scope of the transaction.** A grid action arriving during a
    // page transition does not shrink it to the grid — the page is still being replaced.
    fade(if page { Scope::Page } else { Scope::Grid });
}

/// Withdraw a queued action without applying it — BACK before the fade's floor.
///
/// **A page transition, its target-scoped menu and its queued grid action are ONE transaction.**
/// Queueing a semantic action created a way to strand one, so cancelling discards the action AND
/// closes the menu it was built for: BACK already gives an open menu precedence, and the unwatched
/// switch deliberately leaves its Filter menu open after the press, so a menu really can outlive
/// the section it was built for.
fn cancel_pending() -> bool {
    if pending().is_none() {
        return false;
    }
    take_pending();
    xf().cancel();
    if menu_open() {
        close_menu();
    }
    true
}

/// **Is the DOCUMENT hidden right now** — the half of a staged shelf commit's permission that does
/// not depend on where the viewer is standing.
///
/// Only a PAGE-scoped fade hides it. This read every `Xfade` as a hidden page, which is wrong for
/// exactly the scope added beside it: a `Scope::Grid` fade dips the grid block alone and
/// deliberately holds the library chip and the shelves at full alpha — and those are precisely what
/// a shelf commit moves. A staged refresh could therefore publish in the middle of a sort, changing
/// the shelf count and the pitches on screen and re-seating focus to the head, which is the reflow
/// staging exists to prevent.
fn hidden_page() -> bool {
    (xf().is_swapping() && unsafe { addr_of!(SCOPE).read() } == Scope::Page)
        || crate::ui::nav::page_alpha() < 0.99
}

/// **May a staged shelf set be published RIGHT NOW?** The whole permission, in one place, so the
/// draw and the tests read the same rule.
///
/// Two cases, and they are different situations rather than one predicate. HIDDEN ([`hidden_page`])
/// — the document is not on screen, so nothing can be SEEN to move. VISIBLE — the scroll is settled
/// at the head AND no press is armed.
///
/// That second term is not decoration. A commit re-resolves focus onto the post-commit layout's
/// first content zone, and [`at_document_head`] cannot tell "the head of a composed page" from
/// "grid row 0 of a page that has no chip and no shelves yet": with neither drawn, row 0's own
/// reveal IS zero. The viewport does not move in that case — the arriving first shelf reveals at 0
/// too — so the visible effect is the ring stepping from the first poster onto the first shelf of a
/// page that has just finished composing itself, which is accepted; the alternative starves a
/// single-library household's shelves until it navigates away and back. What is NOT acceptable is
/// doing it under an armed press, whose deferred activation would then resolve against the zone
/// focus was moved TO rather than the card that was pressed. `ui::press`' "focus cannot move
/// mid-press" contract, one more time.
fn may_publish_shelves() -> bool {
    hidden_page() || (at_document_head() && !crate::ui::press::is_live())
}

/// **A store replaced by something other than this screen** — `browse::reset()` on a profile
/// switch, or the favourites editor re-pointing `cur()` while the Library is frozen under Settings.
///
/// Three things it does that the older inline version did not, each a Codex finding:
///
/// * **It acts during a fade.** It used to remount only `if !xf().is_swapping()`, which was how it
///   told our own commit from a foreign one — and the price was that a full-document change
///   arriving inside a grid reload was absorbed with no remount at all. [`apply_pending`] accounts
///   for its own bump now, so this can be unconditional.
/// * **It promotes to PAGE scope.** A foreign change replaces the whole column, not the grid, so a
///   running `Scope::Grid` fade is the wrong shape for it; [`remount`] sets `Scope::Page`.
/// * **It restores the NEW section's own coordinates.** It only faded before, so the incoming
///   library was mounted using the outgoing one's focus and scroll.
///
/// Called twice a frame — before the pump and after it — because the pump can reset the roster,
/// append sections and re-point `cur()`, and a change that lands there would otherwise reach that
/// frame's draw before anything noticed.
fn watch_foreign_store() {
    let g = crate::browse::query_gen();
    if g == unsafe { addr_of!(EPOCH).read() } {
        return;
    }
    unsafe { EPOCH = g };
    // **The queued transaction goes with the document it was queued against.** A section switch and
    // its grid action both name the OLD table; `apply_pending`'s epoch guards would reject them one
    // at a time, but leaving them queued spends the next fade floor on a rejection instead of on
    // the page that is actually arriving. Cancelling says it once.
    take_pending();
    restore_view();
    seat_from_restored_view();
    remount();
}

/// Seat the focus band on the block the RESTORED scroll is showing. Shared by [`enter`] and by
/// [`watch_foreign_store`], so a library arrived at and a library swapped in underneath are put on
/// screen the same way.
fn seat_from_restored_view() {
    let lay = layout();
    let scroll = unsafe { (*addr_of!(SCROLL)).pos };
    if let Some((a, i)) = lay.seat_for_scroll(scroll, unsafe { addr_of!(GR).read() }) {
        enter_zone(&lay, a, 1);
        match a {
            Area::Shelf => unsafe { SHELF_F = i },
            // the ROW the restored scroll is showing, not whatever row was last walked — see
            // `seat_for_scroll`'s note on the toolbar/row-0 coordinate they share
            Area::Grid => unsafe {
                GR = i;
                GC = addr_of!(GC).read().min(COLS - 1);
            },
            _ => {}
        }
        clamp_focus();
    }
}

/// Apply the queued reload. Called on exactly one frame — [`Xfade::tick`]'s commit frame — and by
/// [`flush`]. Every scroll/focus teleport ([`grid_reset`], [`restore_view`]) happens HERE, at
/// alpha 0, so the jump is never on screen.
fn apply_pending() {
    // **Everything below is OURS, so account for the generation it moves.** The watchdog's whole
    // job is spotting a store replaced by somebody else, and it used to tell ours apart by asking
    // whether a fade happened to be running — true after our own commit, false after a foreign
    // reset. That worked and cost the watchdog every other tool it needed: it could not fire during
    // a fade at all, so a full-document change absorbed inside a grid reload went unnoticed. Owning
    // the bump here is the direct statement, and it frees the watchdog to act unconditionally.
    let account = |_: ()| unsafe { EPOCH = crate::browse::query_gen() };
    let p = take_pending();
    // **The SECTION first, so the grid action lands in the library it named.** They are one
    // transaction; applying the action against the section that is still on screen — or dropping
    // it because that section is not its target — is what the enum shape used to do.
    if let Some(SecReq { sec, epoch }) = p.section {
        // the same guard the grid half has always had: an index chosen against a table that has
        // since been cleared and renumbered is not a name, it is a number
        if epoch != crate::browse::table_epoch() {
            account(());
            return;
        }
        apply_section(sec);
    }
    if let Some(GridReq { sec, epoch, act }) = p.grid {
        {
            // **The scope check, by TABLE EPOCH and by index, and BOTH reject on their own.**
            // `browse::reset` clears and redefines the table's indices, so a stale index would
            // otherwise name a different profile's library. The epoch half used to be `&&`-ed with
            // the index half, which made it dead: a reset that happens to leave the cursor on the
            // same NUMBER passed the guard, and index 0 after a profile switch is exactly that case.
            if epoch != crate::browse::table_epoch() || sec != crate::browse::cur() {
                account(());
                return;
            }
            let landed = match act {
                GridAct::Sort { key, desc } => crate::browse::set_sort_by_key(&key, desc),
                GridAct::Unwatched(v) => {
                    // the target, re-checked against the store: two presses inside one fade
                    // collapse to a no-op, and a no-op must NOT re-query (nor reset the scroll —
                    // nothing changed)
                    if crate::browse::unwatched() != v {
                        crate::browse::toggle_unwatched();
                        true
                    } else {
                        false
                    }
                }
                GridAct::Genre(id) => crate::browse::set_genre_by_id(id.as_deref()),
            };
            if landed {
                grid_reset();
            }
        }
    }
    account(());
}

/// Take the queued action, leaving nothing behind. The ONE place `PENDING` is cleared, so a
/// cancelled transition and a committed one cannot end up spelled differently.
fn take_pending() -> Pending {
    unsafe { std::mem::replace(&mut *addr_of_mut!(PENDING), Pending::NONE) }
}

/// Apply a queued reload NOW, without waiting for the fade — for the paths that LEAVE the screen
/// mid-fade. Without this, a sort picked and then BACKed out of within 70 ms is silently dropped
/// while the toolbar chip — which reads the queued intent, see [`view_sort_label`] — has already
/// relabelled itself. The chrome lying about the listing is worse than the original no-fade cut.
/// Re-mounts the fader so the (now different) content dissolves in rather than cutting.
fn flush() {
    if !pending().is_none() {
        apply_pending();
        remount();
    }
}

// ---- what the CHROME shows while a reload is queued -------------------------------------------
// The grid's data swap is deferred to the fade floor, but a CONTROL must acknowledge a press on
// the frame it happens. So the pills/chips resolve through the queued [`Pending`] when there is
// one and through the committed store otherwise. There is exactly ONE pending value, so the chip,
// the pill and the menu's own `On`/`Off` read-out can never disagree with each other — and none of
// them touches `browse`'s query state, so the store stays coherent for the whole fade.
//
// Do NOT "fix" the 70 ms by committing the query state early and deferring only the store wipe:
// `browse::maybe_spawn` scans the OLD listing for holes and `pump` splices the reply in at
// `st.items[start + k]`, so flipping `st.unwatched` without clearing the store opens a window in
// which a FILTERED page lands at UNFILTERED offsets.

/// The section the chrome should read as current — the queued one while a tab switch is in flight.
fn view_section() -> usize {
    pending()
        .section
        .map(|r| r.sec)
        .unwrap_or_else(crate::browse::cur)
}
/// The unwatched-filter state the Filter menu's trailing `On`/`Off` read-out and the Filter chip's
/// value should show — the QUEUED one while a switch is in flight. The switch row is rebuilt from
/// this the frame OK lands, so the word flips under the finger while `browse` is still tearing the
/// listing down and re-querying it; there is no mark in the leading column to flip instead.
fn view_unwatched() -> bool {
    match pending_grid() {
        Some(GridAct::Unwatched(v)) => v,
        _ => crate::browse::unwatched(),
    }
}
/// The Sort chip's value token.
fn view_sort_label() -> &'static str {
    match pending_grid() {
        Some(GridAct::Sort { key, .. }) => crate::browse::sorts()
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.title.as_str())
            .unwrap_or("Title"),
        _ => crate::browse::sort_label(),
    }
}
/// The Filter chip's value token.
fn view_filter_label() -> &'static str {
    match pending_grid() {
        Some(GridAct::Genre(None)) => "All",
        Some(GridAct::Genre(Some(id))) => crate::browse::genres()
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.title.as_str())
            .unwrap_or("All"),
        _ => crate::browse::filter_label(),
    }
}

/// The queued grid action, if one is in flight — **and only while its TARGET is the section on
/// screen**. That scope is what makes the chip read-outs honest during a page transition: what the
/// user is looking at while the page fades is the library they are arriving in, so an action queued
/// against a different one must not relabel this one's chips.
fn pending_grid() -> Option<GridAct> {
    pending()
        .grid
        .filter(|g| g.sec == crate::browse::cur())
        .map(|g| g.act)
}

/// What the **Filter chip** reads — every active filter, not just the genre: `All`, `Comedy`,
/// `Unwatched`, or `Comedy · Unwatched`.
///
/// It folds both because the Filter MENU now owns both switches. The unwatched filter used to have
/// its own capsule on this toolbar, so the chip beside it only ever had to name the genre; with that
/// capsule gone the chip is the only thing on screen that can say the grid is filtered at all, and a
/// filter with no visible sign of itself is a library that has silently lost half its titles.
/// Middle dots separate the active ones, the same way every metadata run in the app does.
fn view_filter_value() -> String {
    let genre = view_filter_label();
    match (genre, view_unwatched()) {
        ("All", false) => "All".to_string(),
        ("All", true) => "Unwatched".to_string(),
        (g, false) => g.to_string(),
        (g, true) => format!("{g} \u{b7} Unwatched"),
    }
}
// ---- the toolbar's chip row -------------------------------------------------------------------

/// Does the LIBRARY CHIP head the scroll? **When there is nothing to switch to, none of this is
/// drawn** — not a bare suffix, not an empty slot, not a disabled chip. Absence, not a branch that
/// draws nothing visible.
///
/// **The design says "with one server the chip is not drawn" and this is deliberately WIDER than
/// that.** The maintainer's single server publishes both `Movies` and a second film library: two
/// libraries behind one *Movies* pill, unreachable from each other with no chip, on an account with
/// exactly one server. Server COUNT is the wrong question — what the chip is for is having somewhere
/// else to go, so it asks that directly: **more than one FAVOURITE library of the type being
/// browsed, or the one you are in is BORROWED** (which the chip is also the only thing that names,
/// since the strip's pill says only "Movies").
fn lib_chip_on() -> bool {
    // The original rule, kept as the FIRST term deliberately: the roster is known before any
    // library has landed, so a second server draws the chip from the boot frame rather than popping
    // it in when discovery finishes. Widening this predicate must not make the chrome flicker
    // during startup.
    if crate::browse::sources().len() > 1 {
        return true;
    }
    // …the case the design's "with one server" rule misses: one server, two libraries of this type.
    if crate::browse::source_rows().len() > 1 {
        return true;
    }
    // …and a single BORROWED library still earns the chip: it is the only surface that says whose
    // it is.
    crate::browse::section_sid_is_borrowed(crate::browse::cur())
}
/// The old name, kept while the fixed toolbar's own callers are migrated onto the scroll.
fn source_chip_on() -> bool {
    lib_chip_on()
}

/// **The ordered list of zones that are actually DRAWN this frame**, top to bottom — the
/// projection UP, DOWN, BACK, pointer reachability and the async focus clamp are all derived from.
///
/// The first draft of this screen wrote those transitions out per zone, and parked focus on an
/// undrawn control in two reachable states: an EMPTY library (no grid heading, no Sort, no Filter,
/// no count, no rail — the design draws none of them over a grid with nothing in it) and a
/// shelves-plus-empty-grid library. A ladder has to be right in five places at once; a projection
/// is right in one. `sync_readout_focus` already worked this way for the read-out band and is the
/// model.
///
/// The order IS the vertical walk. LEFT/RIGHT stay per-zone, because they mean something different
/// in each one (pills, shelf columns, `[Sort, Filter]`, grid columns, and RIGHT off the last grid
/// column into the rail).
fn zones(lay: &Layout) -> Vec<Area> {
    // **The profile chip is NOT a rung of its own.** It and the pills are two stops on ONE shared
    // control — the top bar — reached from each other sideways, and DOWN off either must leave the
    // band together or the chip reads as a different control from the pills beside it (which
    // `the_profile_chip_is_the_bars_leftmost_stop` grades directly). `step` maps `Chip` onto `Tabs`
    // before consulting this list, so the bar occupies one rung here.
    let mut z = vec![Area::Tabs];
    if lay.chip {
        z.push(Area::LibChip);
    }
    // **The whole RUN of shelves is one rung, for the same reason the bar is and a harder one.**
    // `Area` names a kind of zone rather than an instance, so N `Area::Shelf` entries are N entries
    // `step_zone` cannot tell apart: `position` finds the first of them whatever shelf focus is on,
    // and DOWN off the last shelf walks back to the first instead of leaving the run. Which shelf
    // the band is on is `move_focus`'s cursor (`SHELF_F`), exactly as which COLUMN it is on is; the
    // projection's job is only to say where the run begins and ends.
    if lay.shelves > 0 {
        z.push(Area::Shelf);
    }
    if lay.status {
        // A failed source stands where the grid would, as ONE control. The chip and the strip above
        // it are the way out, which is why the read-out spends no line saying so.
        z.push(Area::Status);
    } else if lay.grid_head {
        // **The heading and its control row belong to the BLOCK; the grid rung belongs to the
        // ROWS.** Those two used to be one test, which was harmless only because `grid_head` itself
        // required `total() > 0` and `rows` is `ceil(total / 6)` — so the pair could never
        // disagree. `grid_block_reloading` introduces the one state where they do: mid-re-query the
        // store is empty, but the control row the user just pressed is still on screen and still
        // theirs to stand on.
        z.push(Area::Toolbar);
        if lay.rows > 0 {
            z.push(Area::Grid);
            if rail_on() {
                z.push(Area::Rail);
            }
        }
    }
    // …otherwise (loading, empty, or a grid with nothing in it) no heading, no controls, no rail:
    // the page is its chip and its shelves, and that is a complete screen rather than a broken one.
    z
}

/// The zone a vertical step lands on, or `None` at either end of the drawn list. `Rail` is skipped
/// on the vertical walk — it is entered sideways off the grid's last column and left the same way,
/// which is the one zone whose entry is horizontal.
fn step_zone(lay: &Layout, from: Area, dir: i32) -> Option<Area> {
    let z: Vec<Area> = zones(lay).into_iter().filter(|a| *a != Area::Rail).collect();
    let i = z.iter().position(|a| *a == from)?;
    let j = i as i32 + dir;
    (j >= 0 && (j as usize) < z.len()).then(|| z[j as usize])
}

/// Move focus one zone DOWN, doing nothing at the foot of the drawn list. Every zone's DOWN goes
/// through here rather than naming its own destination, which is what stops five arms from
/// disagreeing about what is below them on an empty library.
fn step_down(lay: &Layout) {
    step(lay, 1);
}
/// …and UP, the same way.
fn step_up(lay: &Layout) {
    step(lay, -1);
}
fn step(lay: &Layout, dir: i32) {
    // the bar is one rung; see `zones`
    let from = if area() == Area::Chip {
        Area::Tabs
    } else {
        area()
    };
    let Some(to) = step_zone(lay, from, dir) else {
        return;
    };
    enter_zone(lay, to, dir);
}

/// Put focus IN a zone, seating whatever cursor that zone owns. Entering from ABOVE lands on the
/// zone's first stop, from BELOW on the one nearest where you left — the ordinary television rule,
/// and the reason this is one function rather than an assignment at each call site.
fn enter_zone(lay: &Layout, to: Area, dir: i32) {
    unsafe {
        AREA = to;
        match to {
            Area::Tabs => set_tab_p(own_pill_id()),
            // the control row's FIRST stop, which is not index 0 when the head chip shares the array
            Area::Toolbar => TOOL_F = control_lo(chips()),
            Area::Shelf => {
                // walking DOWN into the run of shelves lands on the first, walking UP on the last
                SHELF_F = if dir > 0 {
                    0
                } else {
                    lay.shelves.saturating_sub(1)
                };
                SHELF_C = SHELF_C.min(shelf_len(SHELF_F).saturating_sub(1));
                note_shelf_id();
            }
            _ => {}
        }
    }
}

/// **Which tile of shelf `to` sits under the one focused in shelf `from`** — the shared
/// [`card_row::column_near_x`] rule, applied to this screen's two shelf rows.
///
/// The walk used to carry the COLUMN INDEX across and clamp it, which is right only while both rows
/// hold the same horizontal scroll. They routinely do not: walk one shelf sideways until it
/// scrolls, press DOWN, and the index lands on a tile that can be most of a screen away — after
/// which `scroll_into_view` drags the destination row hard to reveal it, so a vertical press reads
/// as the page lurching sideways. Directional navigation follows where the user SEES a tile.
fn shelf_column_near(from: usize, col: usize, to: usize) -> usize {
    let rows = unsafe { &*addr_of!(SHELF_ROWS) };
    let (fs, ts) = (shelf_style(from), shelf_style(to));
    let x = card_row::tile_centre_x(col, MARGIN_X, fs.w + fs.gap, fs.w, rows[from].scroll_x());
    card_row::column_near_x(
        x,
        MARGIN_X,
        ts.w + ts.gap,
        ts.w,
        rows[to].scroll_x(),
        shelf_len(to),
        col,
    )
}

/// **Shelf `i`'s style** — the ONE answer, read by the draw, the update, the hit test, the focus
/// anchor and the vertical walk alike. A row of episodes is landscape and everything that reasons
/// about where its tiles are has to know that, or the picture and the pointer disagree.
fn shelf_style(i: usize) -> &'static RowStyle {
    let landscape = crate::browse::section_hubs::shelves(crate::browse::cur())
        .get(i)
        .map(|s| s.landscape)
        .unwrap_or(false);
    if landscape {
        &RowStyle::EPISODE
    } else {
        &RowStyle::HOME
    }
}

/// How many tiles shelf `i` holds. Zero for a shelf that is not there, which is the answer every
/// clamp below wants.
fn shelf_len(i: usize) -> usize {
    crate::browse::section_hubs::shelves(crate::browse::cur())
        .get(i)
        .map(|s| s.items.len())
        .unwrap_or(0)
}

/// Record WHICH FILM the shelf cursor is standing on, so a shelf that changes underneath can put
/// focus back on the same one rather than the same slot.
///
/// It has to be an identity rather than an index because on this endpoint a shelf changing under
/// the cursor is ROUTINE: `docs/pms-api.md` §3a measured the genre and actor/director rows rotating
/// their subject on every request, and Mark Watched and *Remove from Continue Watching* both delete
/// a tile out from under the user.
fn note_shelf_id() {
    unsafe {
        SHELF_ID = crate::browse::section_hubs::shelves(crate::browse::cur())
            .get(SHELF_F)
            .and_then(|s| s.items.get(SHELF_C).map(|m| (s.id.clone(), m.rk.clone())));
    }
}

/// Put the shelf cursor back after the shelf SET changed. **Same shelf, same column, clamped**; if
/// the whole shelf is gone, the nearest surviving shelf at the same column; if none survives, the
/// caller's zone clamp takes focus somewhere drawn.
///
/// Identity alone does not cover disappearance, which is why the rule has three rungs rather than
/// one lookup.
fn shelf_focus_survives() {
    let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
    if shelves.is_empty() {
        return;
    }
    unsafe {
        // 1. the same FILM, wherever it moved to
        if let Some((id, rk)) = addr_of!(SHELF_ID).as_ref().and_then(|o| o.clone()) {
            if let Some((si, ci)) = shelves.iter().enumerate().find_map(|(si, s)| {
                (s.id == id)
                    .then(|| s.items.iter().position(|m| m.rk == rk).map(|ci| (si, ci)))
                    .flatten()
            }) {
                SHELF_F = si;
                SHELF_C = ci;
                return;
            }
        }
        // 2. the same shelf, same column, clamped — the tile was deleted but the row survives
        if let Some(s) = shelves.get(SHELF_F) {
            SHELF_C = SHELF_C.min(s.items.len().saturating_sub(1));
            note_shelf_id();
            return;
        }
        // 3. the shelf itself is gone: the nearest surviving one at the same column
        SHELF_F = shelves.len() - 1;
        SHELF_C = SHELF_C.min(shelves[SHELF_F].items.len().saturating_sub(1));
        note_shelf_id();
    }
}

/// Is `a` drawn this frame? The ONE test behind the async clamp — a landing that takes a zone away
/// (a source fails, a library empties, shelves commit) must not leave focus on it for a frame.
fn zone_drawn(lay: &Layout, a: Area) -> bool {
    // the bar is always drawn, and it is one rung in `zones` — so its two stops answer for it
    a == Area::Chip || zones(lay).contains(&a)
}

/// **This frame's [`Layout`], from live state.** Call it once and pass it down; every function that
/// needs a document coordinate takes it as an argument rather than rebuilding one, so the draw and
/// the hit test can never be looking at two different documents in the same frame.
///
/// The shelf count is the COMMITTED set (`section_hubs`), never the staged one — publishing is what
/// moves this geometry, which is the whole reason that module has a publication axis.
fn layout() -> Layout {
    layout_with(|i| unsafe { (*addr_of!(SHELF_ROWS))[i].band_expand() })
}

/// **The document as it will be once the springs settle** — the same shape, with every shelf's
/// label band at its DESTINATION (open on the focused shelf, closed everywhere else) instead of
/// wherever it is this frame.
///
/// Only the scroll TARGET may be computed from this, and it must be: the band and the scroll are
/// two springs at one rate, so a target derived from the live band moves every frame while the
/// band chases it and the row arrives and then drifts. `card_row::settled_top` carries the full
/// argument. Everything that describes what is on screen right now — the draw, the pointer, the
/// visible-row window — reads [`layout`].
fn layout_settled() -> Layout {
    let f = (area() == Area::Shelf).then(|| unsafe { SHELF_F });
    layout_with(|i| (f == Some(i)) as i32 as f32)
}

/// The shared body of the two: `band(i)` gives shelf `i`'s label-band expansion, 0 collapsed → 1.
fn layout_with(band: impl Fn(usize) -> f32) -> Layout {
    let read = readout();
    let sh = crate::browse::section_hubs::shelves(crate::browse::cur());
    let shelves = sh.len();
    // one pitch per shelf, from the shape that shelf is actually drawn in AND how much of its
    // focused label block it is currently reserving
    let mut pitch = [crate::ui::consts::ROW_PITCH; MAX_SHELVES];
    for (i, (slot, s)) in pitch.iter_mut().zip(sh.iter()).enumerate() {
        *slot = shelf_pitch_of(s.landscape, card_row::under_band(band(i)));
    }
    if read == Readout::Failed {
        return Layout {
            pitch,
            ..Layout::failed(lib_chip_on(), shelves)
        };
    }
    Layout {
        pitch,
        ..Layout::new(
        lib_chip_on(),
        shelves,
        n_rows(),
        // the grid's heading and its Sort/Filter row draw over a grid that has items — the
        // design's rule that a control acting on nothing is not drawn — OR mid-reload, the one
        // case where the control row outlives an empty store because it IS what was just pressed
        (read == Readout::Grid && total() > 0) || grid_block_reloading(),
    )
    }
}

/// **Is the grid block being REPLACED rather than removed?** — i.e. is a grid-scoped transition in
/// flight right now.
///
/// This is the difference the focus clamp could not see, and it is the whole of the "sorting throws
/// you back to the top" report. `browse::requery` empties the store synchronously on the press, so
/// for the length of the fade `total()` is 0 and the grid heading, its Sort/Filter row and the rail
/// are all "controls acting on nothing" — the async clamp in [`sync_readout_focus`] then correctly
/// observes that the zone focus is standing in is not drawn and moves it to the head of the
/// document. Which is the library chip, at the top of the page, above every shelf: the user pressed
/// Sort and was returned to the recommendations.
///
/// The control row is not GONE, though. It is the thing they just pressed, it is still on screen
/// (the fader dips it, it does not remove it), and its chips are deliberately live and already
/// showing the QUEUED value (`view_sort_label`). So the block is drawn for the length of its own
/// replacement, and focus has somewhere legitimate to stay.
///
/// Deliberately NOT "any fade": a SECTION transition really does replace the whole document, and
/// focus belongs at the head of the library being arrived in.
fn grid_block_reloading() -> bool {
    unsafe { addr_of!(SCOPE).read() == Scope::Grid && xf().is_swapping() }
}

/// A shelf's whole vertical band, by the shape it is drawn in and by how much of its focused label
/// block it is reserving. `consts::ROW_PITCH` is authored for a poster (`CARD_H` + the focused
/// under-label block + air); a landscape row is the same band with a shorter tile in it, so it is
/// that pitch less the height the tile does not use.
///
/// **`band` is the second axis and the reason this is no longer a constant per shape** — the label
/// block exists only while the shelf holds focus, so an unfocused one reserves
/// `card_row::LABEL_BAND_COLLAPSED` in its place (`card_row::under_band`). At `band ==
/// card_row::UNDER_LABEL_H` this is byte-identical to what it returned before the band could close.
fn shelf_pitch_of(landscape: bool, band: f32) -> f32 {
    let shape = if landscape {
        crate::ui::consts::ROW_PITCH - (CARD_H - RowStyle::EPISODE.h)
    } else {
        crate::ui::consts::ROW_PITCH
    };
    shape - card_row::UNDER_LABEL_H + band
}
const CHIPS_NONE: [Chip; 0] = [];
const CHIPS_SOURCE_ONLY: [Chip; 1] = [Chip::Source];
const CHIPS_ONE: [Chip; 2] = [Chip::Sort, Chip::Filter];
const CHIPS_MANY: [Chip; 3] = [Chip::Source, Chip::Sort, Chip::Filter];
/// The chips DRAWN, in drawn order — Source FIRST when it exists. The ONE predicate behind the
/// draw, the pointer scan and the vertical walk, so a chip can never be invisible-and-clickable or
/// drawn-and-unreachable.
///
/// **The failure read-out takes the grid's controls away and keeps navigation.** Sort and Filter
/// act on a grid that has no items, so neither is drawn (nor is the count, nor the A–Z rail); the
/// Source chip is how you LEAVE a dead source, which is exactly why the read-out spends no line
/// telling you the way out (`Shared Sources.dc.html` D). With one source there is no Source chip to
/// keep, so that row empties completely and the tab strip above is the way out.
///
/// An OPEN menu is the exception: it is anchored under its own chip and would otherwise be a panel
/// hanging off nothing — a listing can fail while its Filter menu is up, and a chip must outlive
/// the panel it opened.
fn chips() -> &'static [Chip] {
    match (
        source_chip_on(),
        readout() == Readout::Failed && !menu_open(),
    ) {
        (true, false) => &CHIPS_MANY,
        (true, true) => &CHIPS_SOURCE_ONLY,
        (false, false) => &CHIPS_ONE,
        (false, true) => &CHIPS_NONE,
    }
}
/// **The toolbar's focused chip INDEX — the ONE authority, read by the draw, by the walk and by
/// the activation alike.** Nothing else may read `TOOL_F`.
///
/// The raw static is where the user last walked to, and the row's LENGTH moves under it without
/// any input at all: a second source is granted, or a listing fails and `chips()` collapses
/// `CHIPS_MANY` → `CHIPS_SOURCE_ONLY`, or a profile switch is absorbed by the `EPOCH` watchdog with
/// no `enter()` to zero it. So the index is clamped to the row that is actually DRAWN, at the one
/// point everything reads, because the failure of clamping it in only some of them is silent and
/// asymmetric: `focused_chip` clamped and the draw did not, so the toolbar drew NO focused chip
/// while OK still activated the clamped last one and RIGHT was dead against a length the ring was
/// not standing in.
///
/// Clamping rather than resetting is deliberate: the raw value survives, so a row that grows back
/// (the source answered again) returns focus to the chip the user actually walked to instead of
/// dumping it at 0.
fn tool_f() -> usize {
    let row = chips();
    unsafe { addr_of!(TOOL_F).read() }
        .min(row.len().saturating_sub(1))
        .max(control_lo(row))
}

/// **The first index of the GRID'S CONTROL ROW inside [`chips`].**
///
/// One array holds both, because they share their strings, their measured widths and their drawn
/// rects — but they are TWO zones now: [`Chip::Source`] heads the document as [`Area::LibChip`] and
/// the rest are the row under the grid's heading. So the toolbar's cursor may not rest on index 0
/// when Source is there. It did until this was written: [`enter_zone`] seated `TOOL_F = 0`, the
/// draw skips Source in the control row, and the result was a toolbar you could walk into where
/// NOTHING was focused — visible in the simulator as a page whose Sort and Filter never light up.
///
/// Pure over the row so the four shapes are gradeable without a section table.
fn control_lo(row: &[Chip]) -> usize {
    row.iter().position(|&c| c != Chip::Source).unwrap_or(0)
}

/// …and how many chips that row actually has. NOT `chips().len()`, which counts the head chip too:
/// a one-library page whose listing failed keeps its Source chip and has no controls at all, and
/// [`below_top_bar`] and [`sync_readout_focus`] both need that to read as ZERO.
fn control_n(row: &[Chip]) -> usize {
    row.iter().filter(|&&c| c != Chip::Source).count()
}

/// The chip holding toolbar focus, or `None` when the row is EMPTY — which it is on a one-source
/// failure, where every chip this toolbar has is a grid operation.
fn focused_chip() -> Option<Chip> {
    chips().get(tool_f()).copied()
}

/// THE chip → menu map, and the ONE place a chip is turned into an action. Both activation paths
/// (OK on the focused chip, a pointer click on one) call it, so the remote and the pointer cannot
/// disagree about what a chip does — the pointer arm used to route through `on_ok`, which meant it
/// inherited whatever `on_ok` got wrong.
///
/// **Exhaustive on purpose.** A `_` arm here is the bug this whole enum exists to prevent: the
/// toolbar used to match on the chip's INDEX with a catch-all, so the day Source was inserted at 0
/// it opened the Sort menu and both Sort and Filter opened Filter — silently, since every index
/// still resolved to *something*. Add a chip and this stops compiling, which is the point.
fn open_chip_menu(c: Chip) {
    match c {
        Chip::Source => open_source_menu(),
        Chip::Sort => open_sort_menu(),
        // the unwatched switch lives inside this one
        Chip::Filter => open_filter_menu(false),
    }
}

/// Which menu a chip opens, as a value — the testable half of [`open_chip_menu`], which is the
/// half that performs it. They are written as one `match` each over the same enum, so a new chip
/// is a compile error in both.
///
/// The keys are empty, and that is the honest answer here: a key describes a panel that was BUILT
/// from a row set, and this function answers which menu a chip leads to. Its caller matches on the
/// variant, never on the payload.
#[cfg(test)]
fn chip_menu(c: Chip) -> Menu {
    match c {
        Chip::Source => Menu::Source {
            gen: 0,
            acts: Vec::new(),
        },
        Chip::Sort => Menu::Sort { built: 0 },
        Chip::Filter => Menu::Filter,
    }
}

/// A chip's two runs: the dim static NAME and the bright VALUE that carries the information.
fn chip_value(c: Chip) -> (&'static str, String) {
    match c {
        // the chip names the LIBRARY (the owner rides beside it as a dim micro run — see
        // `chip_strs`), which is the one piece of source annotation the design keeps and the reason
        // the tab strip above needs none
        Chip::Source => (
            "Library",
            crate::browse::section_title(view_section()).to_string(),
        ),
        Chip::Sort => ("Sort", view_sort_label().to_string()),
        Chip::Filter => ("Filter", view_filter_value()),
    }
}

// ---- per-frame draw caches (rebuilt only when their source state changes; building CStrings
// + re-measuring text every frame was a review-confirmed waste — same rationale as TAB_CACHE) --
struct ChipStrs {
    /// WHICH chip this is. The row is built from `chips()`, whose length and order both move, and
    /// the document draws the Source chip in a different place from the grid's own controls — so a
    /// position is not an identity here any more than it is at the activation.
    kind: Chip,
    name: CString,
    val: CString,
    /// The Source chip's owner annotation — the handle, a rung down and at 62% of the label's ink.
    /// `None` on your own libraries and on every other chip, and None means NO RUN: no gap, no
    /// separator, no draw call.
    note: Option<CString>,
    w: f32,
}
/// (cache key, chips). The key is every string the row draws — **including the Source chip's own
/// value and its handle**, without which the chip keeps a stale label right across a source switch,
/// which is the one moment its label is the only thing on screen that changed.
static mut CHIP_CACHE: Option<(String, Vec<ChipStrs>)> = None;
static mut RAIL_LABELS: Option<(u32, usize, Vec<CString>)> = None; // (sections_gen, section, labels)
                                                                   // (total, the section TYPE's noun, the rendered string) — keyed on the noun rather than on a
                                                                   // movie/show boolean, because a section is one of four types now (`browse::SecKind`)
static mut COUNT_C: Option<(i64, &'static str, CString)> = None;
/// The failure read-out's two runtime lines, keyed on the RAW source labels they were built from
/// (`(name, handle)`) — see [`dead_strs`] for why that rather than a generation, and why the key is
/// the raw pair rather than the resolved one.
static mut DEAD_C: Option<(String, String, CString, Option<CString>)> = None;
/// The Empty read-out's caption, cached per (section table, section, filtered) — the three inputs
/// that can change what it says.
static mut EMPTY_C: Option<(u32, usize, bool, CString)> = None;
// letter rail: focused letter + the per-letter rects recorded at draw (pointer hit-testing)
/// Every bucket `/firstCharacter` can return, not one alphabet's worth. `#` + Latin is 27, which
/// is where the old 34 came from — but a Cyrillic section adds 32 more, and the cap TRUNCATED in
/// silence: the rail simply stopped mid-alphabet and every title past it became unreachable by
/// jump, with nothing on screen saying anything had been dropped. 64 covers `#` + Latin + Cyrillic
/// (59) with room, and now that the rail SCROLLS a large `n` costs no layout.
const MAX_LETTERS: usize = 64;
/// The rail's letter pitch — a CONSTANT, not the band divided by `n`. It was the latter (capped at
/// this value), which made the spacing a function of how many letters a section happens to have: 9
/// letters drew at 34 px, a Cyrillic 30 fell to 27.5 against a CAPTION cap height of ~17, closing
/// the gap to under half a letter and reading as a solid column of type rather than an index. Past
/// what fits at this pitch the rail scrolls ([`RAIL_SCROLL`]) — so every section's rail has the
/// same spacing, for the same reason it is top-anchored rather than centred.
const RAIL_PITCH: f32 = 34.0;
/// The rail's capsule track width — the widest thing the rail draws, and so what [`RAIL_BAND`]
/// reserves and what [`rail_geom`]'s `cx` is placed from. Named because those three used to be the
/// literal `44` / `22` / `SCR_W - 64` spelled in three places with no arithmetic tying them.
const RAIL_TRACK_W: f32 = 44.0;
/// The rail's first letter-slot top, below [`GRID_TOP`] — the letters line up with the grid's first
/// row rather than with its card tops.
const RAIL_TOP_OFF: f32 = 8.0;
/// How far the capsule track overhangs the letter WINDOW at each end. It is the difference between
/// what the rail measures its scroll against (the window) and what it draws (the capsule), and so
/// the term the safe-area bound in [`rail_geom`] has to carry.
const RAIL_CAP_PAD: f32 = 10.0;
static mut RAIL_F: usize = 0;
static mut RAIL_RECTS: [Rect; MAX_LETTERS] = [Rect::new(0.0, 0.0, 0.0, 0.0); MAX_LETTERS];
static mut RAIL_N: usize = 0;
/// Scroll offset (px) for a section with more letters than fit. Driven by the rail's own focus
/// while it holds focus, otherwise by the letter the GRID is in — so scrolling the grid carries
/// the rail along and it never shows a stretch of alphabet the grid left behind.
static mut RAIL_SCROLL: Spring = Spring::at(0.0);

// ---- the section read-out: what it says, where it sits, what its one control does ------------
//
// `Shared Sources.dc.html` deliverable D. A borrowed server that does not answer is stated in two
// places, neither of them Home — the Sources list, and here. The rules that are easy to lose:
//
//   * The failure owns the GRID, not the screen. It is anchored in the content region and the
//     chrome above it — profile chip, tab strip, the Source chip — is untouched and still live.
//     That difference is the whole message: a section failed, the app did not.
//   * The verdict names the MACHINE ("Can't reach <machine>") and the reason names the PERSON
//     ("Shared by <handle> · your own server is fine."). This is the one place outside the Sources
//     list that speaks a machine name, because the machine is what failed; and the reason does two
//     jobs at once — whose it is, and that nothing else is down.
//   * Those are the only two identifying strings this screen renders, and the list is CLOSED. No
//     address, no path, no URL, no machineIdentifier, no token — `ui::stats`' redaction rule, and
//     for its reason: a read-out is something a user PHOTOGRAPHS for a bug report, and on a
//     borrowed source the machine is not even theirs to publish.
//   * NO alert glyph (that mark is the player's), no BACK hint (BACK is universal, and the chip
//     and tabs above are the visible way out), no sort/filter chips, no count, no rail — all four
//     of those act on a grid that has no items.

/// The band the grid occupies — what the read-out is ABOUT, and where it is anchored.
///
/// Deliberately NOT `Rect::FULL`, which is the read-out's canonical position everywhere else: a
/// block centred on the panel says the APP failed. Centring it on the content region says this
/// section did, and leaves the live chrome above it saying so.
fn content_region() -> Rect {
    // bottom on `MARGIN_Y` rather than a `space::LG` rung: the read-out and its Try again pill are
    // CENTRED in this band, so the band itself is what has to sit inside the overscan frame
    Rect::new(
        MARGIN_X,
        GRID_TOP,
        SCR_W - 2.0 * MARGIN_X,
        SCR_H - GRID_TOP - MARGIN_Y,
    )
}

/// How many toolbar chips are DRAWN — [`chips`]'s length, and nothing else. The whole decision
/// (which chips a failure keeps, and that an open menu keeps its anchor) lives in `chips`, so the
/// walk and the draw cannot answer it differently.
///
/// One cost is recorded here so it is not rediscovered as a surprise: a query the SERVER rejects
/// outright (a sort key it 400s on) fails the same way a dead server does, and the chips that could
/// have changed it are not drawn — so *Try again* re-runs the same losing query, and the way out is
/// the Source chip, another section, or Home. Drawing Sort and Filter over the failure is in the
/// design's rejected column ("they act on a grid that has no items") and clearing the user's filter
/// behind their back is worse than either, so this stands.
fn toolbar_n() -> usize {
    control_n(chips())
}

/// The failure read-out's verdict + reason — the machine that failed, and the person who shared it.
///
/// **The owner is real now.** On the branch this came from it was hard-wired `None` with a note
/// that nothing yet mapped a section to the source it came from; the multi-source table is exactly
/// that mapping, so `browse::cur_source_labels` answers both halves and the reason line stops being
/// a stand-in. An empty handle still means YOUR OWN server, and the line is then ABSENT rather than
/// empty — our own server failing is a complete statement and there is nobody to name.
///
/// Cached, and keyed on the STRINGS rather than on a generation, because that is what actually
/// changes: a machine names itself through `friendlyName`, which lands on a worker long after the
/// table generation last moved, and a cache keyed on the generation would hold "Can't reach "
/// forever. Two `&str` comparisons a frame, and only while the read-out is on screen.
fn dead_strs() -> &'static (String, String, CString, Option<CString>) {
    // Keyed on the RAW labels, which are two `&'static str` reads off this module's own table — and
    // the staleness check runs BEFORE anything resolves them. It used to resolve first and compare
    // after, which meant `source_labels`' session fallback ran every drawn frame the read-out was
    // up: a file read and a JSON parse, in the one state where that fallback is guaranteed to fire
    // (a source that never answered can never have supplied a `friendlyName`).
    let (name, handle) = crate::browse::cur_source_labels();
    let cache = unsafe { &mut *addr_of_mut!(DEAD_C) };
    let stale = cache
        .as_ref()
        .map(|(n, h, _, _)| n != name || h != handle)
        .unwrap_or(true);
    if stale {
        let (machine, owner) = source_labels();
        let verdict = CString::new(format!("Can't reach {machine}")).unwrap_or_default();
        let reason = owner.as_ref().map(|o| {
            CString::new(format!("Shared by {o} \u{b7} your own server is fine."))
                .unwrap_or_default()
        });
        *cache = Some((name.to_string(), handle.to_string(), verdict, reason));
    }
    cache.as_ref().unwrap()
}

/// The two names the read-out may speak, resolved for the source the screen is showing.
///
/// The MACHINE falls back to the session's own server name and then to a WORD — never to the
/// address, though it is right there in the client. This read-out is photographable (it is what a
/// user sends when reporting "my friend's library stopped working") and `ui::stats` already settled
/// the rule for anything a camera can reach: no URL, no path, no address, no machineIdentifier. On
/// a BORROWED source that reasoning is stronger, because the machine is a third party's.
///
/// The OWNER is `None` for your own server. [`FailTest::Dead`] can put a stand-in there, and only
/// there: with one server and nothing shared, the design's three-block read-out has no way to
/// appear on a real panel at all, and a capture of it is the only thing that grades the layout.
fn source_labels() -> (String, Option<String>) {
    let (name, handle) = crate::browse::cur_source_labels();
    let machine = if !name.is_empty() {
        name.to_string()
    } else {
        let session = crate::plex::session::peek().server.name;
        if session.is_empty() {
            "this server".to_string()
        } else {
            session
        }
    };
    let owner = if handle.is_empty() {
        (fail_test() == Some(FailTest::Dead)).then(|| STANDIN_OWNER.to_string())
    } else {
        Some(handle.to_string())
    };
    (machine, owner)
}

/// The stand-in handle [`FailTest::Dead`] falls back to. A DOCUMENTATION placeholder, deliberately
/// not a plausible one: it is drawn on a real screen and may be photographed, and a real person's
/// plex.tv handle is not ours to put on a television.
const STANDIN_OWNER: &str = "friend";

/// The Empty read-out's caption. A library the server ANSWERED for and that holds nothing is
/// named — "No films in Film Club" — because the name is what makes it a statement rather than a
/// shrug. A listing emptied by a FILTER is not the library being empty and must not claim to be.
fn empty_caption() -> &'static CString {
    let sec = crate::browse::cur();
    let gen = crate::browse::sections_gen();
    let filtered = crate::browse::unwatched() || crate::browse::genre_sel().is_some();
    let cache = unsafe { &mut *addr_of_mut!(EMPTY_C) };
    let stale = cache
        .as_ref()
        .map(|(g, s, f, _)| *g != gen || *s != sec || *f != filtered)
        .unwrap_or(true);
    if stale {
        let s = if crate::browse::section_count() == 0 {
            // the table itself answered with nothing we browse (a music-only account) — there is no
            // section to name, and naming one would be inventing it
            "No libraries on this server".to_string()
        } else if filtered {
            "Nothing here matches".to_string()
        } else {
            // the TYPE's own noun — a section is one of four kinds now, so "films" is a guess
            // rather than an answer for three of them
            let noun = crate::browse::section_kind(sec)
                .map(|k| k.noun())
                .unwrap_or("items");
            format!("No {noun} in {}", crate::browse::section_title(sec))
        };
        *cache = Some((gen, sec, filtered, CString::new(s).unwrap_or_default()));
    }
    &cache.as_ref().unwrap().3
}

/// The read-out's one action: re-kick THIS source, and nothing else in the app.
///
/// Two layers can have failed and the fix differs. A failed section TABLE is re-fetched here and
/// now — blocking, one small GET, the same call [`enter`] makes on every route entry, so a dead
/// server costs the connect timeout exactly as leaving and coming back does. A failed PAGE only
/// needs its back-off cleared: `browse` is already counting down to the next automatic attempt, and
/// the button's job is to skip the wait rather than to start a second fetch.
fn retry_source() {
    // `browse` owns WHICH layer failed — it holds the per-source flags — and clears the back-off on
    // both, which is the right answer either way. Nothing blocks and nothing re-enters the screen:
    // a table that lands arrives through `pump`'s worker like any other source's, and the read-out
    // clears itself on the frame `readout()` stops saying Failed.
    crate::browse::retry_cur_source();
}

/// Keep the focus band and the read-out in agreement, every frame. The failure can arrive long
/// after [`enter`] (a first page that fails two seconds in) and can clear without a keypress (the
/// automatic retry succeeding), and the grid and the read-out can never both hold focus — one of
/// them is not on screen. Focus LANDS on Try again rather than beside it, because a server that did
/// not answer often answers a second later and that press is the reason the control exists.
fn sync_readout_focus() {
    let failed = readout() == Readout::Failed;
    unsafe {
        // the rail goes first: it is taken away by EVERY read-out, not only the failure, and a
        // focus band left on it is one that only LEFT can escape
        if AREA == Area::Rail && !rail_drawn() {
            AREA = Area::Grid;
        }
        // The bands the failure takes away: the grid and its rail always, the toolbar only when it
        // has nothing left to hold. A control that is not drawn must not hold focus — the same rule
        // that stops it being clickable.
        let undrawn =
            matches!(AREA, Area::Grid | Area::Rail) || (AREA == Area::Toolbar && toolbar_n() == 0);
        if failed && undrawn {
            AREA = Area::Status;
        } else if !failed && AREA == Area::Status {
            AREA = Area::Grid;
        }
    }
    // **…and then the general rule, which the three special cases above are instances of.** A
    // landing can take ANY zone away — a source fails, a library empties, a staged shelf set
    // commits, the last favourite of a type is switched off — and focus must not spend a frame on
    // something that is not drawn. Derived from the same projection the walk uses, so a zone can
    // never be reachable-but-undrawn or drawn-but-unreachable.
    let lay = layout();
    unsafe {
        if !zone_drawn(&lay, AREA) {
            // the head of the document — the first CONTENT zone, not the first drawn one. Those
            // differ by exactly the tab bar, which is the shared control ABOVE the page: landing
            // focus there on every async landing would read as the page throwing you out of it.
            // `first_content` is `None` only when the content region really is empty, and then the
            // bar is the honest answer.
            AREA = lay.first_content().unwrap_or(Area::Tabs);
            enter_zone(&lay, AREA, 1);
        }
        // …and inside the shelves, the CURSOR needs the same treatment for the same reason: the
        // shelf set changes under it routinely on this endpoint (two hubs rotate their subject
        // between requests), and Mark Watched and Remove from Continue Watching both delete a tile
        // out from under the user.
        if AREA == Area::Shelf {
            shelf_focus_survives();
        }
    }
}

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
/// The open menu and its key, as a borrow — [`Menu`] owns a `Vec` and so cannot be read out by
/// copy.
///
/// **Take what you need out of it before anything rebuilds.** Every builder ASSIGNS `MENU`, which
/// drops the key this borrow points into: [`src_action`] copies its answer out, [`menu_stale`]
/// compares and returns a `bool`, and an arm that calls a builder either binds nothing out of the
/// variant or reads its key into a local first ([`menu_commit`]'s Sort arm).
fn menu() -> &'static Menu {
    unsafe { &*addr_of!(MENU) }
}
/// Install the open menu together with its key, dropping whatever the last one held.
fn set_menu(m: Menu) {
    unsafe { *addr_of_mut!(MENU) = m };
}
/// Is the SOURCES panel the open menu? Asked by the panel geometry (it is wider and carries a level
/// band above its list), by both input ladders and by the draw, so it is one predicate rather than
/// a `matches!` re-spelled at each of them.
fn source_menu_open() -> bool {
    matches!(menu(), Menu::Source { .. })
}
fn area() -> Area {
    unsafe { addr_of!(AREA).read() }
}
/// The tab pill holding remote focus, or -1. ONE source for it: the draw pass and the strip's
/// scroll step must agree on which pill to keep on screen, or the row chases a pill it isn't
/// highlighting.
fn tab_focus() -> c_int {
    if area() == Area::Tabs && !menu_open() {
        tab_idx() as c_int
    } else {
        -1
    }
}

/// The focused pill's identity, and the DRAWN index it resolves to today.
fn tab_p() -> Pill {
    unsafe { addr_of!(TAB_P).read() }
}
fn set_tab_p(p: Pill) {
    unsafe { TAB_P = p }
}
/// **The drawn index of the focused pill — the one place the identity becomes a position.**
///
/// A pill whose type lost its last favourite is no longer in the strip, and the honest answer then
/// is Home: the one pill that is always drawn. Resolving on READ rather than remapping on change is
/// what makes this correct without anything having to notice the change.
fn tab_idx() -> usize {
    crate::ui::widgets::pill_of(tab_p()).unwrap_or(0)
}

/// The band DOWN off the shared top bar lands in — the toolbar normally, the failure read-out when
/// the chips it would otherwise land on are not drawn. One expression, because both stops on that
/// bar ([`Area::Chip`] and [`Area::Tabs`]) leave it downward and a screen whose two halves of one
/// control disagreed about where DOWN goes is the bug this whole change is about.
fn below_top_bar() -> Area {
    if toolbar_n() > 0 {
        Area::Toolbar
    } else {
        Area::Status
    }
}

/// **Where this screen's focus is on the SHARED top bar** — the one question `widgets` asks of
/// every screen that wears it, so the strip's capsules and the profile chip's unfurl are animated
/// in one place rather than three. A menu takes the bar's focus away entirely, which is what
/// [`tab_focus`] already says for the pills.
pub(crate) fn top_focus() -> crate::ui::widgets::TopFocus {
    use crate::ui::widgets::TopFocus;
    if area() == Area::Chip && !menu_open() {
        return TopFocus::Chip;
    }
    match tab_focus() {
        i if i >= 0 => TopFocus::Pill(i as usize),
        _ => TopFocus::Away,
    }
}

/// The pill focus LANDS on when it walks up into the tab row.
fn own_pill() -> usize {
    // The pill that REPRESENTS this section, which for a borrowed library is its type's — and then
    // clamped into the pills that EXIST. `last_section_pill` is that ceiling and owns the "with no
    // section table there is only Home" fallback: an index past the last pill is a highlight on
    // nothing, reachable now that a failed discovery keeps the user on this screen instead of
    // bailing out of `enter` before it had a table. It used to be spelled `search_pill() - 1` here,
    // which meant "the last pill that is a library" only because Search happens to sit last — a
    // fact about the strip that now lives in the strip.
    // `None` when this type's pill has just been switched off under us — one frame at most, since
    // `browse::repoint_cur` moves `cur()` in the same breath as the edit. Home is the honest
    // answer meanwhile: it is the one pill that is always drawn.
    let p = crate::ui::widgets::pill_of(Pill::Section(unsafe { WANTED_KIND })).unwrap_or(0);
    p.min(crate::ui::widgets::last_section_pill())
}

/// The same answer as [`own_pill`], as an IDENTITY — what a cursor stores. `Pill::Home` when this
/// type has no pill today, which is [`own_pill`]'s own `unwrap_or(0)` said in the other vocabulary.
fn own_pill_id() -> Pill {
    let kind = unsafe { WANTED_KIND };
    if crate::ui::widgets::pill_of(Pill::Section(kind)).is_some() {
        Pill::Section(kind)
    } else {
        Pill::Home
    }
}

/// The tab pill holding focus, as a plain index — what a route change LEAVING this screen carries
/// to Home, so the pill the user is standing on is still the pill under focus on the other side.
/// Without it the SELECTION capsule travels to Home while the FOCUS capsule snaps back to wherever
/// Home was last left, which reads as two objects rather than one bar crossing a boundary. `None`
/// whenever the tab row is not what holds focus (a menu, the grid, the toolbar, the rail).
pub(crate) fn focused_pill() -> Option<Pill> {
    (tab_focus() >= 0).then(tab_p)
}

// ---- derived grid facts ---------------------------------------------------------------------
fn total() -> usize {
    crate::browse::total().max(0) as usize
}
fn n_rows() -> usize {
    total().div_ceil(COLS)
}
fn cols_in_row(r: usize) -> usize {
    let t = total();
    if (r + 1) * COLS <= t {
        COLS
    } else {
        t.saturating_sub(r * COLS)
    }
}
fn focus_idx() -> usize {
    unsafe { addr_of!(GR).read() * COLS + addr_of!(GC).read() }
}
/// Clamp the grid focus into the live item count (a re-query / late page can shrink it).
fn clamp_focus() {
    unsafe {
        let t = total();
        if t == 0 {
            GR = 0;
            GC = 0;
            return;
        }
        if GR >= n_rows() {
            GR = n_rows() - 1;
        }
        let nc = cols_in_row(GR);
        if GC >= nc {
            GC = nc.saturating_sub(1);
        }
    }
}
/// The focused grid item (None while its page is still loading).
/// **The item focus is standing on, wherever it is standing.** The grid's paged store, or — since
/// the library grew shelves of its own — a tile on one of them.
///
/// One function rather than a second `focused_shelf_item`, because every consumer asks the same
/// question: `app.rs`'s activation, its context-menu open, and the poster prefetch all want "the
/// item under the ring" and none of them should have to know which half of the page it came from.
pub(crate) fn focused_item() -> Option<&'static PmsMovie> {
    if area() == Area::Shelf {
        return unsafe {
            crate::browse::section_hubs::shelves(crate::browse::cur())
                .get(SHELF_F)
                .and_then(|s| s.items.get(SHELF_C))
        };
    }
    crate::browse::item(focus_idx())
}

/// Is the focused card on this library's own Continue Watching shelf? **The half `open_tile_menu`
/// used to hardcode as `false`** — it is what makes *Remove from Continue Watching* appear, and
/// the Library had no deck to show for as long as it had no shelves.
pub(crate) fn focused_from_deck() -> bool {
    area() == Area::Shelf
        && unsafe {
            crate::browse::section_hubs::shelves(crate::browse::cur())
                .get(SHELF_F)
                .map(|s| s.is_continue)
                .unwrap_or(false)
        }
}
pub(crate) fn menu_open() -> bool {
    !matches!(menu(), Menu::None)
}

// ---- enter / view persistence ---------------------------------------------------------------

/// How the screen is being ARRIVED at — the one thing [`enter`] cannot infer, and the only
/// difference between its two kinds of caller.
pub(crate) enum Arrival {
    /// A hard cut: the `/tmp/plxnative-library` boot, where there is nothing on screen to be
    /// continuous with, so the shared strip LANDS on this section's pill instead of sliding to it.
    Cut,
    /// The page cross-fade (`ui::nav`). The tab bar is continuous chrome with a capsule crossing it
    /// right now, and [`widgets::tab_row_reveal`](crate::ui::widgets::tab_row_reveal) JUMPS the
    /// strip scroll — a jump under a travelling capsule teleports the pills out from under it. It
    /// is also unnecessary here: `tab_row_update` has been targeting the pending pill since the
    /// press frame (`nav::view_tab`), so the strip is already springing there, and a pressed pill
    /// was visible by construction anyway (`tab_pill_at` matches CLIPPED rects, and D-pad focus
    /// scrolls a pill into view before OK can reach it).
    Faded,
}

/// Enter the Library on a permanent type pill (0-based, Home excluded). This stores the requested
/// kind without I/O; an already discovered section is restored immediately, otherwise the page
/// shows Loading/Empty/Failed until worker discovery resolves one.
///
/// Everything here TELEPORTS something the user can see — the store swap, the restored grid scroll,
/// the focus band — so on the `Faded` arrival it is called from the page fade's floor, at alpha 0,
/// rather than on the press frame.
pub(crate) fn enter(kind: crate::browse::SecKind, arrival: Arrival) {
    flush(); // a reload queued on the way out is applied, not dropped (see [`flush`])
    unsafe { WANTED_KIND = kind };
    // A table that could not be FETCHED used to return here, before a single line of screen state
    // was set — which left the focus band wherever the last visit had parked it, on a screen that
    // now has no grid and no chips. The discovery failing is a state this screen draws (the read-out
    // one layer up), so it is entered like any other: everything below runs, and only the parts
    // that index a section are skipped.
    if let Some(section) = crate::browse::tab_section(wanted_tab()) {
        // `tab` is a PILL index (`app.rs`'s `Nav::Library`, which derives it from the pill pressed),
        // and a pill is a type rather than a library — so it resolves owned-first/shared-second
        // here, at the ONE boundary where the strip's index space enters this screen.
        crate::browse::set_cur(section);
        crate::browse::kick_letters();
        restore_view();
        // Put the selected permanent type pill on screen once, rather than dragging the row every
        // frame — but only on a CUT;
        // see `Arrival::Faded` for why a jump is both wrong and redundant under a travelling capsule.
        if matches!(arrival, Arrival::Cut) {
            // `pill_of` rather than a hand-rolled `+ 1`: the Home offset is the strip's fact, and
            // this was one of the six sites that used to spell the ladder out (see `Pill`'s doc).
            if let Some(p) = crate::browse::tab_of_section(crate::browse::cur())
                .and_then(|t| crate::ui::widgets::pill_of(Pill::Section(crate::browse::tab_kind(t)?)))
            {
                crate::ui::widgets::tab_row_reveal(p);
            }
        }
    }
    set_menu(Menu::None);
    unsafe {
        // **Where the band lands is a function of the RESTORED SCROLL, not a constant.** This used
        // to be `Area::Grid` unconditionally, which was right while the grid WAS the screen and is
        // wrong now that it is the document's last block: a first visit restores `(0, 0)`, so focus
        // sat on grid row 0 while the page showed its head, and the scroll target that focus asks
        // for immediately dragged the document past every shelf the library publishes. At the head
        // the band belongs on the first CONTENT zone; a returning visit that was scrolled into the
        // grid still comes back to the grid.
        AREA = Area::Grid;
        TOOL_F = 0;
        PENDING = Pending::NONE; // whatever was queued before we left has just been flushed
        EPOCH = crate::browse::query_gen(); // AFTER `set_cur` above, so our own bump isn't foreign
    }
    // …and then the band is seated on the zone the RESTORED SCROLL is actually showing — the head
    // on a first visit, the shelf you left from, the grid if that is where you were. Seated through
    // `enter_zone` so a shelf gets its cursor like any other entry from above.
    seat_from_restored_view();
    // ...and then the read-out takes the band if it owns the content region, so an entry onto a
    // dead source opens ON Try again rather than on the frame after it
    sync_readout_focus();
    // Route entry has NO outgoing content: park at 0 and fade in once the listing exists (the very
    // next frame for a section whose store is still resident). This is also the ONE recovery for a
    // fader left mid-phase by leaving the screen — only this route steps it.
    remount();
}

fn restore_view() {
    let (focus, scroll) = crate::browse::saved_view();
    unsafe {
        GR = focus / COLS;
        GC = focus % COLS;
        (*addr_of_mut!(SCROLL)).jump(scroll);
        FOCUS_S = Spring::at(1.0);
        PREV_IDX = -1;
    }
    clamp_focus();
}

/// Reset the grid to the top — every re-query (sort/filter change) starts a fresh listing.
/// Deliberately does NOT touch the focus band: toggling the unwatched filter must leave focus
/// on that chip, not warp it into the grid.
fn grid_reset() {
    // **To the GRID's top, not the document's.** `jump(0.0)` was right for exactly as long as
    // scroll 0 WAS grid row 0; under one continuous document it is the head of the page — the
    // library chip and the shelves — so choosing a sort threw the viewer all the way back up to the
    // recommendations and made them navigate down to All again to see the result of the sort they
    // had just picked. The listing is what changed; the reader's place in the document is not.
    //
    // `row_reveal(0)` rather than `grid_top()`: it is the same clamped target the focus rule uses,
    // so the reset lands exactly where walking into row 0 would, heading and control row included.
    let want = layout_settled().row_reveal(0);
    unsafe {
        GR = 0;
        GC = 0;
        (*addr_of_mut!(SCROLL)).jump(want);
        PREV_IDX = -1;
    }
}

/// Queue a switch to the library a TAB PILL opens. The STORE moves on the fade floor
/// ([`apply_section`]); the PILL moves now (every chrome reader goes through [`view_section`]).
///
/// A pill is a TYPE, not a library, so the strip's index space is not the table's — see
/// `browse::tab_section`. Everything downstream of here speaks SECTION indices. It takes the KIND
/// rather than the strip position for the reason [`Pill::Section`] carries one: the position is
/// not a stable name for a destination now that the favourite switch can add or remove a pill.
fn switch_tab(kind: crate::browse::SecKind) {
    unsafe { WANTED_KIND = kind };
    if let Some(section) = crate::browse::tab_of_kind(kind).and_then(crate::browse::tab_section) {
        switch_to_section(section);
    } else {
        remount();
        crate::ui::idle::invalidate();
    }
}

/// Queue a switch to a section by its ABSOLUTE index in the table — what the Sources list picks,
/// where a row can name a library on any server and on no pill at all.
fn switch_to_section(i: usize) {
    if i >= crate::browse::section_count() || i == view_section() {
        return;
    }
    request_section(i);
}

/// The committed half of [`switch_section`] — runs at alpha 0. The guard is re-checked against
/// the store because `cur()` may have moved between the request and the commit.
fn apply_section(i: usize) {
    if i >= crate::browse::section_count() || i == crate::browse::cur() {
        return;
    }
    crate::browse::set_cur(i);
    crate::browse::kick_letters();
    restore_view();
}

// ---- letter rail helpers --------------------------------------------------------------------

fn rail_on() -> bool {
    crate::browse::rail_available()
}
/// Is the A–Z rail actually on screen? The ONE answer, read by the draw, by the RIGHT key that
/// enters it and by [`sync_readout_focus`] — the `toolbar_n` rule applied to the other control
/// this screen can take away.
///
/// It INDEXES a grid, so any read-out standing in for one takes it with it. Its letters do not:
/// they are query-independent and stay resident from the listing that worked before this one
/// failed, so `rail_on` alone would happily draw a rail into an empty region — and, worse, let
/// RIGHT park focus on it, since with no items `cols_in_row` is 0 and RIGHT always falls through.
fn rail_drawn() -> bool {
    // …and it is the GRID's rail, so it is drawn only while focus is in the grid REGION. It
    // indexes the alphabet of a poster wall; hanging on the right edge while the user is walking a
    // shelf would be a control pointing at something they are not looking at.
    //
    // An alpha spring fades it rather than cutting it, and that spring reports to `ui::idle` like
    // every other — see `RAIL_A`.
    rail_on() && !menu_open() && readout() == Readout::Grid && rail_region()
}

/// Is focus in the region the rail indexes? The grid, its control row (which acts on the same
/// grid), or the rail itself.
fn rail_region() -> bool {
    matches!(area(), Area::Grid | Area::Rail | Area::Toolbar)
}

/// The rail's own fade, 0…1. It exists so the rail DISSOLVES as focus leaves the grid region
/// instead of blinking out, and it is a `Spring`, so `ui::idle` hears the motion for free — an
/// animator that reported nothing would either freeze on screen or hold the panel presenting
/// forever, which are opposite failures and each invisible to the other's gate.
static mut RAIL_A: Spring = Spring::at(0.0);
/// The rail letter whose range contains item `idx`. Clamped to the drawn set, since it also
/// indexes [`RAIL_RECTS`] by way of [`RAIL_F`].
fn letter_of(idx: usize) -> usize {
    let mut sum = 0usize;
    for (i, (_, n)) in crate::browse::letters()
        .iter()
        .enumerate()
        .take(MAX_LETTERS)
    {
        sum += *n as usize;
        if idx < sum {
            return i;
        }
    }
    crate::browse::letters()
        .len()
        .min(MAX_LETTERS)
        .saturating_sub(1)
}
/// The rail's geometry for `n` letters: `(y0, cx, win_h, max_scroll)`. ONE answer, because the
/// scroll step in [`update`] and [`draw_rail`] must agree about the window — a reveal rule aimed
/// at a window that is not the one on screen parks the focused letter off the end of it.
fn rail_geom(n: usize) -> (f32, f32, f32, f32) {
    // How many letters fit: the band from the rail's own top down to the OVERSCAN FRAME, less the
    // capsule's overhang past the last letter. It was `SCR_H - GRID_TOP - 40` — the band the old
    // fit-to-fill divisor used — which is a bare inset rather than a bound, and once `GRID_TOP`
    // moved down 18px it put the capsule's bottom cap 6px outside the frame.
    let vis = ((SCR_H - MARGIN_Y - (GRID_TOP + RAIL_TOP_OFF) - RAIL_CAP_PAD) / RAIL_PITCH)
        .floor()
        .max(1.0);
    let win_h = vis.min(n as f32) * RAIL_PITCH;
    // cx puts the track's RIGHT edge on the safe area's, which is what `RAIL_BAND` bought
    (
        GRID_TOP + RAIL_TOP_OFF,
        SCR_W - MARGIN_X - RAIL_TRACK_W * 0.5,
        win_h,
        (n as f32 * RAIL_PITCH - win_h).max(0.0),
    )
}
/// **This screen's outermost drawn chrome, for the overscan audit** ([`crate::ui::consts::SAFE`]).
///
/// The full-width A–Z rail, the first and last grid columns, the toolbar's leading chip and the
/// right-aligned item count — the four rects that reach furthest, at the widest state each can be
/// in. It lives here rather than in the test that grades it because these are private constants and
/// because the rects have to be the DRAWN ones: a table in another module would be a second copy of
/// the layout, which is exactly the shape that let a 90px margin pass for a 5% frame.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (y0, cx, win_h, _) = rail_geom(MAX_LETTERS);
    out.push((
        "library A–Z rail track",
        Rect::new(
            cx - RAIL_TRACK_W * 0.5,
            y0 - RAIL_CAP_PAD,
            RAIL_TRACK_W,
            win_h + 2.0 * RAIL_CAP_PAD,
        ),
    ));
    out.push((
        "library grid, first column",
        Rect::new(MARGIN_X, GRID_TOP, CARD_W, CARD_H),
    ));
    let last = MARGIN_X + (COLS as f32 - 1.0) * (CARD_W + LGAP);
    out.push((
        "library grid, last column",
        Rect::new(last, GRID_TOP, CARD_W, CARD_H),
    ));
    // **Every block of the document, at the head of the scroll** — where each is drawn when the
    // page is at `scroll == 0`, which is the position they are furthest UP the panel and so the
    // only one that can violate the top of the safe frame. Deeper in the document they can only
    // move down.
    let head = Layout::new(true, 1, 40, true);
    out.push((
        "library chip (document head)",
        Rect::new(MARGIN_X, CONTENT_TOP, 200.0, TOOL_H),
    ));
    out.push((
        "library shelf heading (first)",
        Rect::new(
            MARGIN_X,
            CONTENT_TOP + head.shelf_origin(0) - crate::ui::consts::TITLE_DY,
            400.0,
            crate::ui::consts::TITLE_DY,
        ),
    ));
    out.push((
        "library shelf tile (first)",
        Rect::new(
            MARGIN_X,
            CONTENT_TOP + head.shelf_origin(0) + crate::ui::consts::CARD_DY,
            CARD_W,
            CARD_H,
        ),
    ));
    // …and the same three with NO chip and NO shelves, which is the shortest document and puts the
    // grid's own heading highest on the panel
    let bare = Layout::new(false, 0, 40, true);
    out.push((
        "library grid heading (no chip, no shelves)",
        Rect::new(
            MARGIN_X,
            CONTENT_TOP + bare.grid_block_top(),
            400.0,
            crate::ui::consts::TITLE_DY,
        ),
    ));
    out.push((
        "library grid control row (no chip, no shelves)",
        Rect::new(
            MARGIN_X,
            CONTENT_TOP + bare.grid_block_top() + crate::ui::consts::TITLE_DY
                + crate::ui::consts::CARD_DY,
            200.0,
            TOOL_H,
        ),
    ));
    out.push(("library failure read-out band", content_region()));
}

/// Where the rail's scroll wants to be with letter `drive` in hand: the grid's own reveal rule in
/// letter units, keeping one whole letter clear on each side so the driver never sits in the edge
/// fade. Pure, so the window invariant is assertable off-device.
fn rail_scroll_target(cur: f32, drive: usize, n: usize) -> f32 {
    let (_, _, win_h, max) = rail_geom(n);
    let d = drive.min(n.saturating_sub(1)) as f32;
    card_row::reveal(
        cur,
        (d + 2.0) * RAIL_PITCH - win_h,
        (d - 1.0) * RAIL_PITCH,
        max,
    )
}
/// Jump the grid focus to letter `i`'s first title (the prefix-sum index) — focus + scroll
/// move, the listing itself never changes.
fn rail_jump(i: usize) {
    let letters = crate::browse::letters();
    if letters.is_empty() {
        return;
    }
    let i = i.min(letters.len().min(MAX_LETTERS) - 1);
    unsafe { RAIL_F = i };
    let idx = crate::browse::letter_start(i);
    let old = focus_idx();
    unsafe {
        GR = idx / COLS;
        GC = idx % COLS;
    }
    clamp_focus();
    if focus_idx() != old {
        grid_focus_moved(old);
    }
}

// ---- update ---------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    unsafe { PHASE_MS += dt * 1000.0 };

    // ---- content cross-fade. FIRST, because the commit frame teleports SCROLL/GR/GC and the
    // wanted window below must be computed from the POST-swap scroll (the same reason that window
    // is computed before `pump`).
    // "There is something to show" — which is a READ-OUT as much as a grid. This used to ask
    // `!browse::loading_initial()`, i.e. "did a section answer", and a table that answered with
    // NOTHING BROWSABLE has no section to answer: `fetch_state()` falls through its `unwrap_or`
    // forever, the fader never left its hold, and the empty read-out was drawn at alpha 0 on a
    // screen that looked simply blank. `readout()` is the one question with the right shape.
    let ready = readout() != Readout::Loading;
    if xf().tick(dt, ready) {
        apply_pending();
    }
    // Watchdog. Read AFTER the commit above, which accounts for its own bump.
    watch_foreign_store();

    unsafe {
        // wanted window BEFORE pump (its maybe_spawn reads it — after a tab switch the first
        // fetch must target THIS section's restored scroll, not the previous frame's).
        //
        // **Through `Layout`, which is the fix for this screen's sharpest geometry bug.** This used
        // to derive rows straight from `SCROLL`, i.e. assuming scroll 0 is grid row 0. With a
        // header and N shelves above the grid, a 12-shelf library asked `browse` for pages ~13 rows
        // into the catalog while row 0's own slots sat unloaded.
        let (lo_row, hi_row) = layout().visible_rows((*addr_of!(SCROLL)).pos);
        crate::browse::want(lo_row.saturating_sub(1) * COLS, (hi_row + 1) * COLS);
    }
    if crate::browse::pump() {
        clamp_focus();
    }
    // …and AGAIN, because `pump` is one of the things that can replace the store under us: it
    // resets the roster, appends sections and can re-point `cur()`. Sampled only before it, a
    // change landing here reached this frame's own draw before anything noticed. The compare is
    // one atomic read when nothing moved.
    watch_foreign_store();
    let wanted = unsafe { WANTED_KIND };
    if pending().is_none()
        && crate::browse::section_kind(crate::browse::cur()) != Some(wanted)
    {
        if let Some(section) = crate::browse::tab_section(wanted_tab()) {
            crate::browse::set_cur(section);
            crate::browse::kick_letters();
            restore_view();
            unsafe { EPOCH = crate::browse::query_gen() };
        }
    }
    // **Ask this library for its own shelves.** The one call that takes `section_hubs` out of
    // dormancy, and it is idempotent — armed once, then owed a fetch only when something
    // invalidates it.
    crate::browse::section_hubs::kick(crate::browse::cur());
    // …and publish a staged set when the ground may move. TWO cases, and they are different
    // situations rather than one predicate:
    //
    //   1. HIDDEN — a route or section transition is mid-fade, so the page is not on screen and
    //      nothing can be SEEN to move.
    //   2. VISIBLE — only once a BACK-driven head transition has SETTLED, which is
    //      `at_document_head`, shared with `back` so the two rules cannot drift apart.
    //
    // A commit re-resolves focus against the POST-commit layout, which the drawable-zone clamp
    // alone does not cover: a zero-shelf library sitting on its `GridTool` is standing on a zone
    // that stays DRAWN when shelves arrive but moves thousands of pixels down the document.
    // **HIDDEN means the PAGE is hidden, and only a PAGE-scoped fade does that.** This read every
    // `Xfade` as a hidden page, which is wrong for exactly the scope that was added beside it: a
    // `Scope::Grid` fade dips the grid block alone and deliberately holds the library chip and the
    // shelves at full alpha — and those are precisely what a shelf commit moves. So a staged
    // refresh could publish DURING a sort, changing the shelf count and pitches on screen and
    // re-seating focus to the document head, which is the reflow staging exists to prevent.
    // **…and a VISIBLE commit must not land inside a press.** At the head, a commit re-resolves
    // focus onto the new first content zone, and `at_document_head` cannot tell "the head of a
    // composed page" from "grid row 0 of a page that has no chip and no shelves yet" — with
    // neither drawn, row 0's own reveal IS zero. The viewport does not move there (the new first
    // shelf reveals at 0 too), so the visible effect is only the ring stepping from the first
    // poster onto the first shelf of a page that has just finished composing itself, which is
    // accepted: the alternative starves a single-library household's shelves until it navigates
    // away and back. What is NOT acceptable is doing it under an armed press, whose deferred
    // activation would then resolve against the zone focus was moved TO rather than the card that
    // was pressed. `ui::press`' "focus cannot move mid-press" contract, one more time.
    if crate::browse::section_hubs::commit_staged(crate::browse::cur(), may_publish_shelves()) {
        let lay = layout();
        unsafe {
            AREA = lay.first_content().unwrap_or(Area::Tabs);
            enter_zone(&lay, AREA, 1);
        }
        crate::ui::idle::invalidate();
    }
    // AFTER the pump: a landing is exactly what turns the grid into a read-out (or back), and the
    // focus band must not spend a frame on the one that is no longer drawn.
    sync_readout_focus();
    // waiting menu data re-kicks each frame (self-guarded): the single-flight fetch flags are
    // GLOBAL, so a fetch busy on another section must not permanently starve this one
    if crate::browse::letters().is_empty() {
        crate::browse::kick_letters();
    }
    if matches!(menu(), Menu::Genre { .. }) && crate::browse::genres().is_empty() {
        crate::browse::kick_genres();
    }
    unsafe {
        // springs: focused pop + the one shrinking previous cell (NOT a spring per cell — a
        // paged grid has unbounded rows; two springs give the same read on the A53 budget)
        (*addr_of_mut!(HEAD_S)).step(grid_head_lift_target(), K_SCALE, dt);
        // **The target is the focus scale only while the GRID actually holds the band.** It was
        // unconditional, so the spring sat at full magnification the whole time focus was on the
        // chip, the strip or the Sort/Filter row — and the first grid tile focus landed on was
        // therefore drawn already popped, with no transition, while every tile-to-tile step
        // animated normally (`grid_focus_moved` reseats it at 1.0). Relaxing it while the grid is
        // not focused costs nothing and is invisible: `focused_card_drawn` draws no focused card in
        // those zones at all. The shelves never had the fault — `CardRow::update` is handed
        // `None` for an unfocused row and relaxes every cell for free.
        let focus_target = if matches!(area(), Area::Grid | Area::Rail) {
            RowStyle::HOME.focus_scale
        } else {
            1.0
        };
        (*addr_of_mut!(FOCUS_S)).step(focus_target, K_SCALE, dt);
        (*addr_of_mut!(PREV_S)).step(1.0, K_SCALE, dt);
        if (*addr_of!(PREV_S)).pos < 1.003 {
            PREV_IDX = -1;
        }

        // **Vertical scroll: ROW SNAPPING, through `Layout`.** A focused row's target is its own
        // top edge rather than the minimal reveal `card_row::reveal` performs — which is right for
        // a shelf inside a page and wrong for a wall of posters you walk down. `card_row::reveal`
        // is untouched and Home still uses it.
        //
        // The bottom-clearance invariant the old `max_y`/`lo` pair spent a paragraph on is now
        // `Layout`'s: both the clamp and the target come out of one document height, so the last
        // row's caption cannot end up off-panel because two expressions drifted.
        // **The scroll target is a function of the zone focus is in** — which is what makes BACK's
        // head-of-document move a focus change and nothing more.
        // …and it reads the SETTLED document, never `lay`: `lay` is where the bands are THIS
        // frame, and a target chasing a moving document is a row that arrives and then drifts.
        let slay = layout_settled();
        let want = match area() {
            // rail jumps scroll the grid too
            Area::Grid | Area::Rail => slay.row_reveal(GR),
            Area::Shelf => slay.shelf_reveal(SHELF_F),
            // the grid's own control row: show the grid block from its heading down
            Area::Toolbar => (slay.grid_block_top()).clamp(0.0, slay.max_scroll()),
            // the library chip heads the document, and the read-out stands in for the whole
            // content region — both are the top of the page
            Area::LibChip | Area::Status => slay.head(),
            // **The shared top BAR does not scroll the page.** Walking up to the strip is not a
            // request to go to the top of the library, and the canvas keeps `scrollY` unchanged for
            // exactly this zone (`setFocus`: `f.zone === "tabs" ? this.state.scrollY : …`). BACK is
            // what returns to the head, and it does it by moving focus to the first CONTENT zone.
            Area::Chip | Area::Tabs => (*addr_of!(SCROLL)).pos,
        };
        (*addr_of_mut!(SCROLL)).step(want, K_SCROLL, dt);

        // every shelf's own springs — the focus pop, its horizontal scroll and its heading lift.
        // ALL of them every frame, not just the focused one: a row being left keeps animating
        // after it has stopped being the focused one, which is `CtlPop`'s rule and `CardRow`'s.
        let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
        for i in 0..MAX_SHELVES {
            let n = shelves.get(i).map(|s| s.items.len()).unwrap_or(0);
            let focused = (area() == Area::Shelf && SHELF_F == i).then_some(SHELF_C);
            // its OWN style: the slot pitch and the tile width feed `scroll_into_view`, so a
            // landscape row stepped as a poster row scrolls to the wrong place
            (*addr_of_mut!(SHELF_ROWS))[i].update(n, focused, shelf_style(i), dt);
        }

        // the letter rail's own scroll, for the sections whose alphabet does not fit at
        // `RAIL_PITCH`. Same reveal rule as the grid above, in letter units: keep the driving
        // letter one whole letter clear of each end, so it never sits in the edge fade.
        let drive = if area() == Area::Rail {
            RAIL_F
        } else {
            letter_of(focus_idx())
        };
        let ln = crate::browse::letters().len().min(MAX_LETTERS);
        let want_rs = rail_scroll_target((*addr_of!(RAIL_SCROLL)).pos, drive, ln);
        (*addr_of_mut!(RAIL_SCROLL)).step(want_rs, K_SCROLL, dt);
        // …and the rail's own presence, which now follows the REGION focus is in
        let rail_want = if rail_on() && !menu_open() && readout() == Readout::Grid && rail_region() {
            1.0
        } else {
            0.0
        };
        (*addr_of_mut!(RAIL_A)).step(rail_want, K_SCROLL, dt);

        // remember the view for re-entry (state amnesia is the official app's #2 complaint)
        crate::browse::save_view(focus_idx(), (*addr_of!(SCROLL)).pos);
    }
    // the shared tab strip's horizontal scroll — with more libraries than fit the row, this is
    // what reaches the far pills. Drawn from `.pos`; the draw pass runs at dt=0. Off the tab row
    // it tracks THIS section's pill, which `enter` already put on screen, so it holds still.
    // the VIEW section, so the strip travels to the pressed pill on the press frame while the grid
    // dissolves under it
    crate::ui::widgets::tab_row_update(wanted_tab() as c_int + 1, top_focus(), dt);
    if menu_open() {
        // The panel's appear spring and its list's springs are the MENU's motion, not the grid's
        // behind it — see `popover::own_motion`. Without this every frame of the entry ramp read
        // as page damage and threw the frozen grid away.
        let _own = crate::ui::popover::own_motion();
        pop().update(dt);
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        table().update(dt, table_rect().h);
        // async menu data landing while the popover is open — rebuild it in place (a Sort
        // menu stuck on "Loading…" whose OK secretly toggled the direction was a
        // review-confirmed bug). Two matches over one enum, the `chip_menu`/`open_chip_menu`
        // idiom: one DECIDES and is host-gradeable, one PERFORMS and measures text. A new menu
        // stops both compiling.
        //
        // Nothing is bound out of the variants here, deliberately: each of these rebuilds REPLACES
        // `MENU` — key and all — so a field borrowed from the scrutinee would be dropped underneath
        // the arm that is still running (see [`menu`]).
        if menu_stale() {
            match menu() {
                Menu::Sort { .. } => build_sort_menu(true),
                Menu::Genre { .. } => build_genre_menu(true),
                // a source's libraries landing while the list is open (its discovery is a worker) —
                // rebuilt in place, same rule as a menu's value list arriving
                Menu::Source { .. } => build_source_menu(true),
                Menu::Filter | Menu::None => {}
            }
        }
    }
    // ---- the page ground, LAST and OUTSIDE every branch above: it keys off [`focused_item`], and
    // everything before it can move the focus this frame (the fade's commit teleports GR/GC, the
    // pump lands the page the ring is standing on, a shelf step moves the column). Keyed earlier it
    // would spend a frame on the item focus was leaving; keyed inside one of those branches — which
    // is where it first went, inside `if menu_open()` — it would only ever run with a menu up, and
    // the page would simply have no ground. That failure is invisible on the host and silent in the
    // log: the wash stays flat, `is_flat` skips the pass, and the screen looks exactly as it did
    // before the feature existed.
    unsafe {
        (*addr_of_mut!(GROUND)).key(
            focused_item().filter(|m| m.has_blur).map(|m| m.blur),
            PageGround::CARD_W,
            dt,
        )
    };
}

// ---- input: D-pad ---------------------------------------------------------------------------

pub(crate) fn move_focus(sym: c_uint) {
    if menu_open() {
        if sym == SDLK_UP {
            table().move_sel(-1);
        } else if sym == SDLK_DOWN {
            table().move_sel(1);
        }
        // The selection moved and nothing behind the menu did — `popover::note_own_damage`.
        crate::ui::popover::note_own_damage();
        return;
    }
    let lay = layout();
    unsafe {
        match area() {
            Area::Chip => {
                // The bar's leftmost stop: LEFT has nowhere to go, RIGHT is the only way back into
                // the pills, and DOWN leaves the band by exactly the rule the pills use — the same
                // [`below_top_bar`] call, not a restatement of it, so the two exits cannot drift.
                if sym == SDLK_RIGHT {
                    AREA = Area::Tabs;
                    set_tab_p(Pill::Home);
                } else if sym == SDLK_DOWN {
                    step_down(&lay);
                }
            }
            Area::Tabs => {
                // Home + every section: the row scrolls the far pills into view, so focus may
                // walk to the last one (it used to stop at a hard cap of 4 sections)
                let n = crate::ui::widgets::tab_count();
                // the walk is by POSITION and the result is stored as an IDENTITY: stepping is a
                // question about the row drawn right now, and only the answer outlives the frame
                let i = tab_idx();
                if sym == SDLK_LEFT {
                    // …and off the FIRST pill, LEFT reaches the profile chip beside it — the same
                    // walk Home has always had, now that the chip is a stop here too
                    if i > 0 {
                        set_tab_p(crate::ui::widgets::pill_at(i - 1));
                    } else {
                        AREA = Area::Chip;
                    }
                } else if sym == SDLK_RIGHT && i + 1 < n {
                    set_tab_p(crate::ui::widgets::pill_at(i + 1));
                } else if sym == SDLK_DOWN {
                    step_down(&lay);
                }
            }
            Area::LibChip => {
                // ONE control at the head of the document: nothing sideways, and the vertical walk
                // is the projection's — which is what makes an empty library (no grid, no controls)
                // land DOWN on nothing rather than on an undrawn Sort chip.
                if sym == SDLK_UP {
                    AREA = Area::Tabs;
                    set_tab_p(own_pill_id());
                } else if sym == SDLK_DOWN {
                    step_down(&lay);
                }
            }
            Area::Shelf => {
                let n = shelf_len(SHELF_F);
                if sym == SDLK_LEFT && SHELF_C > 0 {
                    SHELF_C -= 1;
                    note_shelf_id();
                } else if sym == SDLK_RIGHT && SHELF_C + 1 < n {
                    SHELF_C += 1;
                    note_shelf_id();
                } else if sym == SDLK_UP || sym == SDLK_DOWN {
                    // UP and DOWN walk the SHELVES, and the projection is what carries focus off
                    // the last one into the grid block below or back up to the chip above — the
                    // shelf list is a run of identical zones in it, so the index moves here and the
                    // zone changes only at either end.
                    let up = sym == SDLK_UP;
                    if up && SHELF_F > 0 {
                        SHELF_C = shelf_column_near(SHELF_F, SHELF_C, SHELF_F - 1);
                        SHELF_F -= 1;
                        note_shelf_id();
                    } else if !up && SHELF_F + 1 < lay.shelves {
                        SHELF_C = shelf_column_near(SHELF_F, SHELF_C, SHELF_F + 1);
                        SHELF_F += 1;
                        note_shelf_id();
                    } else if up {
                        step_up(&lay);
                    } else {
                        step_down(&lay);
                    }
                }
            }
            Area::Toolbar => {
                // walk from the DRAWN index (`tool_f`), never from the raw static: a step taken
                // from a stale one moves the ring by more than one chip, or by none at all
                let f = tool_f();
                if sym == SDLK_LEFT && f > control_lo(chips()) {
                    TOOL_F = f - 1;
                } else if sym == SDLK_RIGHT && f + 1 < chips().len() {
                    TOOL_F = f + 1;
                } else if sym == SDLK_RIGHT && rail_drawn() {
                    // **RIGHT off the LAST chip reaches the alphabet**, exactly as RIGHT off the
                    // grid's last column does. The rail is already drawn here — `rail_region`
                    // counts the control row, because Sort and Filter act on the grid the rail
                    // indexes — so it was a control the user could SEE and could not reach without
                    // first going down into the grid and back out sideways.
                    enter_rail(Area::Toolbar);
                } else if sym == SDLK_UP {
                    step_up(&lay);
                } else if sym == SDLK_DOWN {
                    step_down(&lay);
                }
            }
            Area::Grid => {
                let (r, c) = (GR, GC);
                if sym == SDLK_LEFT && GC > 0 {
                    GC -= 1;
                } else if sym == SDLK_RIGHT {
                    if GC + 1 < cols_in_row(GR) {
                        GC += 1;
                    } else if rail_drawn() {
                        // RIGHT off the last column enters the letter rail at the current letter
                        enter_rail(Area::Grid);
                        return;
                    }
                } else if sym == SDLK_UP {
                    if GR > 0 {
                        GR -= 1;
                    } else {
                        step_up(&lay);
                        return;
                    }
                } else if sym == SDLK_DOWN && GR + 1 < n_rows() {
                    GR += 1;
                }
                clamp_focus();
                if (GR, GC) != (r, c) {
                    grid_focus_moved(r * COLS + c);
                }
            }
            Area::Rail => {
                let n = crate::browse::letters().len().min(MAX_LETTERS); // never beyond the drawn rects
                if sym == SDLK_UP && RAIL_F > 0 {
                    rail_jump(RAIL_F - 1); // live jump: the grid follows the letter
                } else if sym == SDLK_DOWN && RAIL_F + 1 < n {
                    rail_jump(RAIL_F + 1);
                } else if sym == SDLK_LEFT {
                    // back where the rail was entered from — the grid at the jumped position, or
                    // the control row if that is where RIGHT was pressed. A rail entered from the
                    // chips that only ever exits into the grid is a one-way door.
                    AREA = addr_of!(RAIL_FROM).read();
                }
            }
            Area::Status => {
                // ONE control, so there is nowhere to walk sideways or down to. UP leaves the
                // read-out for the chrome above it, which never went down — the toolbar when it has
                // a chip to hold, else the tab strip. The read-out therefore spends no line telling
                // the user how to get out: every exit is a control they can already see.
                if sym == SDLK_UP {
                    step_up(&lay);
                }
            }
        }
    }
}

/// **One step of the `libosc` sweep, reversing at the DOCUMENT's own ends rather than on a clock.**
///
/// `homeosc` and `searchosc` flip direction every 3 s, which is right for a shelf column whose
/// whole extent is a few presses. It is wrong here and became wrong with this screen: the document
/// now opens with the library chip and one rung per shelf before the grid begins, so at the
/// oscillator's 350 ms cadence a 12-shelf library spends more than four seconds reaching the grid
/// and reverses before it gets there. The scene that exists to sweep the seam between the last
/// shelf and the poster wall would then never cross it, and an FPS floor over a sweep that cannot
/// reach the thing it grades approves nothing.
///
/// The end test is the walk's own answer rather than a second expression for "the bottom": take the
/// focus signature, step, and if nothing moved we are against an end — flip and step again. That
/// cannot disagree with the projection, and it needs no endpoint arithmetic that would have to be
/// kept in step with `Layout`.
pub(crate) fn osc_step() {
    static mut DOWN: bool = true;
    // every cursor the walk can move, so "nothing moved" is a statement about focus and not about
    // one zone's idea of it
    fn sig() -> (Area, usize, usize, usize, usize, usize, usize) {
        unsafe {
            (
                area(),
                SHELF_F,
                SHELF_C,
                tool_f(),
                GR,
                GC,
                addr_of!(RAIL_F).read(),
            )
        }
    }
    let sym = |down: bool| if down { SDLK_DOWN } else { SDLK_UP };
    let before = sig();
    let down = unsafe { addr_of!(DOWN).read() };
    move_focus(sym(down));
    if sig() == before {
        unsafe { DOWN = !down };
        move_focus(sym(!down));
    }
}

/// CH▲/CH▼ page jump: one screenful of rows per press.
pub(crate) fn page(dir: i32) {
    if menu_open() || area() != Area::Grid {
        return;
    }
    unsafe {
        let old = focus_idx();
        // a visible screen holds ~1.84 rows — round, don't truncate (a truncated "page" of 1
        // row was identical to plain UP/DOWN)
        let step = (((SCR_H - GRID_TOP) / PITCH).round().max(1.0)) as usize;
        if dir > 0 {
            GR = (GR + step).min(n_rows().saturating_sub(1));
        } else {
            GR = GR.saturating_sub(step);
        }
        clamp_focus();
        if focus_idx() != old {
            grid_focus_moved(old);
        }
    }
}

/// Hand the pop spring to the newly-focused cell; the old cell shrinks via PREV.
fn grid_focus_moved(old_idx: usize) {
    unsafe {
        PREV_IDX = old_idx as i64;
        PREV_S = addr_of!(FOCUS_S).read();
        FOCUS_S = Spring::at(1.0);
    }
}

// ---- input: OK / BACK -----------------------------------------------------------------------

pub(crate) fn focus_is_card() -> bool {
    // **The shelves count, and they have to.** `app.rs` ARMS the press-and-hold from this
    // predicate (`press::arm`'s `Route::Library` term), so a surface it excludes never becomes a
    // hold at all — the key-up arrives as an ordinary tap and the tile opens Detail. That is what a
    // held OK on a shelf tile did on the television until this said Shelf as well: the item menu
    // was unreachable there, and with it *Remove from Continue Watching*, which is the one row a
    // section deck exists to offer.
    !menu_open() && matches!(area(), Area::Grid | Area::Shelf) && focused_item().is_some()
}

/// What a press on strip pill `i` means for this screen. ONE function for the OK key and the
/// pointer click, which spelled the same three-way ladder out separately and identically — the
/// shape every other paired key/pointer path on this screen already avoids (`click` resolves the
/// toolbar through the same `open_chip_menu` the key does, and for the same reason).
fn tab_pill_action(i: usize) -> Action {
    match crate::ui::widgets::pill_at(i) {
        // the screen the strip leads with; leaving for it is `app.rs`'s call, not ours
        Pill::Home => Action::GoHome,
        Pill::Search => Action::GoSearch,
        // a TAB, not a section: `switch_tab` resolves it through `browse::tabs`, because several
        // libraries can share one pill
        Pill::Section(kind) => {
            switch_tab(kind);
            Action::None
        }
    }
}

pub(crate) fn on_ok() -> Action {
    if menu_open() {
        menu_commit(table().sel);
        return Action::None;
    }
    match area() {
        // The chip's press is the SAME press on all three screens that wear the bar, so `app.rs`
        // answers it once, ahead of this ladder (`key_ok`'s `TopFocus::Chip` arm) — the destination
        // is the account menu wherever you are standing, unlike a pill, whose destination depends
        // on the screen. This arm is therefore unreachable in practice and stays for totality.
        Area::Chip => Action::None,
        Area::Tabs => tab_pill_action(tab_idx()),
        Area::Toolbar => {
            // matched on the CHIP, never on its index: the row is two chips long with one source
            // and three with two, so position is not identity
            // `None` only when the row is empty, which is the one-source failure — and then the
            // toolbar is not a band focus can be in at all (`sync_readout_focus`)
            if let Some(c) = focused_chip() {
                open_chip_menu(c);
            }
            Action::None
        }
        Area::Grid => Action::Card,
        // ONE control at the head of the document: OK opens the Sources picker, which is the only
        // thing the chip does.
        Area::LibChip => {
            open_chip_menu(Chip::Source);
            Action::None
        }
        Area::Shelf => Action::ShelfCard {
            from_deck: unsafe {
                crate::browse::section_hubs::shelves(crate::browse::cur())
                    .get(SHELF_F)
                    .map(|s| s.is_continue)
                    .unwrap_or(false)
            },
        },
        Area::Rail => {
            unsafe { AREA = Area::Grid }; // OK on a letter lands in the grid at that title
            Action::None
        }
        Area::Status => {
            retry_source();
            Action::None
        }
    }
}

/// BACK inside the screen. Returns false when the press should leave to Home (tab bar was
/// already focused) — the Netflix-2025 escape rule: first BACK reaches the tab bar.
pub(crate) fn back() -> bool {
    match menu() {
        Menu::Genre { .. } => {
            open_filter_menu(true);
            return true;
        }
        // the Sources panel has no level to walk back through at all any more — it is the picker
        // and nothing else, so BACK dismisses it exactly as it does the other two
        Menu::Source { .. } | Menu::Sort { .. } | Menu::Filter => {
            close_menu();
            return true;
        }
        Menu::None => {}
    }
    // **BEFORE the commit, BACK CANCELS and is consumed; after it, BACK is the ordinary
    // head-of-library move.** The rule is temporal rather than spatial — "above/below the floor"
    // was the loose way to say it. Below the floor the outgoing content is already gone, so
    // cancelling would restore a page the user has left; before it, the user changed their mind
    // inside the window and nothing has happened yet.
    //
    // The whole transaction goes: the queued action, the fade, and the menu it was built for.
    if xf().is_swapping() && cancel_pending() {
        return true;
    }
    // Nothing to cancel, so this BACK either walks out of the screen or to the head of the
    // document — either way a queued reload must land now rather than be dropped by a route change
    // (and the walk below reads `cur()`, which [`flush`] has just made agree with the chrome).
    flush();
    if area() == Area::Rail {
        unsafe { AREA = Area::Grid };
        return true;
    }
    // **BACK is a move to the HEAD OF THE DOCUMENT, not a walk up a ladder of zones.**
    //
    // One deliberate widening of the canvas, which handles only `grid` and `gridtool`: a SHELF gets
    // the same move. A deeply scrolled shelf is as far from the top of the page as a grid row is,
    // and D's rule is about the document rather than about which block you happen to be standing
    // in — a BACK that worked in the grid and did nothing two shelves up would read as the key
    // being broken there.
    //
    // With one continuous scroll the old "walk to the tab bar first" step is the wrong shape: a
    // deeply scrolled SHELF is as far from the top of the page as a grid row is, and D's rule is
    // about the document rather than about which block you happen to be in. So anything below the
    // head returns to it — animated on the scroll spring, never cut — and only a BACK taken AT the
    // head leaves for Home.
    //
    // The Status read-out is the one zone with no head to return to: it stands in for the whole
    // content region, and C already says the chip and the strip above it are the way out.
    let lay = layout();
    // the head of the LIBRARY is its first content zone, never the shared bar above it
    let head = lay.first_content();
    let at_head = matches!(area(), Area::Tabs | Area::Chip | Area::Status)
        || head.is_none()
        || (Some(area()) == head && at_document_head());
    if !at_head {
        unsafe {
            AREA = head.unwrap_or(Area::Tabs);
            if AREA == Area::Tabs {
                set_tab_p(own_pill_id()); // `flush` above has made the chrome and the store agree
            }
            enter_zone(&lay, AREA, 1);
            // the scroll follows on its own spring — D: "animated, not cut". It is `update`'s
            // rule that carries it there: the target is a function of the zone focus is IN, so
            // moving focus to the head IS the scroll instruction. One rule, not two that must
            // agree about where the top of the page is.
        }
        return true;
    }
    false
}

/// **Is the document SETTLED at its head?** The shared predicate, read by [`back`] and by
/// `section_hubs`' visible staged-commit rule, so the two can never drift into disagreeing about
/// when the page is at the top.
///
/// It reads the settled target and velocity rather than a bare `scroll == 0.0`, which a fast scroll
/// passes straight THROUGH on its way past — committing a set of shelves in that instant would move
/// the ground under a reader who never stopped.
pub(crate) fn at_document_head() -> bool {
    let sp = unsafe { addr_of!(SCROLL).read() };
    sp.pos.abs() < 1.0 && sp.vel.abs() < 1.0
}

// ---- menus (Popover + TableView, all server-driven) -----------------------------------------

/// Sort/Filter panel width — one-line rows.
const MENU_W: f32 = 470.0;
/// The Sources panel is wider **because its rows are two-line**: a library's name over its size,
/// under a machine-name header carrying an owner. 470 elides all three.
const SRC_W: f32 = 640.0;
/// The open panel's frame — **anchored under the chip it was opened FROM**, which now scrolls.
///
/// It hung off a constant `TOOL_Y + TOOL_H + 18`, correct while every chip sat on a fixed toolbar.
/// With the controls inside the document a menu anchored there would float away from its own chip
/// the moment the page moved — the panel is what the chevron points at, so it has to follow it.
///
/// `TOOL_RECTS` holds where each chip was DRAWN this frame; the fallback is the old constant, for
/// the frame before anything has been drawn.
fn panel_rect() -> Rect {
    let w = if source_menu_open() { SRC_W } else { MENU_W };
    let ph = table().measured_height().clamp(120.0, SCR_H - 280.0);
    let anchor = menu_anchor();
    // …and clamped into the panel: a chip scrolled near the bottom edge must not push its own menu
    // off the screen, which is the one thing a fixed anchor could never do wrong.
    let y = anchor.1.min(SCR_H - MARGIN_Y - ph);
    Rect::new(anchor.0, y.max(crate::ui::widgets::TOP_BAR_BOTTOM), w, ph)
}

/// **Where the open menu hangs from — LATCHED at open, and deliberately not re-read after.**
///
/// The chip is inside the scroll on this screen, so it MOVES: applying a filter re-queries the
/// grid, the document re-seats, and the toolbar travels down the page. Re-reading it every frame
/// sprang the panel after it — measured in the simulator at 529 → 671 → 808 over about twenty
/// frames — while the table laid its rows against the rect it had, so the panel's glass body and
/// its own rows came apart in flight. Photographed on the television 2026-09-05: *"during
/// filtering, menu explodes"*.
///
/// **Following the chip could never have been right, and the reason is the host snapshot.** This
/// popover is `caching_host`: the page behind the scrim is FROZEN at the moment the menu opened, so
/// the chip the viewer is looking at is at its old place and the live one the anchor was tracking
/// is not on the screen at all. A panel that chases the live chip walks away from the only chip
/// anybody can see. Freezing the anchor is the same statement as freezing the page.
///
/// The latch is keyed by the CHIP, not by a bare open flag, because Filter drills into Genre
/// without closing (they share `Chip::Filter`, so the submenu correctly keeps the panel in place)
/// while Sort and Sources are different chips and must re-anchor. Dropped by [`close_menu`], and
/// re-taken lazily on the first read of the next menu — so nothing has to remember to arm it.
fn menu_anchor() -> (f32, f32) {
    let chip = menu_chip();
    if let Some((c, x, y)) = unsafe { addr_of!(MENU_ANCHOR).read() } {
        if Some(c) == chip {
            return (x, y);
        }
    }
    let a = chip
        .and_then(|i| unsafe { addr_of!(TOOL_RECTS).read() }.get(i).copied())
        .filter(|r| r.w > 0.5)
        .map(|r| (r.x, r.y + r.h + 18.0))
        .unwrap_or((MARGIN_X, CONTENT_TOP + TOOL_H + 18.0));
    if let Some(c) = chip {
        unsafe { *addr_of_mut!(MENU_ANCHOR) = Some((c, a.0, a.1)) };
    }
    a
}

/// The latched anchor: `(chip index, x, y)`. See [`menu_anchor`].
static mut MENU_ANCHOR: Option<(usize, f32, f32)> = None;
fn table_rect() -> Rect {
    panel_rect()
}
/// Dismiss the open menu. **The key goes with it** — it lives in the [`Menu`] variant, so
/// `Menu::None` is not a flag sitting beside a row count and an action list that describe a panel
/// nobody can see any more; it is the absence of them.
fn close_menu() {
    set_menu(Menu::None);
    // The anchor goes with it — see [`menu_anchor`]. Dropped here rather than re-derived on open,
    // so a menu that is never opened again leaves nothing behind.
    unsafe { *addr_of_mut!(MENU_ANCHOR) = None };
    pop().close();
}

/// **Has the data under the OPEN menu moved since it was built?** The rebuild-on-landing test,
/// split out of [`update`] as a value so it can be graded on the host — the rebuild itself goes
/// through `TableView`, which measures text.
///
/// Every arm compares LIKE WITH LIKE, which is the whole content of this function: a menu's key is
/// whatever it recorded at build time and nothing else. Sort and Genre record the length of the
/// list their rows are; the Sources panel records the section-table GENERATION, because its rows
/// are a kind-filtered projection whose count matches no other count in the app (see
/// [`Menu::Source`]'s `gen` for the frame-per-frame rebuild that cost). `Filter` is static rows
/// built from `view_*`, so it has no landing to wait for, carries no key and is never stale.
///
/// Reading the key off the OPEN variant is what makes "like with like" structural: there is no way
/// to reach Sort's count while the Sources panel is up, because while it is up that count does not
/// exist.
fn menu_stale() -> bool {
    match menu() {
        Menu::Sort { built } => *built != crate::browse::sorts().len(),
        Menu::Genre { built } => *built != crate::browse::genres().len(),
        Menu::Source { gen, .. } => *gen != crate::browse::source_list_gen(),
        Menu::Filter | Menu::None => false,
    }
}

// ---- the Sources list ------------------------------------------------------------------------
//
// The ROW MODEL is `ui::source_list` — the same builder the first-run route mounts, so the panel
// and that route cannot drift on which column carries a mark, which carries a word, or what an
// unreachable group says. What is left here is the panel: its level state, its cursor, and the
// popover it lives in.

/// Build (or rebuild in place) the Library picker. Home membership is edited only from Settings.
fn build_source_menu(keep: bool) {
    // READ THE GENERATION FIRST, before the projections it stamps. `source_groups`/`source_rows`
    // only read, so nothing can move between here and the build today — but recording it after
    // would silently adopt a landing this panel was not built from, and that is a rebuild lost
    // rather than one too many.
    let table_gen = crate::browse::source_list_gen();
    let groups = crate::browse::source_groups();
    let rows = crate::browse::source_rows();
    let (secs, acts) = source_list::sections(Level::Browse, &groups, &rows, Tail::Recheck);
    let sel = source_list::sel(&rows, keep.then(|| table().sel));
    // the panel IS its key: the generation these rows were projected from and one action per row,
    // installed together and dropped together by [`close_menu`]
    set_menu(Menu::Source {
        gen: table_gen,
        acts,
    });
    table().compact = false; // two-line rows: HEADLINE titles over CAPTION sub-lines
                             // `slide` = `keep`, and that is the level swap's whole promise kept in one argument: a rebuild
                             // GLIDES (so the panel's scroll and highlight hold still while the words under them change),
                             // an OPEN snaps to the current library with the list scrolled to the top.
    table().set_sections(secs, sel, keep);
    table().list_focused = true;
    if !keep {
        pop().open();
    }
}

/// Open the Sources list as a pure Library picker.
fn open_source_menu() {
    build_source_menu(false);
}

/// What the row at global index `sel` of the OPEN Sources panel does.
///
/// The list is read out of the open variant, so this can only ever answer for the panel that is on
/// screen — with any other menu up there is no list to index, rather than a leftover one that
/// happens to be indexable. The answer is COPIED out because every rebuild assigns `MENU` and drops
/// the list it was read from ([`build_source_menu`]).
fn src_action(sel: i32) -> SrcAction {
    match menu() {
        Menu::Source { acts, .. } => usize::try_from(sel)
            .ok()
            .and_then(|i| acts.get(i))
            .copied()
            .unwrap_or(SrcAction::None),
        _ => SrcAction::None,
    }
}

fn open_sort_menu() {
    build_sort_menu(false);
}

/// `keep` = rebuilding the already-open menu in place (the server sorts landed) — don't
/// restart the popover's appear animation.
fn build_sort_menu(keep: bool) {
    let sorts = crate::browse::sorts();
    let mut sec = Section::new("Sort by");
    if sorts.is_empty() {
        sec = sec.row(Row::new("Loading\u{2026}").dim(true));
    }
    for (i, s) in sorts.iter().enumerate() {
        let active = i == crate::browse::sort_idx();
        let mut row = Row::new(s.title.clone()).checked(active);
        if active {
            // direction as the chevron ASSET (per the icon rule), up = asc; OK again flips it
            row = row.ticon(if crate::browse::sort_desc() {
                Icon::ChevronDown
            } else {
                Icon::ChevronUp
            });
        }
        sec = sec.row(row);
    }
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], crate::browse::sort_idx() as i32, keep);
    set_menu(Menu::Sort { built: sorts.len() });
    if !keep {
        pop().open();
    }
}

/// `keep` = re-entered from the Genre level via BACK (slide the pill, don't restart the fade).
/// Does the section being browsed answer "unwatched"? Only movies and shows have that state, so on
/// a music or photo library the switch is not offered rather than offered and inert — `browse`
/// refuses to put the filter on such a query, and a control that visibly changes nothing is a worse
/// answer than one that is not there. It also shifts the Genre row up, which is why every index in
/// this menu is resolved through [`filter_rows`] rather than written as a literal.
fn has_unwatched() -> bool {
    matches!(
        crate::browse::section_kind(crate::browse::cur()),
        Some(crate::browse::SecKind::Movie) | Some(crate::browse::SecKind::Show)
    )
}

/// The Filter menu's row map: which row index means what, for the section being browsed.
fn filter_rows() -> (i32, i32) {
    if has_unwatched() {
        (0, 1) // unwatched, genre
    } else {
        (-1, 0) // no switch; genre leads
    }
}

fn open_filter_menu(keep: bool) {
    let mut sec = Section::new("Filter");
    if has_unwatched() {
        // A SWITCH, not one option among several: it says what it is set to in words at the trailing
        // edge, and carries no mark at all in the leading column.
        sec = sec.row(Row::new("Unwatched only").toggle(view_unwatched()));
    }
    let sec = sec.row(
        // "All" / "Comedy" is a READ-OUT — what this row is set to — so it belongs in the same
        // trailing slot as the switch above it, not on a sub-line. It was a sub-line until the
        // 2026-08-13 sync, which put the two rows of this one menu in the odd position of
        // answering "what is set" in two different places; a sub-line is for a DESCRIPTION
        // (the track menu's "EAC3 · 5.1"), and it also costs the row 32px of height it was
        // spending to say one word.
        Row::new("Genre")
            .value(
                crate::browse::genre_sel()
                    .map(|g| g.title.clone())
                    .unwrap_or_else(|| "All".into()),
            )
            .chevron(true),
    );
    let sel = if keep { 1 } else { 0 };
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], sel, keep);
    set_menu(Menu::Filter); // static rows, so no key: there is no landing to be stale against
    if !keep {
        pop().open();
    }
}

fn build_genre_menu(keep: bool) {
    crate::browse::kick_genres();
    let genres = crate::browse::genres();
    let selected = crate::browse::genre_sel().map(|g| g.id.clone());
    let mut sec = Section::new("Genre").row(Row::new("All Genres").checked(selected.is_none()));
    if genres.is_empty() {
        sec = sec.row(Row::new("Loading\u{2026}").dim(true));
    }
    let mut sel = 0i32;
    for (i, g) in genres.iter().enumerate() {
        let active = selected.as_deref() == Some(g.id.as_str());
        if active {
            sel = i as i32 + 1;
        }
        sec = sec.row(Row::new(g.title.clone()).checked(active));
    }
    table().compact = true; // mock spec: menu rows read at BODY regular, not menu-bold
    table().set_sections(vec![sec], sel, keep);
    set_menu(Menu::Genre {
        built: genres.len(),
    });
    if !keep {
        pop().open();
    }
}

fn menu_commit(sel: i32) {
    match menu() {
        Menu::Source { .. } => match src_action(sel) {
            SrcAction::Library(s) => {
                switch_to_section(s);
                close_menu();
            }
            SrcAction::Recheck => {
                crate::browse::recheck_shares();
                build_source_menu(true);
            }
            SrcAction::None => {}
        },
        Menu::Sort { built } => {
            // gate on the row set the TABLE was built with, not the live sorts() — OK on the
            // "Loading…" row must be a no-op, never a hidden direction toggle. Copied out of the
            // variant first: `close_menu` below drops the menu this key belongs to.
            let built = *built;
            if built > 0 && sel >= 0 && (sel as usize) < built {
                // the stable KEY and the direction the press produces, resolved HERE against the
                // menu the user is looking at — never an index carried to another section
                if let Some(e) = crate::browse::sorts().get(sel as usize) {
                    let (cur_key, cur_desc) = crate::browse::sort_key_now();
                    // re-picking the ACTIVE entry flips the direction; picking another takes its
                    // own default. `browse::set_sort`'s rule, stated rather than inherited.
                    let desc = if e.key == cur_key {
                        !cur_desc
                    } else {
                        e.default_desc
                    };
                    request_grid(GridAct::Sort {
                        key: e.key.clone(),
                        desc,
                    });
                }
            }
            close_menu(); // the panel closes NOW; the grid dissolves under it
        }
        Menu::Filter => {
            let (unwatched_row, genre_row) = filter_rows();
            if sel == unwatched_row {
                request_grid(GridAct::Unwatched(!view_unwatched()));
                open_filter_menu(true); // rebuilt from `view_unwatched()` — its `On`/`Off` flips at once
                table().sel = unwatched_row;
            } else if sel == genre_row {
                build_genre_menu(false);
            }
        }
        Menu::Genre { .. } => {
            let genres_n = crate::browse::genres().len();
            if sel == 0 {
                request_grid(GridAct::Genre(None));
                close_menu();
            } else if !crate::browse::genres().is_empty() && (sel as usize) <= genres_n {
                let id = crate::browse::genres()
                    .get(sel as usize - 1)
                    .map(|g| g.id.clone());
                request_grid(GridAct::Genre(id));
                close_menu();
            }
        }
        Menu::None => {}
    }
}

// ---- pointer --------------------------------------------------------------------------------

/// Hover. **Returns whether the focus STOP moved**, which is what an armed press has to know:
/// `ui::press`'s contract is that focus cannot move mid-press, the nav keys pay it by calling
/// `press::cancel`, and hover owes the same. Without it, arming a press on a card and then sliding
/// the Magic Remote's pointer onto another one committed the click — or opened the press-and-hold
/// menu — on a tile the user was no longer pressing. `detail::pointer_focus` is the precedent and
/// the shape is deliberately identical; the other hovering screens (`home`, `person`, `search`)
/// still return nothing and carry the same gap, which is a wider change than this branch.
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    if menu_open() {
        if let Some(gi) = table().hit_row(table_rect(), mx, my) {
            table().sel = gi;
            crate::ui::popover::note_own_damage();
        }
        return false;
    }
    let before = (area(), unsafe {
        (addr_of!(SHELF_F).read(), addr_of!(SHELF_C).read())
    }, focus_idx());
    let moved = |b: (Area, (usize, usize), usize)| {
        b != (area(), unsafe {
            (addr_of!(SHELF_F).read(), addr_of!(SHELF_C).read())
        }, focus_idx())
    };
    unsafe {
        if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
            AREA = Area::Tabs;
            set_tab_p(crate::ui::widgets::pill_at(i));
            return moved(before);
        }
        let tools = addr_of!(TOOL_RECTS).read();
        // `w > 0.5` rather than a slice by the row's length: a chip that is not drawn parks its
        // rect, so the geometry itself says what is hittable and the scan cannot outlive a shorter
        // row (the failure read-out drops Sort and Filter — see `chips`).
        //
        // The LIBRARY chip records its rect in the same array and is its own zone, because it heads
        // the document rather than sitting in the grid's control row.
        if let Some(i) = tools.iter().position(|r| r.w > 0.5 && r.contains(mx, my)) {
            AREA = if chips().get(i) == Some(&Chip::Source) {
                Area::LibChip
            } else {
                Area::Toolbar
            };
            TOOL_F = i;
            return moved(before);
        }
        // …and the shelves, which had no hit test at all: a Magic Remote user could SEE rows they
        // had no way to reach.
        if let Some((i, k)) = shelf_at(mx, my) {
            AREA = Area::Shelf;
            SHELF_F = i;
            SHELF_C = k;
            note_shelf_id();
            crate::ui::idle::invalidate();
            return moved(before);
        }
        if status_btn_at(mx, my) {
            AREA = Area::Status;
            return moved(before);
        }
        if let Some(i) = rail_at(mx, my) {
            AREA = Area::Rail;
            RAIL_F = i; // hover focuses the letter; the click jumps
            return moved(before);
        }
        if let Some((r, c)) = cell_at(mx, my) {
            let old = focus_idx();
            AREA = Area::Grid;
            GR = r;
            GC = c;
            if focus_idx() != old {
                grid_focus_moved(old);
            }
        }
    }
    // a MISS leaves focus where it was, so it is not a move and must not abort a press — the same
    // trade `onboard::pointer_hold` makes for dead space
    moved(before)
}

/// The grid cell under the pointer, with home's fly-away guard: a partially-visible row is not
/// hoverable unless it is already the focused row (hovering it would scroll the page under the
/// pointer).
fn cell_at(mx: f32, my: f32) -> Option<(usize, usize)> {
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    let lay = layout();
    // **Through `Layout`, the same projection the draw used.** It read `my - GRID_TOP` — the grid's
    // own origin as a screen constant — which stops being true the moment a header and N shelves
    // sit above it: every click would land on a row `shelves * ROW_PITCH / PITCH` too far down.
    let g = lay.doc_to_grid(sc + my - CONTENT_TOP);
    // O(visible): only the two rows that can straddle the pointer's y, not the whole section
    let r_lo = ((g - CARD_H) / PITCH).floor().max(0.0) as usize;
    let r_hi = ((g / PITCH).floor().max(0.0) as usize + 1).min(n_rows());
    for r in r_lo..r_hi.min(n_rows()) {
        let y = lay.row_y(r, sc);
        if my < y || my > y + CARD_H {
            continue;
        }
        let fully_visible =
            y >= crate::ui::widgets::TOP_BAR_BOTTOM && y + CARD_H <= SCR_H - 20.0;
        if !fully_visible && r != unsafe { addr_of!(GR).read() } {
            continue;
        }
        for c in 0..cols_in_row(r) {
            let x = MARGIN_X + c as f32 * (CARD_W + LGAP);
            if mx >= x && mx <= x + CARD_W {
                return Some((r, c));
            }
        }
    }
    None
}

/// A SHELF tile under the pointer, as `(shelf, column)`. The document's other half, and it did not
/// exist before: a Magic Remote user could see shelves they had no way to click.
///
/// Built from the same `Layout` and the same per-row horizontal scroll the draw used, so a tile
/// that is drawn is a tile that is hittable — including the partially-scrolled ones at either end of
/// a shelf.
fn shelf_at(mx: f32, my: f32) -> Option<(usize, usize)> {
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    let lay = layout();
    let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
    for (i, sh) in shelves.iter().take(MAX_SHELVES).enumerate() {
        let sty = shelf_style(i);
        let top = doc_y(&lay, lay.shelf_origin(i), sc) + crate::ui::consts::CARD_DY;
        if my < top || my > top + sty.h {
            continue;
        }
        // the row's OWN scroll, so a shelf scrolled halfway still hits the tile that is drawn
        let sx = unsafe { (*addr_of!(SHELF_ROWS))[i].scroll_x() };
        let n = sh.items.len().min(card_row::MAX_ROW_ITEMS);
        for k in 0..n {
            let x = MARGIN_X + k as f32 * (sty.w + sty.gap) - sx;
            if mx >= x && mx <= x + sty.w {
                return Some((i, k));
            }
        }
    }
    None
}

/// Pointer click — same activation map as OK.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if menu_open() {
        if let Some(gi) = table().hit_row(table_rect(), mx, my) {
            table().sel = gi;
            menu_commit(gi);
        } else if !panel_rect().contains(mx, my) {
            // inside the panel but on no row (the level band's dead space) is not a dismissal
            close_menu();
        }
        return Action::None;
    }
    pointer_focus(mx, my);
    if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
        return tab_pill_action(i);
    }
    let tools = unsafe { addr_of!(TOOL_RECTS).read() };
    if let Some(i) = tools.iter().position(|r| r.w > 0.5 && r.contains(mx, my)) {
        // resolved through the CHIP the click landed on, exactly as the key path does. It used to
        // defer to `on_ok`, which reads the FOCUSED chip — `pointer_focus` above has just moved
        // focus there, so the two agreed by side effect rather than by construction.
        // `get`, not an index: the row can be EMPTY now (a one-source failure drops every chip it
        // has), and `saturating_sub(1)` would then index an empty slice. Unreachable today because
        // an undrawn chip parks its rect and the scan above tests `w > 0.5` — which is exactly the
        // kind of "safe by a fact somewhere else" that stops being true when the row changes again.
        if let Some(&c) = chips().get(i) {
            open_chip_menu(c);
        }
        return Action::None;
    }
    if status_btn_at(mx, my) {
        return on_ok(); // `pointer_focus` above has already parked the band on the pill
    }
    // a shelf tile: `pointer_focus` has parked focus on it, so `on_ok` resolves the same tile and
    // the same deck flag the remote would — one activation, two input paths
    if shelf_at(mx, my).is_some() {
        return on_ok();
    }
    if let Some(i) = rail_at(mx, my) {
        rail_jump(i); // pointer click on a letter = the jump
        return Action::None;
    }
    if cell_at(mx, my).is_some() {
        return Action::Card;
    }
    Action::None
}

/// Is the pointer on the failure read-out's Try again pill? Rect recorded at draw and parked off
/// the panel whenever the read-out is not up, so a stale one cannot be clicked (see [`STATUS_BTN`]).
fn status_btn_at(mx: f32, my: f32) -> bool {
    unsafe { addr_of!(STATUS_BTN).read() }.contains(mx, my)
}

/// The rail letter under the pointer, or None (rects recorded at draw; empty when hidden).
fn rail_at(mx: f32, my: f32) -> Option<usize> {
    let n = unsafe { addr_of!(RAIL_N).read() };
    let rects = unsafe { addr_of!(RAIL_RECTS).read() };
    rects[..n].iter().position(|r| r.contains(mx, my))
}

pub(crate) fn wheel(dy: c_int) {
    if menu_open() {
        table().move_sel(if dy < 0 { 1 } else { -1 });
        crate::ui::popover::note_own_damage();
        return;
    }
    move_focus(if dy < 0 { SDLK_DOWN } else { SDLK_UP });
}

// ---- draw -----------------------------------------------------------------------------------

const CHIP_PAD: f32 = 24.0;
const CHIP_ISZ: f32 = 22.0; // trailing-icon box
/// The Source chip's owner annotation sits one rung down, on `MICRO` — the scale's kicker rung,
/// which exists for exactly this: a one-line de-emphasised run beside a value, never for content.
const NOTE_SZ: c_int = theme::size::MICRO;
/// …at 62% of the label's OWN ink. A weight, not a second colour role, so the annotation reads the
/// same step down whether the chip is idle (light ink on a dark capsule) or focused (dark ink on
/// the accent), where a fixed grey would go muddy.
const NOTE_INK_A: f32 = 0.62;

/// The toolbar chips' strings + measured widths, cached until anything they draw changes.
fn chip_strs() -> &'static [ChipStrs] {
    // the VIEW labels, not the committed ones: a chip must relabel on the press frame, not 70 ms
    // later. The key is every drawn run, so it invalidates on the QUEUE and then again on the
    // commit only if the resolved label actually differs (by construction it doesn't).
    let row = chips();
    // the QUEUED section's owner, not the committed one — the chip's name already comes from
    // `view_section()`, and resolving the handle from `cur()` would draw a friend's library name
    // with no owner beside it for the 70 ms of the fade and then pop the run in, changing the
    // chip's measured width mid-transition
    let handle = crate::browse::handle_of(view_section());
    let key = row.iter().fold(String::new(), |mut k, &c| {
        let (n, v) = chip_value(c);
        k.push_str(n);
        k.push('\u{1}');
        k.push_str(&v);
        k.push('\u{1}');
        k
    }) + handle;
    let cache = unsafe { &mut *addr_of_mut!(CHIP_CACHE) };
    if cache.as_ref().map(|(k, _)| *k != key).unwrap_or(true) {
        let built = row
            .iter()
            .map(|&c| {
                let (name, value) = chip_value(c);
                let sz = theme::size::LABEL;
                let name_c = CString::new(name).unwrap_or_default();
                let val_c = CString::new(format!(" \u{b7} {value}")).unwrap_or_default();
                // The owner annotation rides ONLY the Source chip, and only for someone else's
                // library. `handle` is "" on your own, so this is `None` there — absent, not empty.
                let note = (c == Chip::Source && !handle.is_empty())
                    .then(|| CString::new(format!("  {handle}")).unwrap_or_default());
                let nw = crate::text::text_width(name_c.as_ptr(), sz, 0);
                let vw = crate::text::text_width(val_c.as_ptr(), sz, 1);
                let hw = note
                    .as_ref()
                    .map(|h| crate::text::text_width(h.as_ptr(), NOTE_SZ, 0))
                    .unwrap_or(0.0);
                let w = CHIP_PAD + nw + vw + hw + 10.0 + CHIP_ISZ + CHIP_PAD;
                ChipStrs {
                    kind: c,
                    name: name_c,
                    val: val_c,
                    note,
                    w,
                }
            })
            .collect();
        *cache = Some((key, built));
    }
    &cache.as_ref().unwrap().1
}

/// One toolbar chip from its cached strings. Hierarchy per the design review: the VALUE
/// ("Title"/"All") is the information-bearing token — bright + bold; the static name prefix is
/// dim + regular. Every chip on this toolbar OPENS A MENU, which is why the trailing glyph is
/// always a chevron: the third chip — a value-less capsule that WAS its own toggle, wearing an
/// amber disc when on — is gone (2026-08-13), because the Filter menu it sat beside already
/// carries "Unwatched only" as a SWITCH row, and one state deserves one control.
/// One chip of the grid's control row, or the library chip at the head of the document. **`y` is a
/// parameter now**: both of those scroll, where the old fixed toolbar sat at a constant `TOOL_Y`.
fn toolbar_chip_at(p: Painter, x: f32, y: f32, cs: &ChipStrs, icon: Icon, focused: bool) -> Rect {
    let sz = theme::size::LABEL;
    let r = Rect::new(x, y, cs.w, TOOL_H);
    let (bg, bright_ink, dim_ink) = if focused {
        (
            crate::ui::ACCENT,
            crate::ui::ACCENT_INK,
            theme::with_a(crate::ui::ACCENT_INK, 0.62),
        )
    } else {
        (
            theme::CONTROL_IDLE_FILL,
            theme::CONTROL_IDLE_INK,
            theme::TEXT_SECONDARY,
        )
    };
    p.rrect(r, TOOL_H * 0.5, TOOL_H * 0.5, bg);
    let ty = crate::text::text_vcenter_y(sz, 1, r.y + r.h * 0.5);
    let mut tx = r.x + CHIP_PAD;
    tx += p.text(cs.name.as_ptr(), tx, ty, sz, dim_ink, 0, 0);
    tx += p.text(cs.val.as_ptr(), tx, ty, sz, bright_ink, 0, 1);
    // the owner annotation, on the VALUE's baseline so the two runs sit on one line despite the
    // rung change (layout ≠ paint). Absent entirely on your own libraries.
    if let Some(h) = &cs.note {
        let hy = crate::text::baseline_y(NOTE_SZ, 0, sz, 1, ty);
        tx += p.text(
            h.as_ptr(),
            tx,
            hy,
            NOTE_SZ,
            theme::with_a(bright_ink, NOTE_INK_A),
            0,
            0,
        );
    }
    // trailing icon — an SVG asset, never a font glyph
    let ir = Rect::new(tx + 10.0, r.y + (r.h - CHIP_ISZ) * 0.5, CHIP_ISZ, CHIP_ISZ);
    let tint = if focused {
        crate::ui::ACCENT_INK
    } else {
        theme::TEXT_SECONDARY
    };
    crate::ui::icons::draw(p, icon, ir, tint);
    r
}

pub(crate) fn draw() {
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    // TWO nested fades, and the nesting is the whole design.
    //
    // `p` is the PAGE — everything this screen owns that Home does not: the grid, the top scrim,
    // the toolbar chips, the count, the rail, the menus. It dips to the app ground and back when
    // the ROUTE changes (`ui::nav`), which is what makes Home↔Library a transition rather than a
    // cut. `pk` is the CONTINUOUS CHROME — the profile chip and the shared tab row, the same
    // control Home draws — which holds still while the pages swap under it.
    //
    // `pg` adds the grid's OWN content cross-fade on top of `p`, multiplicatively and for free.
    // Everything that is a FUNCTION OF THE LISTING draws through it: the tiles, the focused tile's
    // label block, the empty line, the item count and the A–Z rail (which indexes the grid and is
    // meaningless without it). Everything that PERSISTS ACROSS a re-query stays on `p`: the
    // top-chrome scrim, the three toolbar chips, the loading spinner and the menus.
    // `Painter::alpha` is multiplicative and folded into every primitive by `Painter::c` —
    // including `tex_carded`'s tint and its folded drop shadow, `sheen_rim`, `Painter::text` and
    // `icons::draw` — so no per-tile work is needed and nothing can escape either fade.
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    let pk = Painter::root().alpha(crate::ui::nav::chrome_alpha());
    // **Two content painters, one per BLOCK** — see [`Scope`]. `pg` fades the grid block, which
    // every re-query replaces; `ph` fades the document's HEAD (the library chip and the library's
    // own shelves) only when the whole page is being replaced. A sort used to dissolve and rebuild
    // both, for a change that touched one.
    let pg = p.alpha(block_alpha(Scope::Grid));
    let ph = p.alpha(block_alpha(Scope::Page));
    // The GROUND, standing in for the flat clear above — opaque, so it goes down before anything
    // else and the glass tab track samples it like any other page content. On `p` rather than `ph`:
    // the ground is the PAGE's, not the head block's, so a sort that dissolves the grid must not
    // also dissolve the atmosphere the grid is standing on. It skips itself when it has resolved
    // to the clear colour anyway (a library with no artwork colours draws no extra pass at all).
    unsafe { (*addr_of!(GROUND)).draw(p, Rect::FULL) };
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    // ONE layout for the whole frame, shared by the document draw, the grid cull and the hit test
    // recorded inside them — so the picture and the pointer can never be looking at two documents.
    let lay = layout();
    let focused_i = focus_idx();

    // ---- the content region: a grid, or the ONE read-out that stands in for it ----------------
    // The spinner and the failure draw on `p`, not `pg`: a status must stay legible while the
    // content band is dark (`Xfade`'s rule — never fade a read-out with the content it replaces).
    // The empty line rides `pg` because it IS a function of the listing, and dissolves with it.
    let t = total();
    let read = readout();
    // …and whether PASS 2 runs at all — one predicate, read here, by the neighbour loop that skips
    // the cell pass 2 owns, and by the lift over a context menu's scrim ([`focused_card_drawn`]).
    let focused_pass = focused_card_drawn(read, t);
    let mut status_btn = Rect::new(-1.0, -1.0, 0.0, 0.0);
    match read {
        Readout::Loading => {
            Spinner::new(SCR_W * 0.5, SCR_H * 0.52, 26.0)
                .phase(unsafe { addr_of!(PHASE_MS).read() } as u32)
                .draw(&Env::inert(), p);
        }
        Readout::Failed => {
            // Anchored in the CONTENT REGION, not on the panel: the chrome above is still live, and
            // that is the difference between a section failing and the app failing. No spinner
            // (`browse` retries underneath, and a spinner for a source that isn't coming would hold
            // the panel presenting — `ui::idle`), no alert glyph, one action.
            let (_, _, verdict, reason) = dead_strs();
            let mut ov = crate::ui::widgets::StatusOverlay::new(
                content_region(),
                verdict,
                StatusKind::Failed,
            )
            .action(c"Try again")
            .focused(area() == Area::Status && !menu_open());
            if let Some(r) = reason {
                ov = ov.reason(r);
            }
            ov.draw(&Env::inert(), p);
            status_btn = ov.action_frame().unwrap_or(status_btn);
        }
        Readout::Empty => {
            // NOT `Failed`, and so no danger tint and no Retry: the server answered, and its answer
            // was nothing. There is nothing here to try again.
            crate::ui::widgets::StatusOverlay::new(
                content_region(),
                empty_caption(),
                StatusKind::Empty,
            )
            .draw(&Env::inert(), pg);
        }
        Readout::Grid => {}
    }
    unsafe { *addr_of_mut!(STATUS_BTN) = status_btn };
    // visible row RANGE by arithmetic, not an O(totalSize) walk (a 10k-item section must not
    // cost 1.7k iterations per frame); rows fully under the opaque top-chrome scrim are culled
    // too — drawing the full card composite beneath it was pure fill-rate waste.
    // Deliberately NO `if pg.alpha > 0` early-out around these loops: at the floor they already
    // cost nothing (`n_rows()` is 0 while `total < 0`), and skipping them would stop `resolve_tex`
    // being called for the incoming section's visible tiles until the fade-IN starts — delaying
    // every poster fetch by the length of the fade. The 64-slot LRU must see the on-screen
    // requests exactly as early as it does today.
    //
    // The read-out STANDS IN PLACE OF the grid, so a state that draws one draws no tiles. In the
    // product that is already true by arithmetic — every non-`Grid` read-out has `total <= 0` and
    // `n_rows()` is 0 — but it is true by ARITHMETIC, not by construction, which is exactly the
    // kind of agreement that stops holding the moment something else decides the state (the
    // `libfail` boot). It costs no poster fetch: there are no tiles to resolve in those states.
    // **Through `Layout`**, so the rows drawn and the rows `browse::want` asked for are the same
    // rows: both call `visible_rows` on this frame's document. They used to be two expressions over
    // `SCROLL` that agreed only while the grid started at scroll 0.
    let (r_lo, r_hi) = lay.visible_rows(sc);
    for r in r_lo..r_hi {
        if read != Readout::Grid {
            break;
        }
        let y = lay.row_y(r, sc);
        if !block_on_screen(y, CARD_H) {
            continue;
        }
        for c in 0..cols_in_row(r) {
            let idx = r * COLS + c;
            if focused_pass && idx == focused_i {
                continue; // drawn last (z-order over neighbours)
            }
            let m = crate::browse::item(idx);
            let x = MARGIN_X + c as f32 * (CARD_W + LGAP);
            let s = if idx as i64 == unsafe { addr_of!(PREV_IDX).read() } {
                unsafe { addr_of!(PREV_S).read() }.pos
            } else {
                1.0
            };
            let rect = Rect::new(x, y, CARD_W, CARD_H).scaled(s);
            let resume = m.and_then(PmsMovie::resume_frac);
            card_row::draw_tile(pg, Art::Poster(m), rect, s, &RowStyle::HOME, resume);
        }
    }

    // ---- grid: pass 2 — the focused card + under-label, over its NEIGHBOURS but UNDER the top
    // chrome: the toolbar/pills win, so a focused card scrolling through the top band slides
    // beneath the Sort/Filter chips instead of popping over them (Home's convention; this used to
    // draw after the chrome and inverted it).
    if focused_pass {
        draw_focused_card(pg);
    }

    // ---- the document's own chrome: the library chip, the shelves, the grid's heading row -----
    //
    // **All of it INSIDE the scroll**, which is deliverable D: one continuous column, and the tab
    // track the only chrome standing over content. The fixed toolbar at `TOOL_Y` is gone and so is
    // the `widgets::nav_scrim` that backed it — a scrim exists to keep a FIXED bar legible over
    // moving content, and there is no fixed bar under the track any more. `nav_scrim` is DELETED
    // now: Search, its only other caller, became one document too on 2026-09-05.
    draw_document(pg, ph, &lay, sc);

    // ---- top chrome: the shared bar, which is all that is left standing over the page ---------
    // the shared bar, both stops: the centred pills, and then the chip — which unfurls when THIS
    // screen's focus is on it, where it used to be drawn with a hard-coded `expand: 0.0` because it
    // could not be focused here at all.
    //
    // **The chip goes LAST, over the track**, and that order is load-bearing now that it unfurls on
    // this screen: the focused chip grows the profile NAME to its right, far enough with a long
    // name to reach the centred track, and drawn first it went under the track's own scrim. Home
    // has always drawn it in this order for exactly that reason; this screen could get away with
    // the other one only while the chip here was frozen shut.
    crate::ui::widgets::draw_tab_row(pk);
    crate::ui::widgets::profile_chip(pk);

    // ---- letter rail (right edge; only on the unfiltered ascending-title listing) ------------
    // faded WITH the grid it indexes. Its `RAIL_RECTS` stay live at α≈0 for the ~210 ms of a
    // transition, so a pointer click there still jumps focus — judged harmless (the rail is
    // conceptually still present), and the one hit-test/visibility divergence this fade has.
    draw_rail(pg);

    // ---- menus over everything ---------------------------------------------------------------
    // These build their own root painter (`Popover::painter` — its scrim must not compound with
    // its own content fade), which used to mean an open menu sat at full strength over a dipping
    // page. It doesn't any more: `Popover::painter` multiplies BOTH of its roots by
    // `nav::page_alpha`, because a panel is drawn ON a page and a route change replaces the page.
    // The path was already unreachable here — every input that could start a route change while a
    // menu is up dismisses the menu instead, and `enter` clears `MENU` at the floor regardless —
    // but the popovers Home and the detail page carry (the item context menu, the profile menu) are
    // NOT, so the rule belongs in the shared component rather than in a note per screen.
    if menu_open() {
        // Everything from here is LIVE over the frozen grid — and constructing this is what takes
        // the snapshot on the menu's first frame (the grid above is complete, nothing of the menu is
        // on the framebuffer yet). See `popover::host::live`.
        let _live = crate::ui::popover::host::live();
        // Split into TWO phases on purpose (`/tmp/plxnative-profile`): an open menu costs this
        // screen ~2× its draw — measured `draw` 16 → 33-42 ms with the Sort panel up, which is
        // the whole of the frame (`pump=0.0 swap=0.4 up=0`) and lands it either side of the vsync
        // boundary, so frames alternate 16/33 and the rate reads 23-29. The two candidates are a
        // full-screen alpha quad (`Popover::painter`'s scrim: 2.07M fragments over an
        // already-drawn grid) and the panel's own list, and they are separable only by measuring
        // them apart. Zero-overhead with the trigger off.
        use crate::ui::profile::phase;
        // `scrim_lifting` + `content_painter`, which is exactly what `painter` does with the chip
        // LIFTED back out of the dim between the two halves — the chips are drawn above, so
        // without it a chip recedes under its own menu.
        let mp = phase("menu.scrim", || {
            pop().scrim_lifting(0.45, &Opener::drawn(redraw_menu_chip));
            pop().content_painter(20.0)
        });
        phase("menu.panel", || {
            let r = panel_rect();
            pop().panel(mp, r, 24.0);
            table().list_focused = true;
            table().draw(mp, table_rect());
        });
    }
}
/// The zone RIGHT was pressed in to reach the rail, so LEFT can undo it. Both doors into the rail
/// go through [`enter_rail`]; nothing else writes this.
static mut RAIL_FROM: Area = Area::Grid;

/// Enter the letter rail from `from`, seating it on the letter the grid is currently showing.
///
/// One door for both entries — the grid's last column and the control row's last chip — because
/// they must seat the rail identically and must both be undoable by LEFT.
fn enter_rail(from: Area) {
    unsafe {
        RAIL_FROM = from;
        AREA = Area::Rail;
        RAIL_F = letter_of(focus_idx());
    }
}

/// How far the head of the document rises for a magnified tile in the first shelf — that shelf's
/// own [`card_row::CardRow::lift`], and nothing new.
fn head_lift(lay: &Layout) -> f32 {
    if lay.shelves == 0 {
        return 0.0;
    }
    unsafe { (*addr_of!(SHELF_ROWS))[0].lift() }
}

/// The grid heading block's lift — the same [`card_row::heading_clearance`] rule a shelf's heading
/// uses, held on a spring here because the grid has no `CardRow` to hold one for it.
///
/// A spring rather than the raw clearance for `CardRow`'s own reason: the value is a DESTINATION,
/// and reading it straight would drop the whole lift in the frame focus moves and spring it back.
static mut HEAD_S: Spring = Spring::at(0.0);
fn grid_head_lift() -> f32 {
    unsafe { (*addr_of!(HEAD_S)).pos }
}
/// The lift the block is heading for: full clearance while a tile in grid ROW 0 is magnified,
/// tapered by the same proximity ramp a shelf uses (a popped card in the last column is nowhere
/// near a left-aligned heading), and zero otherwise.
fn grid_head_lift_target() -> f32 {
    let focused = (matches!(area(), Area::Grid | Area::Rail) && unsafe { GR } == 0 && !menu_open())
        .then(|| unsafe { GC });
    // scroll 0: the grid does not scroll horizontally, so a column IS its distance from the margin
    card_row::heading_clearance(focused, &RowStyle::HOME, 0.0)
}

/// **The content column's right boundary for TEXT** — the A–Z rail's band, treated as an edge.
///
/// The rail occupies the right of this screen's content region, and it is drawn OVER the document,
/// so a heading laid out to the panel margin runs underneath the letters. That is not something the
/// rail can fix from its own side; the text has to respect the boundary, exactly as it respects the
/// screen edge.
///
/// [`GRID_R`] is that boundary and it already exists: the grid gives the band up unconditionally so
/// its six columns never reflow between two sections of one library. Text takes the same edge for
/// the same reason — a heading that changed width when the rail appeared would be worse than one
/// that is always a little short — and this is deliberately NOT a narrowing of the content column
/// anywhere else: Home passes `INFINITY`, because nothing sits on the right of that screen.
fn heading_max_w() -> f32 {
    GRID_R - MARGIN_X
}

/// **Is a block `h` tall, whose top edge is at screen `y`, worth drawing this frame?** ONE rule,
/// for the shelves and for the grid rows alike.
///
/// They were two, and the grid's carried a second term: `y + CARD_H < TOP_BAR_BOTTOM` also culled a
/// row whose bottom edge had climbed above the tab track. That was correct — free, even — for as
/// long as an OPAQUE scrim covered the band between the track and the content edge, which is what
/// the fixed toolbar sat on. The one-scroll rewrite deleted the toolbar and the scrim with it, and
/// the tab track is a CENTRED PILL rather than a full-width bar: everything to its left and right
/// at that height is open screen. So the term stopped meaning "hidden anyway" and started meaning
/// "vanishes in plain sight" — cards popping out of existence 130px from the top edge while
/// scrolling, in the two columns either side of the track, which is exactly what was reported. The
/// shelves never had the term and never showed the fault, which is what localised it.
///
/// `GLOW_PAD` is the focus glow's spill, so a row is kept a little past its own edges.
fn block_on_screen(y: f32, h: f32) -> bool {
    on_axis(y, h, SCR_H, GLOW_PAD)
}

/// Document y → screen y. The ONE conversion, so nothing derives a screen coordinate from `SCROLL`
/// by hand.
fn doc_y(_lay: &Layout, y: f32, scroll: f32) -> f32 {
    CONTENT_TOP + y - scroll
}

/// **The scrolling column: the library chip, the library's own shelves, and the grid's heading
/// row.** Everything that used to be fixed chrome under the tab track (deliverable D).
///
/// Drawn on `pg` — the page fade times the grid's content cross-fade — because every part of it is
/// a function of the LISTING: change library and the chip's name, the shelves and the count all
/// belong to the new one.
fn draw_document(pg: Painter, ph: Painter, lay: &Layout, sc: f32) {
    // ---- the library chip, at the head ----
    let mut tool_rects = [Rect::new(0.0, 0.0, 0.0, 0.0); 3];
    if lay.chip {
        // **The chip rides the lift of the block BELOW it**, which is the same rule Home's headings
        // have always followed and the reason `card_row` owns it. A magnified tile in the first
        // shelf raises that shelf's heading (`CardRow::lift`); leaving the chip behind opens the
        // gap between them by the whole lift, which is what "the pills stay where they were,
        // leaving an awkward amount of spacing" describes. Moving with it holds the gap constant.
        let y = doc_y(lay, 0.0, sc) - head_lift(lay);
        if on_axis(y, TOOL_H, SCR_H, 0.0) {
            let strs = chip_strs();
            if let Some(cs) = strs.iter().find(|c| c.kind == Chip::Source) {
                toolbar_chip_at(
                    ph,
                    MARGIN_X,
                    y,
                    cs,
                    Icon::ChevronDown,
                    area() == Area::LibChip && !menu_open(),
                );
            }
        }
    }

    // ---- the library's own shelves ----
    let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
    for (i, sh) in shelves.iter().take(MAX_SHELVES).enumerate() {
        let origin = doc_y(lay, lay.shelf_origin(i), sc);
        // one shelf's whole band: heading above the origin, cards below it
        if !block_on_screen(origin - crate::ui::consts::TITLE_DY, lay.shelf_pitch(i)) {
            continue;
        }
        // **A row of EPISODES is drawn landscape** — its own style, its own art variant and its own
        // slot pitch, all three from the one flag the parse set. Everything else about it is the
        // shared shelf: the same `CardRow` springs, the same heading and lift, the same focus ring,
        // the same resume bar, the same watched disc, the same walk.
        let sty = if sh.landscape {
            &RowStyle::EPISODE
        } else {
            &RowStyle::HOME
        };
        let row = unsafe { &(*addr_of!(SHELF_ROWS))[i] };
        let focused = (area() == Area::Shelf && unsafe { SHELF_F } == i && !menu_open())
            .then(|| unsafe { SHELF_C });
        // the heading rides the row's own LIFT, exactly as Home's does — the adjacency rule
        // `card_row::heading_clearance` implements, so a focused tile never crowds its title
        crate::ui::card_row::draw_heading(
            ph,
            &sh.title,
            "",
            MARGIN_X,
            origin - crate::ui::consts::TITLE_DY - row.lift(),
            heading_max_w(),
        );
        let n = sh.items.len().min(card_row::MAX_ROW_ITEMS);
        card_row::strip(
            ph,
            row,
            n,
            focused.map(|c| c as c_int).unwrap_or(-1),
            origin + crate::ui::consts::CARD_DY,
            (sty.w, sty.h),
            sty.w + sty.gap,
            sty,
            SCR_W,
            // `Art::Still` and not `Art::Thumb`: it carries the ROW, which is what `still_key`'s
            // fallback chain and the resume bar are both resolved from. A `Thumb` is a path and a
            // size and can answer neither — the mistake `detail`'s Related shelf made for months.
            //
            // It does NOT carry it for the watched DISC any more, which is what this comment used
            // to say: a landscape tile's watch state is the tick inside its state line, and one
            // mark per tile is the rule (`widgets::still_line`, `Library Screens.dc.html` E).
            |k| {
                if sh.landscape {
                    Art::Still(sh.items.get(k))
                } else {
                    Art::Poster(sh.items.get(k))
                }
            },
            // a LANDSCAPE shelf draws its own bar in `extra`, after the scrim — see
            // `widgets::still_overlay`. A poster shelf keeps `card_row`'s.
            |k| {
                (!sh.landscape)
                    .then(|| sh.items.get(k).and_then(PmsMovie::resume_frac))
                    .flatten()
            },
            |k| shelf_label(sh, k),
            // …and the label INSIDE the artwork, on every tile whether focused or not — the one
            // thing that separates a shelf of stills from a row of anonymous frames. `extra` is
            // handed the tile's unscaled x and whether it is the focused one, so the line is
            // rebuilt on the rect actually drawn and rides the focus pop without resizing.
            |pe, k, x, foc| {
                if !sh.landscape {
                    return;
                }
                let Some(m) = sh.items.get(k) else { return };
                let sc = if foc {
                    row.scale(k) * crate::ui::press::scale()
                } else {
                    row.scale(k)
                };
                let card = Rect::new(x, origin + crate::ui::consts::CARD_DY, sty.w, sty.h)
                    .scaled(sc);
                crate::ui::widgets::still_overlay(
                    pe,
                    m,
                    card,
                    sty.tile_radius(card, sc),
                    sh.is_continue,
                );
            },
        );
    }

    // ---- the grid's heading row: its title, then Sort and Filter, then the count ----
    //
    // Inside the scroll, under the grid's own heading — which is what DELETED the visibility rule
    // the fixed toolbar spent two revisions on. A control that scrolls away with the thing it acts
    // on needs no rule about when it is drawn; it needs only not to be drawn when there is nothing
    // to act on, which `Layout::grid_head` already answers.
    if lay.grid_head {
        // …and the grid's heading BLOCK — its title and its Sort/Filter row, which are one block —
        // rides the lift of grid row 0 the same way. Both parts move together: a heading that rose
        // while the chips under it stood still would open the same gap one rung lower down.
        let top = doc_y(lay, lay.grid_block_top(), sc) - grid_head_lift();
        if on_axis(top, GRID_HEAD, SCR_H, GLOW_PAD) {
            // the heading reads "All films · 185 films · A–Z" — one line, the count folded into it
            // rather than floated at the far right of a bar that no longer exists
            let (head, note) = grid_heading();
            crate::ui::card_row::draw_heading(pg, &head, &note, MARGIN_X, top, heading_max_w());
            // …and the control row a rung below it
            let cy = top + crate::ui::consts::TITLE_DY + crate::ui::consts::CARD_DY;
            let tool_i = tool_f();
            let tool_focus = |i: usize| area() == Area::Toolbar && !menu_open() && tool_i == i;
            let strs = chip_strs();
            let mut x = MARGIN_X;
            for (i, cs) in strs.iter().enumerate() {
                if cs.kind == Chip::Source {
                    continue; // the Source chip heads the document; it is not a grid control
                }
                let r = toolbar_chip_at(pg, x, cy, cs, Icon::ChevronDown, tool_focus(i));
                tool_rects[i] = r;
                x = r.x + r.w + 20.0;
            }
        }
    }
    unsafe { *addr_of_mut!(TOOL_RECTS) = tool_rects };
}

/// The grid block's own heading — `All films · 185 films · A–Z`, the design's line. One string, so
/// the count travels with the words instead of being right-aligned on a bar that is gone.
fn grid_heading() -> (String, String) {
    let noun = crate::browse::section_kind(crate::browse::cur())
        .map(|k| k.noun())
        .unwrap_or("items");
    let tot = crate::browse::total();
    // **The count is part of the HEADING, not a control** (`Library Interactive.dc.html`): it
    // describes the rows under it, it is a fact rather than something you press, and it needs no
    // face of its own. It used to be right-aligned on the toolbar bar that is gone.
    //
    // Two runs, not one string: the shared `draw_heading` puts the title at `size::HEADLINE` in
    // `TEXT_HEADING` and the annotation a rung down in `TEXT_TERTIARY`, which is the canvas's own
    // pairing — and is why this is not simply formatted into the title.
    let title = view_filter_value();
    let sort = view_sort_label();
    let note = if tot >= 0 {
        format!("{tot} {noun} \u{b7} {sort}")
    } else {
        sort.to_string()
    };
    (title, note)
}

/// One shelf tile's label block — what FOCUS reveals under the tile.
///
/// **On a landscape shelf this is the EPISODE's NAME and its one trailing FACT, and it repeats
/// neither printed line** (`Library Screens.dc.html` E, revised 2026-09-05: "focus adds the
/// episode's name and its one trailing fact — time left on Continue Watching, release date on
/// Recently Released. Neither printed line is repeated below: one fact, one place").
///
/// The artwork already carries the show AND the address, both on every tile focused or not
/// ([`crate::ui::widgets::still_overlay`]), so this block owes the two things that are NOT up
/// there. The address was the caption rung until this revision, and moving it onto the artwork is
/// the whole of that revision: four tiles of one show differ only by number, so the number cannot
/// be the thing behind focus — the NAME can, because it distinguishes nothing until you have
/// already picked a tile.
///
/// The trailing fact is chosen by the SHELF and not by the item: a part-watched episode on
/// Continue Watching trails "24 min left" because that is the fact you came for, while a discovery
/// shelf trails the release date. An episode with neither a title nor a fact falls back rather than
/// drawing an empty rung.
/// **The one fact an episode tile trails under its focused artwork**, chosen by the SHELF.
///
/// `Library Screens.dc.html` E: "time left on Continue Watching, release date on Recently
/// Released". Both are facts the printed pair above cannot carry — the artwork prints the show and
/// the address, and the caption rung is the only place left for something that changes with the
/// item's own state.
///
/// **The shelf decides, not the item**, which is what keeps a row consistent: a deck tile with no
/// resume point (a next-up episode) still belongs to the deck, and answering with its air date
/// there would put one dated tile in a row of durations. It falls back to the date rather than to
/// nothing, because a deck tile that has genuinely never been started has no time left to report.
///
/// Empty is an ordinary answer — a server that sent neither a date nor a year — and the caller
/// draws one rung instead of two rather than an empty second line.
fn still_trailing_fact(m: &PmsMovie, is_continue: bool) -> String {
    if is_continue {
        if let Some(_) = m.resume_frac() {
            return crate::ui::fmt::time_left(m.dur_ns / 1_000_000 - m.resume_ms);
        }
    }
    if m.aired.is_empty() && m.year <= 0 {
        return String::new();
    }
    crate::ui::fmt::pretty_date(&m.aired, m.year as i64)
}

fn shelf_label(sh: &crate::browse::section_hubs::Shelf, k: usize) -> card_row::TileLabel {
    let Some(m) = sh.items.get(k) else {
        return card_row::TileLabel::title("");
    };
    if sh.landscape {
        let name = if m.title.is_empty() || m.title == m.show_title {
            crate::ui::fmt::episode_address(m.season_index as i64, m.ep_index as i64)
        } else {
            m.title.clone()
        };
        let fact = still_trailing_fact(m, sh.is_continue);
        return if fact.is_empty() {
            card_row::TileLabel::title(&name)
        } else {
            card_row::TileLabel::titled(&name, &fact)
        };
    }
    // A POSTER shelf: the amber play triangle on the deck's tiles, exactly as Home's — one
    // vocabulary for "this one resumes", whichever screen the shelf is on — and, on every shelf,
    // the SHARED trailing fact under it (`card_row::focused_caption`). This screen passed a bare
    // title until 2026-09-05 and so reserved a two-rung block to draw one rung into it, which is
    // the ragged row the owner reported; Home had carried the caption all along, so the fix was to
    // share its rule rather than to shrink this screen's box.
    let mut l = if sh.is_continue {
        card_row::TileLabel::played(&m.title)
    } else {
        card_row::TileLabel::title(&m.title)
    };
    l.caption = card_row::focused_caption(m, sh.is_continue);
    l
}

/// **Is the grid's PASS 2 — the focused card — drawn at all this frame?**
///
/// ONE predicate, asked by the page's own draw (which also uses it to SKIP that cell in the
/// neighbour loop, so the two halves of the two-pass draw cannot disagree about which cell is
/// pass 2's) and by [`redraw_focused_card`]'s lift over a context menu's scrim. Spelled once
/// because those must agree exactly: a lift is a SECOND draw of a card the page already drew, so a
/// condition that is looser here paints a magnified card the page itself left out — over the dim,
/// where it would be the only thing on screen at full strength.
///
/// `read`/`total` are passed in because the draw already holds both; the lift re-reads them.
fn focused_card_drawn(read: Readout, total: usize) -> bool {
    // the focused card draws LAST (pass 2) while the grid OR the rail owns focus — a rail
    // jump keeps the landed card visibly anchored
    matches!(area(), Area::Grid | Area::Rail) && !menu_open() && total > 0 && read == Readout::Grid
}

/// The focused grid cell's UNMAGNIFIED frame, or None while it is off-screen mid-jump (a page or
/// letter jump leaves the focused row off the panel for a few frames, and nothing there may resolve
/// a texture). The one place this cell's geometry is written — both the drawn card and the rect the
/// context menu anchors beside scale it.
fn focused_card_base() -> Option<Rect> {
    let (r, c) = unsafe { (addr_of!(GR).read(), addr_of!(GC).read()) };
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    // through `Layout`, not the pre-rewrite constant — see `Layout::row_y`
    let y = layout().row_y(r, sc);
    if !on_axis(y, CARD_H, SCR_H, GLOW_PAD) {
        return None;
    }
    Some(Rect::new(
        MARGIN_X + c as f32 * (CARD_W + LGAP),
        y,
        CARD_W,
        CARD_H,
    ))
}

/// The focused grid card's rect at its **focus magnification** — what the item context menu is
/// anchored beside.
///
/// `press::scale()` is deliberately left out, for `home::focused_card_rect`'s reason: the hold that
/// opens the menu cancels the press in the same breath, so anchoring off the transient dip would
/// leave the panel beside a card that springs back out from under it.
pub(crate) fn focused_card_rect() -> Option<Rect> {
    if area() == Area::Shelf {
        return focused_shelf_base();
    }
    Some(focused_card_base()?.scaled(unsafe { addr_of!(FOCUS_S).read() }.pos))
}

/// The focused SHELF tile's frame at its own magnification, or `None` while it is off-panel.
///
/// Built from the same three numbers the shelf's draw uses — the layout's origin, the row's own
/// horizontal scroll and `CardRow`'s per-tile scale — because the context menu is anchored beside
/// the tile that is DRAWN. `shelf_at` is the same geometry read the other way round, for the
/// pointer.
fn focused_shelf_base() -> Option<Rect> {
    let (i, k) = unsafe { (SHELF_F, SHELF_C) };
    let sc = unsafe { addr_of!(SCROLL).read() }.pos;
    let lay = layout();
    if i >= lay.shelves || k >= shelf_len(i) {
        return None;
    }
    let row = unsafe { &(*addr_of!(SHELF_ROWS))[i] };
    let sty = shelf_style(i);
    let y = doc_y(&lay, lay.shelf_origin(i), sc) + crate::ui::consts::CARD_DY;
    if !on_axis(y, sty.h, SCR_H, GLOW_PAD) {
        return None;
    }
    let x = MARGIN_X + k as f32 * (sty.w + sty.gap) - row.scroll_x();
    Some(Rect::new(x, y, sty.w, sty.h).scaled(row.scale(k.min(card_row::MAX_ROW_ITEMS - 1))))
}

/// The grid's PASS 2 — the focused card and its under-label, over its neighbours.
///
/// A function rather than the tail of the grid loop because it is drawn TWICE while the item
/// context menu is up: once in its own z-order, and once more over the modal scrim
/// ([`redraw_focused_card`]). One body, so the lifted copy is the drawn card.
fn draw_focused_card(pg: Painter) {
    let Some(base) = focused_card_base() else {
        return;
    };
    let s = unsafe { addr_of!(FOCUS_S).read() }.pos * crate::ui::press::scale();
    let rect = base.scaled(s);
    let m = crate::browse::item(focus_idx());
    // The grid's own trailing fact, through the shared rule rather than a private year match: the
    // shelves above it now use the same one, and a grid that answered "1994" where a shelf answered
    // "S1 • E8" would be the same object captioned two ways on one screen.
    let label = m
        .map(|mm| {
            let mut l = card_row::TileLabel::title(&mm.title);
            l.caption = card_row::focused_caption(mm, false);
            l
        })
        .unwrap_or_default();
    let resume = m.and_then(PmsMovie::resume_frac);
    card_row::draw_focused(pg, Art::Poster(m), rect, s, &GRID_STYLE, resume, &label);
}

/// Re-draw the focused grid card ON TOP of a modal scrim — this screen's half of
/// [`crate::ui::popover::Opener`], for the item context menu opened over the browse grid.
///
/// The alpha is the page's own CONTENT fade (`page_alpha × Xfade`), the same `pg` the grid draws
/// on: a card lifted on the bare page alpha would stay bright through a re-query that is dissolving
/// the grid it belongs to.
pub(crate) fn redraw_focused_card() {
    crate::ui::guard(|| {
        // A SHELF tile is lifted from the shelf's own draw, not the grid's pass 2 — same card
        // component, different geometry and a different predicate, so it gets its own branch rather
        // than being squeezed through `focused_card_drawn`.
        if area() == Area::Shelf {
            redraw_focused_shelf_tile();
            return;
        }
        // The page's OWN pass-2 predicate, asked again rather than approximated: a lift is only
        // ever the card the page drew, so the two must be one rule.
        if !focused_card_drawn(readout(), total()) {
            return;
        }
        let p = Painter::root().alpha(crate::ui::nav::page_alpha() * xf().alpha());
        // **Cut at the chrome**, which on this screen is not defensive: the grid's pass 2 is drawn
        // BEFORE the toolbar precisely so a focused card scrolling through the top band slides
        // beneath the Sort/Filter chips instead of popping over them, and a lift drawn after
        // everything would invert exactly that. `TOOL_Y + TOOL_H` is the lowest thing the top bar
        // draws — the same line the retired `nav_scrim` used to fade from. Set and cleared in one
        // breath, as
        // `Painter::clip`'s global GL state requires.
        let cut = TOOL_Y + TOOL_H;
        p.clip(Rect::new(0.0, cut, SCR_W, SCR_H - cut));
        draw_focused_card(p);
        p.clip_clear();
    });
}

/// The focused SHELF tile, drawn again over the item menu's scrim — the shelf half of
/// [`redraw_focused_card`]. Same `card_row::draw_focused` the shelf itself ran, on the same
/// geometry, so the lifted copy is the tile the page drew.
fn redraw_focused_shelf_tile() {
    let Some(rect) = focused_shelf_base() else {
        return;
    };
    let Some(m) = focused_item() else {
        return;
    };
    let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
    let Some(sh) = shelves.get(unsafe { SHELF_F }) else {
        return;
    };
    let p = Painter::root().alpha(crate::ui::nav::page_alpha() * block_alpha(Scope::Page));
    // cut at the shared TAB TRACK, which is the only chrome left standing over this page — it was
    // `TOOL_Y + TOOL_H`, the bottom of a fixed toolbar the one-scroll rewrite deleted
    let cut = crate::ui::widgets::TOP_BAR_BOTTOM;
    p.clip(Rect::new(0.0, cut, SCR_W, SCR_H - cut));
    let i = unsafe { SHELF_F };
    card_row::draw_focused(
        p,
        if sh.landscape {
            Art::Still(Some(m))
        } else {
            Art::Poster(Some(m))
        },
        rect,
        unsafe { (*addr_of!(SHELF_ROWS))[i].scale(SHELF_C.min(card_row::MAX_ROW_ITEMS - 1)) },
        shelf_style(i),
        // …and the same split the shelf itself makes: a landscape tile's bar is drawn by
        // `widgets::still_overlay` below, AFTER its scrim, or the gradient darkens it
        (!sh.landscape).then(|| m.resume_frac()).flatten(),
        &shelf_label(sh, unsafe { SHELF_C }),
    );
    // **…and its state line, or the lift is not the tile the page drew.** On a landscape shelf the
    // show's name and its watch glyph live INSIDE the artwork, so a lifted copy without them is an
    // anonymous frame of television — on the one frame where it is the only thing on screen at full
    // strength, which is exactly what `popover::Opener` exists to prevent. `rect` is already the
    // SCALED rect (`focused_shelf_base`), the same one `draw_focused` just used.
    if sh.landscape {
        let s = unsafe { (*addr_of!(SHELF_ROWS))[i].scale(SHELF_C.min(card_row::MAX_ROW_ITEMS - 1)) };
        crate::ui::widgets::still_overlay(
            p,
            m,
            rect,
            shelf_style(i).tile_radius(rect, s),
            sh.is_continue,
        );
    }
    p.clip_clear();
}

/// The toolbar chip an open chip menu hangs off — its index in [`chips`], or None when no menu is
/// open. The [`Opener`](crate::ui::popover::Opener) side of [`redraw_menu_chip`].
///
/// Matched on the [`Chip`] and never on a position, for the reason the enum exists at all: the row
/// is two chips long with one source and three with two, so an index is not an identity.
fn menu_chip() -> Option<usize> {
    menu_chip_of(menu(), chips())
}

/// [`menu_chip`]'s rule, pure — the open menu and the drawn chip row in, an index into that row out.
/// Split from its two live reads so the mapping is host-gradeable: the row's LENGTH moves without
/// input (a second source is granted, or a failed listing collapses `CHIPS_MANY` to
/// `CHIPS_SOURCE_ONLY`), which is exactly the condition an index-based answer gets wrong.
fn menu_chip_of(m: &Menu, chips: &[Chip]) -> Option<usize> {
    let want = match m {
        Menu::None => return None,
        Menu::Source { .. } => Chip::Source,
        Menu::Sort { .. } => Chip::Sort,
        // Genre is drilled into FROM Filter and keeps that chip's panel — one popover, one opener
        Menu::Filter | Menu::Genre { .. } => Chip::Filter,
    };
    chips.iter().position(|c| *c == want)
}

/// Re-draw the chip an open menu belongs to, above that menu's scrim.
///
/// The chips are drawn BEFORE the scrim (they are top chrome and the panel hangs under them), so
/// without this a chip dims under its own menu — the same bug the focused card had, on the one
/// control on screen the panel is about.
///
/// Drawn `focused: false`, which is not a simplification but exactly what the first pass drew:
/// `tool_focus` already answers false while a menu is open, because focus is IN the panel.
fn redraw_menu_chip() {
    crate::ui::guard(|| {
        let Some(i) = menu_chip() else { return };
        let rects = unsafe { addr_of!(TOOL_RECTS).read() };
        let (Some(cs), Some(r)) = (chip_strs().get(i), rects.get(i)) else {
            return;
        };
        if r.w <= 0.5 {
            return; // never drawn this frame (the read-out collapsed the row) — nothing to lift
        }
        let p = Painter::root().alpha(crate::ui::nav::page_alpha());
        // `r` is where the chip was DRAWN this frame, y included — the control row scrolls now, so
        // the lift cannot re-derive a fixed `TOOL_Y` the way it used to
        toolbar_chip_at(p, r.x, r.y, cs, Icon::ChevronDown, false);
    });
}

/// The A–Z rail: only the letters that EXIST (the server's `/firstCharacter` index), vertically
/// centered in the grid band on the right edge. The letter holding rail focus is a snow disc;
/// the letter containing the current grid focus reads primary; the rest tertiary.
fn draw_rail(p: Painter) {
    // the fade is the rail's own; `rail_drawn` is the PRESENCE test the walk and the hit test share
    let a = unsafe { addr_of!(RAIL_A).read() }.pos;
    let p = p.alpha(a);
    if !rail_drawn() && a <= 0.01 {
        unsafe { RAIL_N = 0 };
        return;
    }
    let letters = crate::browse::letters();
    let n = letters.len().min(MAX_LETTERS);
    // TOP-ANCHORED at a fixed pitch (design review): the rail must sit in the same place for
    // every section (a centered block shifted between Movies' 9 letters and Shows' 7), and
    // pulled in from the panel edge. More letters than fit that band SCROLL — the pitch is never
    // divided down to make them fit, see [`RAIL_PITCH`].
    let (y0, cx, win_h, max_scroll) = rail_geom(n);
    let scroll = unsafe { (*addr_of!(RAIL_SCROLL)).pos };
    // a slim capsule track behind the letters — the tab-track family, minimized: makes the
    // rail read as a CONTROL to discover, not stray typography
    let track = Rect::new(
        cx - RAIL_TRACK_W * 0.5,
        y0 - RAIL_CAP_PAD,
        RAIL_TRACK_W,
        win_h + 2.0 * RAIL_CAP_PAD,
    );
    p.rect_sheened(
        track,
        22.0,
        theme::scrim_black(0.30),
        theme::scrim_black(0.40),
    );
    let in_rail = area() == Area::Rail;
    let cur_letter = letter_of(focus_idx());
    unsafe { RAIL_N = n };
    // label CStrings cached per (section table, section) — the letters are immutable once
    // landed, and this loop runs every frame in the default browse state
    let lcache = unsafe { &mut *addr_of_mut!(RAIL_LABELS) };
    let key = (crate::browse::sections_gen(), crate::browse::cur());
    let stale = lcache
        .as_ref()
        .map(|(g, s, v)| *g != key.0 || *s != key.1 || v.len() != n)
        .unwrap_or(true);
    if stale {
        *lcache = Some((
            key.0,
            key.1,
            letters
                .iter()
                .take(n)
                .map(|(l, _)| CString::new(l.as_str()).unwrap_or_default())
                .collect(),
        ));
        // a different section's rail opens at the top rather than sliding back from wherever the
        // last one was left (change-guarded inside `jump`, so this costs no frames)
        unsafe { (*addr_of_mut!(RAIL_SCROLL)).jump(0.0) };
    }
    let labels = &lcache.as_ref().unwrap().2;
    let scrolls = max_scroll > 0.5;
    if scrolls {
        // belt and braces under the edge fade: a glyph reaches α0 before it reaches the window
        // edge, so this only catches the rounding — but without it a letter could paint over the
        // capsule's rounded cap while the spring is mid-flight.
        p.clip(Rect::new(cx - RAIL_TRACK_W * 0.5, y0, RAIL_TRACK_W, win_h));
    }
    for (i, label) in labels.iter().enumerate() {
        let cy = y0 + (i as f32 + 0.5) * RAIL_PITCH - scroll;
        // one letter of fade at each end that has more beyond it: the rail says "there is more
        // alphabet this way" without a scrollbar, and letters leave rather than pop.
        let rel = cy - y0;
        let mut a = 1.0f32;
        if scroll > 0.5 {
            a = a.min(rel / RAIL_PITCH);
        }
        if scroll < max_scroll - 0.5 {
            a = a.min((win_h - rel) / RAIL_PITCH);
        }
        let a = a.clamp(0.0, 1.0);
        // The pointer target is the SLOT, not the disc: a 38 px box at a 34 px pitch overlapped
        // its neighbour, and `rail_at` takes the FIRST match, so the top of every letter clicked
        // the letter above it. Scrolled-away letters keep an empty rect — `Rect::contains` is
        // false on it — so a click can only reach a letter that is actually legible.
        unsafe {
            (*addr_of_mut!(RAIL_RECTS))[i] = if a > 0.5 {
                Rect::new(
                    cx - RAIL_TRACK_W * 0.5,
                    cy - RAIL_PITCH * 0.5,
                    RAIL_TRACK_W,
                    RAIL_PITCH,
                )
            } else {
                Rect::new(0.0, 0.0, 0.0, 0.0)
            }
        };
        if a <= 0.0 {
            continue; // off the window entirely — the cull, since `n` can be twice what fits
        }
        let q = p.alpha(a);
        let d = 38.0f32;
        let r = Rect::new(cx - d * 0.5, cy - d * 0.5, d, d);
        let focused = in_rail && i == unsafe { addr_of!(RAIL_F).read() };
        // idle letters at SECONDARY (the review put TERTIARY below the couch floor here);
        // the current-position letter reads PRIMARY, the focused one is a snow disc
        let ink = if focused {
            q.rect(r, d * 0.5, crate::ui::ACCENT, crate::ui::ACCENT, 0.0);
            crate::ui::ACCENT_INK
        } else if i == cur_letter {
            theme::TEXT_PRIMARY
        } else {
            theme::TEXT_SECONDARY
        };
        let ty = crate::text::text_vcenter_y(theme::size::CAPTION, 1, cy);
        q.text(label.as_ptr(), cx, ty, theme::size::CAPTION, ink, 1, 1);
    }
    if scrolls {
        p.clip_clear();
    }
}

// ---- headless switch-exercise hook (the FPS "all switches" scene) ---------------------------

/// One scripted switch action per call, cycling EVERY switch: tab → tab → sort
/// open/move/close → unwatched on/off → filter open → drill into Genre (kicks the value-list
/// fetch) → back out → letter-rail jump to the last letter and back. Drives the
/// `library_switch` FPS scene.
///
/// The unwatched step drives the same `Pending::Unwatched` REQUEST the Filter menu's row does, so it
/// still exercises the reload path now that the toolbar chip that used to own that switch is gone.
pub(crate) fn switch_step(k: u32) {
    match k % 14 {
        0 => {
            if crate::browse::tab_count() > 1 {
                switch_tab(crate::browse::SecKind::Show);
            }
        }
        1 => switch_tab(crate::browse::SecKind::Movie),
        2 => open_sort_menu(),
        3 => table().move_sel(1),
        4 => {
            let _ = back();
        }
        // the scene fires every 1400 ms — far longer than the ~210 ms transition — so every
        // switch still commits, and the scene now FPS-gates the cross-fade too
        5 | 6 => request_grid(GridAct::Unwatched(!view_unwatched())),
        7 => open_filter_menu(false),
        8 => table().move_sel(1),
        9 => menu_commit(1), // → the Genre value list (fetches it the first time)
        10 | 11 => {
            let _ = back(); // genre → filter → closed
        }
        12 => rail_jump(crate::browse::letters().len().saturating_sub(1)), // A→Z jump
        13 => rail_jump(0),                                                // and back to the top
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! The deferred-reload queue. `PENDING`/`XF` are screen-level `static mut`s, so every test
    //! here holds the module's own [`PEND`] mutex for its whole body — the `home.rs` `FOCUS`
    //! precedent, and the cost of screen-level singleton state; a test that works around it races
    //! its siblings rather than the code.
    //!
    //! What is NOT here, deliberately: `apply_pending`'s effect on the STORE. `set_sort`/`set_genre`
    //! index into `st.sorts`/`st.genres`, which only a live `includeMeta=1` response fills, and
    //! nothing on this host draws a pixel — whether the dissolve READS right is a device capture.
    use super::*;
    use std::sync::Mutex;

    static PEND: Mutex<()> = Mutex::new(());

    /// Put the screen's queue + fader back the way a fresh boot has them, so ordering between
    /// tests cannot matter.
    fn clear() {
        unsafe { PENDING = Pending::NONE };
        *xf() = Xfade::new();
    }

    #[test]
    fn tv_shows_selected_before_discovery_stays_tv_shows_and_loads() {
        // **`serial` BEFORE `PEND`, and the order is the whole point.** Five tests in this module
        // take the crate-wide lock first and this module's own second; this one took them the
        // other way round, which is a lock-order inversion and deadlocks whenever the two
        // schedules interleave — reproduced 2026-09-04 with several checkouts building at once
        // (`every_chip_opens_its_own_menu_by_identity_and_never_by_position` holding `serial` and
        // parked on `PEND` while this test held `PEND` and parked on `serial`; ten tests queued
        // behind them and `make check` never returned). It reads as flakiness under load, which is
        // exactly what `[[test-suite-global-pollution]]` warns it will.
        let _serial = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        crate::browse::reset();
        switch_tab(crate::browse::SecKind::Show);
        assert_eq!(unsafe { WANTED_KIND }, crate::browse::SecKind::Show);
        assert!(crate::browse::tab_of_kind(crate::browse::SecKind::Show).is_none());
        assert_eq!(readout(), Readout::Loading);
    }

    /// Take the open menu OUT, leaving `Menu::None` — the save half of a save/restore around a test
    /// that installs one. [`Menu`] owns a `Vec`, so it cannot be read out by copy the way the
    /// screen's other statics can: a bitwise read would hand back a second owner of the same
    /// allocation and both would free it.
    fn take_menu() -> Menu {
        std::mem::replace(unsafe { &mut *addr_of_mut!(MENU) }, Menu::None)
    }
    /// How many rows the OPEN Sources panel is holding an action for — 0 for every other state,
    /// since the list lives in the variant.
    fn src_acts_len() -> usize {
        match menu() {
            Menu::Source { acts, .. } => acts.len(),
            _ => 0,
        }
    }

    /// A fast double switch must commit ONCE, to the last thing pressed — the queue is one
    /// overwritten value, not a backlog. Deliberately kept off `browse`'s statics: with an empty
    /// section table `apply_section` early-returns, so this needs the module lock only.
    #[test]
    fn the_newest_queued_reload_supersedes_the_one_before_it() {
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        request_grid(GridAct::Sort {
            key: "titleSort".into(),
            desc: false,
        });
        request_section(2);
        assert_eq!(
            pending().section.map(|r| r.sec),
            Some(2),
            "the newer request must replace the older one"
        );
        assert_eq!(
            pending().grid,
            None,
            "…and a grid action aimed at the library being LEFT is discarded, not carried"
        );
        assert!(xf().is_swapping(), "a request arms the fade-out");
        apply_pending();
        assert!(
            pending().is_none(),
            "the commit drains the queue — one press, one swap"
        );
        clear();
    }

    /// The whole point of the typed queue: the store is 70 ms behind, but a CONTROL must
    /// acknowledge its press on the frame it happens, or the fade reads as dropped input. Reads
    /// `browse::cur()`/`unwatched()`, so it takes `testlock::serial()` too — `browse`'s own tests
    /// call `reset()`, which nulls `SECTIONS`/`STATES` underneath these.
    #[test]
    fn the_chrome_reads_the_queued_reload_not_the_committed_store() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let committed = crate::browse::unwatched();
        request_grid(GridAct::Unwatched(!committed));
        assert_eq!(
            view_unwatched(),
            !committed,
            "the chrome flips on the press, not on the commit"
        );
        // a second press inside the same fade cancels the first: the chip must be able to say so
        request_grid(GridAct::Unwatched(committed));
        assert_eq!(view_unwatched(), committed);

        request_section(2);
        assert_eq!(
            view_section(),
            2,
            "the pill travels to the pressed tab immediately"
        );
        assert_eq!(
            view_unwatched(),
            crate::browse::unwatched(),
            "an unrelated request falls back to the store"
        );
        clear();
        assert_eq!(
            view_section(),
            crate::browse::cur(),
            "with nothing queued the chrome IS the store"
        );
    }

    /// [`focused_pill`] feeds a focus assignment ACROSS a route boundary (`app.rs` hands it to
    /// `home::set_hero_focus` at the page fade's floor), so a wrong answer parks Home's focus
    /// somewhere the user never was. It must report the pill only while the tab row is genuinely
    /// what holds focus — not the grid, not the toolbar, not the rail, and not under a menu, where
    /// `TAB_P` is merely the identity the row was last left on.
    #[test]
    fn the_pill_the_user_is_holding_is_what_leaves_the_screen() {
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let (area0, tab0) = unsafe { (addr_of!(AREA).read(), addr_of!(TAB_P).read()) };
        let menu0 = take_menu();

        // Search, whatever position it occupies: the cursor is an IDENTITY now, so this test says
        // WHICH pill rather than which slot — which is the property that broke when the strip
        // stopped being a permanent four.
        let want = crate::ui::widgets::Pill::Search;
        unsafe {
            AREA = Area::Tabs;
            TAB_P = want
        };
        set_menu(Menu::None);
        assert_eq!(
            focused_pill(),
            Some(want),
            "the row holds focus — that pill crosses to Home"
        );

        set_menu(Menu::Sort { built: 0 });
        assert_eq!(
            focused_pill(),
            None,
            "a menu owns the frame; the row underneath is not focused"
        );
        set_menu(Menu::None);

        // `Area::Chip` is in this list deliberately: the chip is a stop on the SAME shared bar, so
        // it is the one band where "the top bar has focus" and "a pill has focus" differ — a chip
        // that reported a pill would carry a highlight across to Home that nobody was standing on.
        for a in [
            Area::Chip,
            Area::Grid,
            Area::Toolbar,
            Area::Rail,
            Area::Status,
        ] {
            unsafe { AREA = a };
            assert_eq!(focused_pill(), None, "focus is not on the tab row");
        }

        unsafe {
            AREA = area0;
            TAB_P = tab0
        };
        set_menu(menu0);
        clear();
    }

    /// **The profile chip is a real focus stop here too, and it is reached the way it is on every
    /// screen that wears the shared bar.**
    ///
    /// The chip is drawn on Home, this screen and Search, and used to be activatable on Home alone
    /// — the rect and the hit test are `widgets`' now, and what each screen still owns is where its
    /// own focus is. The walk is the geometry's: the chip sits at the margin LEFT of the centred
    /// pills, so ◀ off the first pill is the only approach and ▶ is the way back. DOWN leaves the
    /// band by [`below_top_bar`], the same expression the pills use.
    ///
    /// Graded through [`top_focus`] — the value `widgets::tab_row_update` animates the bar from and
    /// `app.rs` routes OK on — so "the chip is focused" cannot mean one thing to the unfurl and
    /// another to the press. Takes `PEND` like its neighbours: it moves `AREA`/`TAB_P`.
    #[test]
    fn the_profile_chip_is_the_bars_leftmost_stop() {
        use crate::ui::widgets::TopFocus;
        // It seeds `browse`'s section table now (the vertical walk is derived from what is DRAWN,
        // so an empty page has nothing below the bar) — and anything touching a crate global owes
        // `testlock::serial()`, not just this module's own `PEND`.
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let (area0, tab0) = unsafe { (addr_of!(AREA).read(), addr_of!(TAB_P).read()) };
        let menu0 = take_menu();
        set_menu(Menu::None);

        unsafe {
            AREA = Area::Tabs;
            TAB_P = Pill::Home
        };
        assert_eq!(
            top_focus(),
            TopFocus::Pill(0),
            "the row holds the FIRST pill"
        );
        move_focus(SDLK_LEFT);
        assert_eq!(
            top_focus(),
            TopFocus::Chip,
            "◀ off the first pill reaches the chip"
        );
        for _ in 0..4 {
            move_focus(SDLK_LEFT); // nothing further left: it must not wrap or underflow
        }
        assert_eq!(
            top_focus(),
            TopFocus::Chip,
            "the chip is the end of the row, not a step before one"
        );

        move_focus(SDLK_RIGHT);
        assert_eq!(
            top_focus(),
            TopFocus::Pill(0),
            "▶ walks back onto the pill it left"
        );

        // DOWN out of the band, from BOTH stops, lands in the same place — the one thing that
        // would read as the chip being a different control from the pills beside it.
        //
        // **The page needs a GRID under it for that to be a question at all.** The vertical walk is
        // derived from what is DRAWN now, so on an empty library every DOWN is correctly a no-op —
        // which is the fix for parking focus on an undrawn control, and which would quietly turn
        // this into an assertion about nothing.
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(12);
        unsafe { AREA = Area::Tabs };
        move_focus(SDLK_DOWN);
        let from_pills = area();
        unsafe { AREA = Area::Chip };
        move_focus(SDLK_DOWN);
        assert!(
            area() == from_pills,
            "DOWN off the chip must land where DOWN off the pills lands"
        );
        assert_eq!(
            top_focus(),
            TopFocus::Away,
            "…and off the bar entirely — neither chip nor pill"
        );
        crate::browse::reset();

        // A menu owns the frame, so the bar underneath holds nothing — the chip's half of the rule
        // `focused_pill` already keeps for the pills.
        unsafe { AREA = Area::Chip };
        set_menu(Menu::Sort { built: 0 });
        assert_eq!(
            top_focus(),
            TopFocus::Away,
            "a panel is up; the chip under it is not focused"
        );

        set_menu(menu0);
        unsafe {
            AREA = area0;
            TAB_P = tab0
        };
        clear();
    }

    // ---- the section read-out ------------------------------------------------------------------
    //
    // [`readout_of`] is PURE — no statics, so no lock and no ordering — which is the whole reason
    // the decision was pulled out of the middle of `draw` in the first place: it is the only part
    // of a failure state a host can grade at all, and every one of its four answers used to be an
    // `else if` nothing could reach without a television.

    use crate::browse::SecFetch;

    /// The failure the screen exists for: this source did not answer, and there is nothing of it to
    /// show. Anything less than the full read-out here is the eternal spinner coming back.
    #[test]
    fn a_failed_fetch_with_nothing_to_show_is_the_failure_read_out() {
        assert_eq!(
            readout_of(SecFetch::Ready, 2, SecFetch::Failed, -1),
            Readout::Failed
        );
    }

    /// …and the mirror of it. A page failing mid-scroll on a section that already HAS items must
    /// not blank a working grid: the items are on screen, the back-off is counting, and a read-out
    /// over them would throw away a library the user can still browse to report a missing page.
    #[test]
    fn a_failed_page_over_a_populated_grid_leaves_the_grid_alone() {
        assert_eq!(
            readout_of(SecFetch::Ready, 2, SecFetch::Failed, 185),
            Readout::Grid
        );
    }

    /// A server that answered with nothing is `Empty` — never `Failed`, so no danger tint and no
    /// Retry. An empty library is an answer, and the read-out states it rather than blaming it.
    #[test]
    fn a_reachable_but_empty_library_is_an_answer_not_a_fault() {
        assert_eq!(
            readout_of(SecFetch::Ready, 2, SecFetch::Ready, 0),
            Readout::Empty
        );
        assert_ne!(
            readout_of(SecFetch::Ready, 2, SecFetch::Ready, 0),
            Readout::Failed
        );
        assert_eq!(
            readout_of(SecFetch::Ready, 2, SecFetch::Ready, 1),
            Readout::Grid,
            "a listing with items is just the grid"
        );
    }

    /// Only a first fetch that has not answered yet is the spinner — the state that must be
    /// reachable exactly once per query, or it is the bug all over again.
    #[test]
    fn only_an_unanswered_first_fetch_spins() {
        assert_eq!(
            readout_of(SecFetch::Ready, 2, SecFetch::Loading, -1),
            Readout::Loading
        );
    }

    /// The eternal spinner one case OVER from the failed table: an account with nothing we browse
    /// (music and photos only) ANSWERED, so there is no section, no state, and both of the section
    /// accessors fall through to their defaults — `Loading` and `-1`, i.e. the spinner again. It is
    /// `Empty`, because the server told us; catching only the FAILED table would have left this
    /// **The LAST row's caption reaches the overscan frame and no further** — the reveal's two
    /// halves (`lo` and the scroll ceiling `max_y`) graded as the pair they are.
    ///
    /// `card_row::reveal` clamps its target to `max_y`, so the bound this screen asks for is only
    /// what it gets while the ceiling is at least as generous. They disagreed by 34px when only `lo`
    /// was moved onto `MARGIN_Y`: every row but the last obeyed the frame and the last one settled
    /// 20px off the panel, which is the state a grid is actually left in more often than any other.
    /// Pure arithmetic, so it needs no store and no mutex.
    #[test]
    fn the_last_grid_rows_caption_settles_on_the_overscan_frame() {
        for n_rows in [1usize, 2, 3, 7, 40] {
            let row_top = (n_rows - 1) as f32 * PITCH;
            let max_y = (n_rows as f32 * PITCH - (SCR_H - GRID_TOP) + MARGIN_Y).max(0.0);
            let lo = row_top + PITCH - (SCR_H - GRID_TOP - MARGIN_Y);
            // scrolled to the very bottom already, then asked to reveal the last row
            let scroll = card_row::reveal(max_y, lo, row_top, max_y);
            let caption_bottom = GRID_TOP + row_top + PITCH - scroll;
            assert!(
                caption_bottom <= SCR_H - MARGIN_Y + 0.01,
                "{n_rows} rows: the last caption settles at {caption_bottom}, past the frame at {}",
                SCR_H - MARGIN_Y,
            );
        }
    }

    /// Item 11: Home's shelf pitch and Library's grid pitch must leave the SAME air under a
    /// focused label block before the next row's top-most ink, which is the whole point of
    /// deriving both from `card_row::UNDER_LABEL_H` + `consts::UNDER_LABEL_AIR` instead of each
    /// screen hand-authoring its own trailing constant (Library's old `96.0` gave a two-line label
    /// only 4px of air; Home's old `144.0` gave it 22px, unnamed and undiscoverable as the same
    /// quantity). Home's pitch spends `TITLE_DY + CARD_DY` on the NEXT shelf's own heading band
    /// before the air is even reached, so that band must be subtracted back out before comparing.
    #[test]
    fn home_and_library_leave_the_same_air_under_a_focused_label() {
        use crate::ui::consts::{CARD_DY, ROW_PITCH, TITLE_DY, UNDER_LABEL_AIR};
        let home_air = ROW_PITCH - TITLE_DY - CARD_DY - CARD_H - card_row::UNDER_LABEL_H;
        let library_air = PITCH - CARD_H - card_row::UNDER_LABEL_H;
        assert_eq!(home_air, UNDER_LABEL_AIR);
        assert_eq!(library_air, UNDER_LABEL_AIR);
        assert_eq!(
            home_air, library_air,
            "Home and Library must agree on the air under a focused label"
        );
    }

    /// spinning exactly as before.
    #[test]
    fn a_table_that_answered_with_no_browsable_library_is_empty_not_a_spinner() {
        assert_eq!(
            readout_of(SecFetch::Ready, 0, SecFetch::Loading, -1),
            Readout::Empty
        );
        assert_ne!(
            readout_of(SecFetch::Ready, 0, SecFetch::Loading, -1),
            Readout::Loading
        );
    }

    /// …and a table nobody has asked for yet is still the spinner, which is the one case where a
    /// spinner is the truth: the boot fetch is out.
    #[test]
    fn a_table_still_being_discovered_spins() {
        assert_eq!(
            readout_of(SecFetch::Loading, 0, SecFetch::Loading, -1),
            Readout::Loading
        );
    }

    /// The layer above: a failed section TABLE. There is no section, so `fetch_state()` answers
    /// `Loading` from its `unwrap_or` and the screen used to spin forever on it — one symptom, one
    /// screen, and now one read-out. The table's verdict OUTRANKS the section state precisely
    /// because that state is a default rather than an observation.
    #[test]
    fn a_failed_sections_list_is_the_same_read_out_and_not_a_spinner() {
        assert_eq!(
            readout_of(SecFetch::Failed, 0, SecFetch::Loading, -1),
            Readout::Failed
        );
        assert_ne!(
            readout_of(SecFetch::Failed, 0, SecFetch::Loading, -1),
            Readout::Loading
        );
        // and it does not depend on which `unwrap_or` default the absent section hands back
        for f in [SecFetch::Loading, SecFetch::Ready, SecFetch::Failed] {
            assert_eq!(readout_of(SecFetch::Failed, 0, f, -1), Readout::Failed);
        }
    }

    /// The decision the multi-source table forced, stated as a test because it is a trade rather
    /// than a mechanism. A source that stops answering KEEPS the sections it had learned and can
    /// still have a page store full of items — so "this source is gone" stopped implying "there is
    /// nothing of it on screen". The read-out takes the region only when BOTH are true; with items
    /// resident the grid stands, exactly as it does for a failed page one layer down.
    #[test]
    fn an_unreachable_source_with_items_still_resident_keeps_its_grid() {
        assert_eq!(
            readout_of(SecFetch::Failed, 3, SecFetch::Failed, 185),
            Readout::Grid
        );
        assert_eq!(
            readout_of(SecFetch::Failed, 3, SecFetch::Failed, -1),
            Readout::Failed,
            "…and takes it when there is nothing"
        );
    }

    /// The read-out sits in the CONTENT REGION, under the live chrome — not centred on the panel,
    /// which is what the app-wide read-out means. `GRID_TOP` is the grid's own top edge, so the
    /// block centres on the band whose content is missing and the toolbar line stays clear.
    #[test]
    fn the_failure_is_anchored_in_the_content_region_not_on_the_panel() {
        let r = content_region();
        assert!(
            r.y >= GRID_TOP,
            "it starts at the grid, below the live toolbar line"
        );
        assert!(
            r.cy() > Rect::FULL.cy(),
            "so its centre sits BELOW the panel centre"
        );
        assert!(
            r.x >= MARGIN_X && r.x + r.w <= SCR_W - MARGIN_X,
            "and inside the screen margins"
        );
    }

    /// The read-out is drawn beside NO GRID CONTROL — Sort, Filter, the count and the A–Z rail all
    /// act on a grid that has no items. The Source chip is the exception, and it is the design's
    /// own: it is NAVIGATION, it is how you leave a dead source, and it is why the read-out spends
    /// no line telling you the way out. `chips()` is the ONE predicate behind the draw, the pointer
    /// scan and the vertical walk, so this pins all three at once.
    #[test]
    fn the_failure_read_out_keeps_navigation_and_drops_every_grid_control() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        // ONE source: an ordinary listing carries Sort + Filter and no Source chip at all
        crate::browse::seed_sources_for_test(1, true);
        assert_eq!(chips(), &CHIPS_ONE[..], "one source draws no Source chip");
        crate::browse::seed_sources_for_test(1, false);
        assert_eq!(readout(), Readout::Failed);
        assert!(
            chips().is_empty(),
            "there is nothing to sort, to filter or to count"
        );

        // TWO sources: the Source chip leads the row, and it OUTLIVES the failure
        crate::browse::seed_sources_for_test(2, true);
        assert_eq!(
            chips(),
            &CHIPS_MANY[..],
            "two sources put Source at the head of the row"
        );
        crate::browse::seed_sources_for_test(2, false);
        assert_eq!(readout(), Readout::Failed);
        assert_eq!(
            chips(),
            &CHIPS_SOURCE_ONLY[..],
            "the way OUT of a dead source stays drawn"
        );
        crate::browse::reset();
    }

    // ---- the Sources chip + its two-level panel -------------------------------------------------

    fn group(name: &str, handle: &str, reachable: bool) -> crate::browse::SrcGroup {
        crate::browse::SrcGroup {
            name: name.into(),
            handle: handle.into(),
            state: if reachable {
                crate::browse::SourceState::Reachable
            } else {
                crate::browse::SourceState::Unreachable
            },
            tier: None,
        }
    }
    fn lib(
        src: usize,
        section: usize,
        title: &str,
        pinned: bool,
        last: bool,
        current: bool,
    ) -> crate::browse::SrcRow {
        crate::browse::SrcRow {
            src,
            section,
            title: title.into(),
            count_line: "26 films".into(),
            pinned,
            last_pinned: last,
            current,
        }
    }
    /// Our own server with two libraries and a friend's with one — the canvas's own shape.
    fn two_sources() -> (Vec<crate::browse::SrcGroup>, Vec<crate::browse::SrcRow>) {
        (
            vec![
                group("mac-mini", "", true),
                group("nas-home", "friend", true),
            ],
            vec![
                lib(0, 0, "Movies", true, false, true),
                lib(0, 1, "TV Shows", true, false, false),
                lib(1, 2, "Film Club", false, false, false),
            ],
        )
    }
    /// The LIBRARY rows, in order, as (has a tick, its trailing word) — `n` of them, which drops
    /// the trailing "Check for new shares" action from the marks under test.
    fn marks(secs: &[Section], n: usize) -> Vec<(bool, Option<String>)> {
        secs.iter()
            .flat_map(|s| s.rows.iter())
            .filter(|r| !r.sep)
            .take(n)
            .map(|r| {
                (
                    r.checked,
                    r.value
                        .clone()
                        .or_else(|| r.toggle.map(|o| if o { "On" } else { "Off" }.to_string())),
                )
            })
            .collect()
    }

    /// THE shared row model, graded at both `Level`s — which since 2026-09-05 belong to two
    /// DIFFERENT screens rather than to two halves of this panel (this one is `Browse` only; the
    /// Favorite libraries editor is `OnHome`). The rule is what keeps them readable as one list
    /// across those two screens: **a mark says
    /// where you are, a word says what is set, and no row says both.** Browse draws one tick and no
    /// words; On Home draws every row's word and no ticks. Neither level mirrors the other's marks.
    #[test]
    fn the_two_levels_never_mirror_each_others_marks() {
        let (g, r) = two_sources();

        let (browse, _) = source_list::sections(Level::Browse, &g, &r, Tail::Recheck);
        let m = marks(&browse, r.len());
        assert_eq!(
            m.iter().filter(|(t, _)| *t).count(),
            1,
            "exactly one tick — the library you are on"
        );
        assert!(m[0].0, "…and it is on the current library");
        assert!(
            m.iter().all(|(_, w)| w.is_none()),
            "Browse states no values: no pins are drawn here"
        );

        let (home, _) = source_list::sections(Level::OnHome, &g, &r, Tail::Recheck);
        let m = marks(&home, r.len());
        assert!(
            m.iter().all(|(t, _)| !t),
            "On Home draws no ticks — it is not a picker"
        );
        assert_eq!(
            m.iter()
                .map(|(_, w)| w.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["On", "On", "Off"],
            "every row reads its value as a word at the trailing edge"
        );
    }

    /// A server is a GROUP — the machine name as its header, the person as the header's accessory,
    /// which is the one place in the app a machine is named. An unreachable one dims WHOLE (header
    /// included) and its rows still read `On`, because nothing was unpinned; hiding it would read
    /// as a revoked share.
    #[test]
    fn a_server_is_a_group_and_an_unreachable_one_dims_whole_while_still_reading_on() {
        let (mut g, r) = two_sources();
        let (secs, _) = source_list::sections(Level::OnHome, &g, &r, Tail::Recheck);
        assert_eq!(secs.len(), 2);
        assert_eq!(
            (secs[0].header.as_str(), secs[0].accessory.as_str()),
            ("mac-mini", "")
        );
        assert_eq!(
            (secs[1].header.as_str(), secs[1].accessory.as_str()),
            ("nas-home", "friend")
        );
        assert!(!secs[0].dim && !secs[1].dim);

        g[1].state = crate::browse::SourceState::Unreachable;
        let mut r = r;
        r[2].pinned = true; // pinned AND unreachable: the state the design calls out by name
        let (secs, _) = source_list::sections(Level::OnHome, &g, &r, Tail::Recheck);
        assert!(secs[1].dim, "the whole group dims, header included");
        assert!(!secs[0].dim, "…and only that group");
        assert_eq!(
            secs[1].rows[0].toggle,
            Some(true),
            "it still reads On — nothing was turned off"
        );
        // the state leads the accessory, so the run that gives way under elision is the handle
        assert_eq!(secs[1].accessory, "Not reachable \u{b7} friend");

        // a source whose libraries we never learned contributes no group at all: a header over
        // nothing reads as broken, and absence is the answer for a share that has never answered
        let (secs, _) = source_list::sections(Level::Browse, &g, &r[..2], Tail::Recheck);
        assert_eq!(secs.len(), 1);
    }

    /// The last pinned library: the VALUE dims and the sub-line states the rule, while the label
    /// keeps live ink and the row keeps its focus. Not the whole row — dim means unavailable, and
    /// this is the library that works.
    #[test]
    fn the_last_pinned_library_dims_its_value_and_states_the_rule() {
        let g = vec![group("mac-mini", "", true)];
        let r = vec![
            lib(0, 0, "Movies", true, true, true),
            lib(0, 1, "TV Shows", false, false, false),
        ];
        let (secs, _) = source_list::sections(Level::OnHome, &g, &r, Tail::Recheck);
        let last = &secs[0].rows[0];
        assert!(last.value_dim, "the value dims");
        assert!(
            !last.dim,
            "the label keeps live ink — the row is not unavailable"
        );
        assert_eq!(last.toggle, Some(true));
        assert_eq!(
            last.detail, "The app needs one library",
            "the sub-line says why"
        );
        assert_eq!(
            secs[0].rows[1].detail, "26 films",
            "every other row keeps its size"
        );
    }

    /// `Check for new shares` is the only row that is not a library: last, under a separator, and
    /// on BOTH levels — which is also what keeps their row counts, and so the panel's height,
    /// identical across the swap. Its action must line up with the row index the table reports,
    /// separator included.
    #[test]
    fn the_recheck_row_sits_last_under_a_separator_on_both_levels() {
        let (g, r) = two_sources();
        for level in [Level::Browse, Level::OnHome] {
            let (secs, acts) = source_list::sections(level, &g, &r, Tail::Recheck);
            let rows: Vec<&Row> = secs.iter().flat_map(|s| s.rows.iter()).collect();
            assert_eq!(
                rows.len(),
                acts.len(),
                "one action per row index, separator included"
            );
            assert_eq!(
                rows.len(),
                r.len() + 2,
                "three libraries, a separator and the recheck row"
            );
            assert!(
                rows[3].sep,
                "the separator divides the libraries from the action"
            );
            assert_eq!(acts[3], SrcAction::None, "…and it does nothing");
            assert_eq!(rows[4].label, "Check for new shares");
            assert_eq!(acts[4], SrcAction::Recheck);
            for (i, row) in r.iter().enumerate() {
                assert_eq!(acts[i], SrcAction::Library(row.section));
            }
        }
    }

    /// **The single most load-bearing test in this unit.** The toolbar used to match on a chip's
    /// INDEX with a `_` catch-all, so the day a chip was inserted at position 0 the whole row
    /// shifted its meaning by one — Source opened Sort, Sort and Filter both opened Filter — with
    /// nothing failing, because every index still resolved to *something*.
    ///
    /// So the mapping is asserted by IDENTITY, chip → menu, and separately that the ROW is the
    /// right chips in the right order. Insert a fourth chip at index 0 and the row assertion fails;
    /// mis-wire a chip to another's menu and the identity assertion fails; add a chip to the enum
    /// without wiring it and `open_chip_menu`'s exhaustive match stops compiling. Position is
    /// asserted nowhere, which is the property under test.
    ///
    /// Takes `testlock::serial` because the one-source half reads `source_chip_on()`, which asks
    /// `browse`'s ROSTER — a crate global another module's test seeds. Without the lock this passed
    /// only while no sibling happened to be holding a two-source table at that instant, which is a
    /// race that reports as "the Source chip is drawn on a single-server install".
    #[test]
    fn every_chip_opens_its_own_menu_by_identity_and_never_by_position() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        // Observe the single-source shape DELIBERATELY. The lock above serialises this test against
        // its siblings but does not undo them, and a sibling that seeds a two-source table leaves
        // the roster behind — so `source_chip_on()` below was reading whatever ran last. That
        // reports as "the Source chip is drawn on a single-server install", which is a real bug's
        // symptom and was not one.
        crate::browse::reset();
        let tool0 = unsafe { addr_of!(TOOL_F).read() };

        // chip → menu, one row per chip. A shifted row cannot satisfy this: it is keyed on the
        // value, not on where the value sits. Matched on the VARIANT — a menu's payload is its
        // build-time key and says nothing about which chip opens it.
        assert!(matches!(chip_menu(Chip::Source), Menu::Source { .. }));
        assert!(matches!(chip_menu(Chip::Sort), Menu::Sort { .. }));
        assert!(matches!(chip_menu(Chip::Filter), Menu::Filter));

        // the row, in drawn order, in both shapes. **Source leads when it exists** — this is the
        // assertion that fails if anyone inserts another chip at index 0.
        assert_eq!(CHIPS_MANY, [Chip::Source, Chip::Sort, Chip::Filter]);
        assert_eq!(CHIPS_ONE, [Chip::Sort, Chip::Filter]);
        // …and the same INDEX means different chips in the two shapes, which is precisely why
        // nothing is allowed to route on one
        assert_ne!(CHIPS_MANY[1], CHIPS_ONE[1]);

        // one source (the host's own state: `browse` has no roster) — Source is ABSENT, not an
        // empty slot and not a disabled chip
        assert!(!source_chip_on());
        assert_eq!(chips(), &CHIPS_ONE);
        for (i, want) in [(0, Chip::Sort), (1, Chip::Filter)] {
            unsafe { TOOL_F = i };
            assert_eq!(focused_chip(), Some(want));
        }
        // a `TOOL_F` left over from the longer row resolves to a chip that EXISTS rather than
        // panicking or naming one that does not
        unsafe { TOOL_F = 2 };
        assert_eq!(focused_chip(), Some(Chip::Filter));

        // ---- and the row SHRINKING under the focus index ------------------------------------
        //
        // The chip the toolbar DRAWS the ring on and the chip OK activates must be one chip. They
        // were not: `focused_chip` clamped and the draw's `tool_focus` compared the raw `TOOL_F`,
        // so a row that lost its grid chips drew no focused chip at all while OK still fired the
        // clamped last one — and RIGHT was dead, because it too measured a stale index against the
        // shorter row. Both now read [`tool_f`], which is why this asserts through it.
        crate::browse::seed_sources_for_test(2, true);
        assert_eq!(chips(), &CHIPS_MANY[..]);
        unsafe { TOOL_F = 2 };
        assert_eq!(tool_f(), 2, "the user walked to the end of the long row");
        assert_eq!(focused_chip(), Some(Chip::Filter));

        // the listing fails and `chips()` collapses under the cursor, with NO input in between
        crate::browse::seed_sources_for_test(2, false);
        assert_eq!(chips(), &CHIPS_SOURCE_ONLY[..]);
        let i = tool_f();
        assert!(
            i < chips().len(),
            "the focused index is inside the row that is drawn"
        );
        assert_eq!(
            chips().get(i).copied(),
            focused_chip(),
            "the ring and the press name one chip"
        );
        assert_eq!(focused_chip(), Some(Chip::Source));
        // …and the walk measures the same row: there is nothing to the RIGHT of the only chip,
        // which is a statement about the row on screen rather than about one the ring left behind
        assert_eq!(
            i + 1,
            chips().len(),
            "RIGHT is at the end because the ring is on the last chip"
        );

        // the source answers again: the row grows back and focus returns to the chip the user
        // actually walked to, rather than having been dumped at 0 while it was away
        crate::browse::seed_sources_for_test(2, true);
        assert_eq!((tool_f(), focused_chip()), (2, Some(Chip::Filter)));

        crate::browse::reset();
        unsafe { TOOL_F = tool0 };
    }

    /// **The rebuild-on-landing guard has to CONVERGE**, and with two kinds of library it did not.
    ///
    /// The Sources panel records a key at build time and `update` rebuilds it in place when the
    /// live one differs. Its key was `source_rows().len()` — KIND-FILTERED, only the libraries of
    /// the tab's own type — compared against `browse::section_count()`, which counts every kind.
    /// On any account with both Movies and TV Shows those two are never equal, so the panel rebuilt
    /// itself (`source_groups()` + `source_rows()` String clones, then a whole `set_sections()`) on
    /// every frame it was open — in the one state this screen's instrumentation measured as
    /// fill-rate-bound. Keyed on the section-table GENERATION it converges, because a generation is
    /// not a count of anything and cannot be compared against the wrong one.
    ///
    /// It must still catch the landing it exists for, so the second half appends a library the way
    /// a discovery worker does and asserts both the rebuild and the row it brought.
    #[test]
    fn the_sources_panel_stops_rebuilding_once_the_table_stops_moving() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        let menu0 = take_menu();
        // The guard comes BEFORE the seed: `append_sections` resolves each row's
        // default the moment it lands, against whatever session file is live at that
        // instant — so a redirect installed afterwards grades pins already decided
        // against the developer's own `auth.json`.
        let _t = crate::plex::session::TempSession::new("panel-rebuild");
        _t.watching("u-panel-rebuild");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        // The panel is FAVOURITE-scoped now, and under `pins::default_on` a shared library of a
        // type you already own starts OFF — so the friend's two libraries would not be offered at
        // all and this fixture would collapse to one row. Favourite them explicitly: what is under
        // test here is the panel's LEVEL and its row identities, not the default.
        //
        // Through the REAL `apply_pins`, not `set_pinned_for_test`, and the difference is the whole
        // reason this note exists: the direct setter writes the live flag and records nothing, so
        // the next landing's `resolve_pins` re-derives the row's default and puts it straight back
        // to OFF. A recorded decision is what survives a library arriving under an open panel,
        // which is exactly the case the second half of this test drives.
        crate::browse::apply_pins(&[(2, true), (3, true)]);

        // THE SHAPE THAT BROKE IT: the panel lists one kind, the section table holds two.
        assert_eq!(
            crate::browse::section_count(),
            4,
            "two sources, a Movies and a Shows library each"
        );
        assert_eq!(
            crate::browse::source_rows().len(),
            2,
            "…of which the panel lists the tab's kind"
        );
        assert_ne!(
            crate::browse::source_rows().len(),
            crate::browse::section_count(),
            "the two quantities the old guard compared can never meet here"
        );

        build_source_menu(false);
        assert!(matches!(menu(), Menu::Source { .. }));
        let rows0 = src_acts_len();
        // …and it settles: every frame the panel stays open is a frame it does NOT rebuild
        for frame in 0..3 {
            assert!(
                !menu_stale(),
                "frame {frame} rebuilt a panel nothing had changed under"
            );
        }

        // a source's libraries landing IS what this rebuild exists for, and still fires — even
        // when the library it brought is not one the panel will OFFER. A shared film library on an
        // account that already owns films defaults OFF (`pins::default_on`), so this landing adds
        // no row; the guard must still notice it, because the same generation carries the landings
        // that DO change the list and the panel cannot tell them apart without rebuilding.
        crate::browse::append_section_for_test(
            1,
            5,
            "Film Club Extra",
            crate::browse::SecKind::Movie,
        );
        assert!(
            menu_stale(),
            "a library landing under the open panel must rebuild it"
        );
        build_source_menu(true);
        assert!(!menu_stale(), "…once, and then settle again");
        assert_eq!(
            src_acts_len(),
            rows0,
            "…and a non-favourite landing is not a row: rebuilt, same list"
        );

        // …while a landing the panel DOES offer proves the rebuild is real rather than a quieted
        // guard. Your own libraries are favourites by default, so this one arrives as a row.
        crate::browse::append_section_for_test(
            0,
            6,
            "Home Movies",
            crate::browse::SecKind::Movie,
        );
        assert!(menu_stale(), "the second landing is seen too");
        build_source_menu(true);
        assert_eq!(
            src_acts_len(),
            rows0 + 1,
            "the rebuild is real — the new library is a row"
        );

        // …and the key is the panel's, not the screen's: closing takes the action list with it, so
        // nothing is left behind for the next open to inherit
        close_menu();
        assert_eq!(
            src_acts_len(),
            0,
            "the closed panel holds no rows to act on"
        );
        crate::browse::reset();
        set_menu(menu0);
    }

    // ---- the input ladders, with a panel open ---------------------------------------------------
    //
    #[test]
    fn sources_panel_is_a_library_picker_only() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let menu0 = take_menu();
        // The guard comes BEFORE the seed: `append_sections` resolves each row's
        // default the moment it lands, against whatever session file is live at that
        // instant — so a redirect installed afterwards grades pins already decided
        // against the developer's own `auth.json`.
        let _t = crate::plex::session::TempSession::new("picker-level");
        _t.watching("u-picker-level");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        // The panel is FAVOURITE-scoped now, and under `pins::default_on` a shared library of a
        // type you already own starts OFF — so the friend's two libraries would not be offered at
        // all and this fixture would collapse to one row. Favourite them explicitly: what is under
        // test here is the panel's LEVEL and its row identities, not the default.
        //
        // Through the REAL `apply_pins`, not `set_pinned_for_test`, and the difference is the whole
        // reason this note exists: the direct setter writes the live flag and records nothing, so
        // the next landing's `resolve_pins` re-derives the row's default and puts it straight back
        // to OFF. A recorded decision is what survives a library arriving under an open panel,
        // which is exactly the case the second half of this test drives.
        crate::browse::apply_pins(&[(2, true), (3, true)]);

        open_source_menu();
        assert!(matches!(menu(), Menu::Source { .. }));
        assert_eq!(table().sel, 0);
        move_focus(SDLK_DOWN);
        assert_eq!(table().sel, 1);
        move_focus(SDLK_LEFT);
        assert_eq!(table().sel, 1, "LEFT cannot reveal a Home-pinning mode");
        assert_eq!(src_action(1), SrcAction::Library(2));

        // Reset on the way OUT: the panel's popover counts in `popover::OPEN_COUNT`, a process
        // static, and leaving it open here failed `popover`'s `!any_open()` assertions in whichever
        // test happened to run between this one and the next library test that closed it.
        close_menu();
        crate::browse::reset();
        set_menu(menu0);
        clear();
    }

    /// The whole point of the constant pitch: a Latin section and a Cyrillic one space their
    /// letters identically, and the long one scrolls instead of compressing. Both rail statics
    /// are untouched here — [`rail_geom`] and [`rail_scroll_target`] are pure, which is why they
    /// were split out of the draw.
    #[test]
    fn a_long_alphabet_scrolls_rather_than_squeezing_its_letters() {
        let (y_short, x_short, win_short, max_short) = rail_geom(9); // Movies, Latin
        let (y_long, x_long, win_long, max_long) = rail_geom(30); // a Cyrillic section

        // same origin, so the rail does not move between sections…
        assert_eq!((y_short, x_short), (y_long, x_long));
        // …the short one is exactly as tall as its letters and never scrolls…
        assert_eq!(win_short, 9.0 * RAIL_PITCH);
        assert_eq!(max_short, 0.0);
        // …and the long one fills the band and scrolls the remainder, at the SAME pitch
        assert!(
            max_long > 0.0,
            "30 letters must not fit the band at {RAIL_PITCH} px"
        );
        assert_eq!(win_long + max_long, 30.0 * RAIL_PITCH);
        assert!(win_long <= SCR_H - GRID_TOP - 40.0);
    }

    /// The window invariant, at every letter of a section that does not fit: whatever the scroll
    /// was, one step of the reveal rule leaves the driving letter fully on screen — and inside the
    /// one-letter margin the edge fade occupies, so the letter you are on is never the faded one.
    #[test]
    fn the_reveal_rule_keeps_the_driving_letter_clear_of_both_fades() {
        const N: usize = 30;
        let (_, _, win_h, max) = rail_geom(N);
        for drive in 0..N {
            // from the top, from the bottom, and from where the previous letter left it
            for from in [0.0, max, drive as f32 * RAIL_PITCH] {
                let s = rail_scroll_target(from, drive, N);
                assert!(
                    (0.0..=max).contains(&s),
                    "scroll {s} escaped 0..={max} at letter {drive}"
                );
                let top = drive as f32 * RAIL_PITCH - s; // the letter's slot, window-relative
                let margin = if drive == 0 || drive + 1 == N {
                    0.0
                } else {
                    RAIL_PITCH
                };
                assert!(
                    top >= margin - 0.01 && top + RAIL_PITCH <= win_h - margin + 0.01,
                    "letter {drive} sits at {top}..{} in a {win_h} window (scroll {s} from {from})",
                    top + RAIL_PITCH,
                );
            }
        }
    }

    /// A section short enough to fit never scrolls, whichever letter drives — otherwise the rail
    /// would drift on a screen whose alphabet is entirely visible.
    #[test]
    fn a_rail_that_fits_never_scrolls() {
        for drive in 0..9 {
            assert_eq!(rail_scroll_target(0.0, drive, 9), 0.0);
        }
    }

    /// **The chip an open menu is lifted out of its own scrim by, resolved by IDENTITY.**
    ///
    /// The chip row is two long with one source and three with two, so the same menu is a different
    /// index on the two — and the whole point of [`Chip`] is that an index is not an identity (a
    /// `_` catch-all here is the bug that once made inserting Source at 0 open the Sort menu).
    /// Getting this wrong is silent in a particular way: the LIFT would land on the wrong chip, so
    /// the menu's own chip stays dimmed under it while a neighbour brightens for no reason.
    #[test]
    fn the_lifted_chip_is_the_one_its_menu_belongs_to_whatever_the_row_length() {
        // Genre keeps the FILTER chip's panel: it is drilled into from Filter, not a fourth chip
        let cases = [
            (
                Menu::Source {
                    gen: 0,
                    acts: Vec::new(),
                },
                Chip::Source,
            ),
            (Menu::Sort { built: 3 }, Chip::Sort),
            (Menu::Filter, Chip::Filter),
            (Menu::Genre { built: 7 }, Chip::Filter),
        ];
        for (m, want) in &cases {
            for row in [&CHIPS_MANY[..], &CHIPS_ONE[..]] {
                let expect = row.iter().position(|c| c == want);
                assert_eq!(
                    menu_chip_of(m, row),
                    expect,
                    "{m:?} on a {}-chip row",
                    row.len()
                );
            }
        }
        // …and the two rows that draw no chip for the open menu answer NOTHING rather than an index
        // into a row that is not on screen — a failed listing collapses the row to Source alone.
        assert_eq!(
            menu_chip_of(&Menu::Sort { built: 0 }, &CHIPS_SOURCE_ONLY),
            None
        );
        assert_eq!(menu_chip_of(&Menu::Filter, &CHIPS_NONE), None);
        assert_eq!(
            menu_chip_of(&Menu::None, &CHIPS_MANY),
            None,
            "no menu, nothing to lift"
        );
    }
    // ---- Layout: the ONE document projection ------------------------------------------------

    /// The blocks stack in the order the design draws them, and each one's origin is the sum of
    /// what is above it. Graded across the shapes a real library takes: with and without the chip,
    /// with 0 / 1 / 12 shelves.
    #[test]
    fn the_document_stacks_chip_then_shelves_then_the_grid() {
        let bare = Layout::new(false, 0, 40, true);
        assert_eq!(bare.grid_block_top(), 0.0, "no chip, no shelves: the grid heads the page");
        assert_eq!(bare.grid_top(), GRID_HEAD, "…under its own heading row");

        let chipped = Layout::new(true, 0, 40, true);
        assert_eq!(chipped.grid_block_top(), HEADER_H, "the chip's block leads");

        let shelved = Layout::new(true, 12, 40, true);
        assert_eq!(
            shelved.grid_block_top(),
            HEADER_H + 12.0 * crate::ui::consts::ROW_PITCH,
            "twelve shelves push the grid exactly twelve pitches down"
        );
        // …and a shelf's own origin is the one Home hangs a shelf from, so `card_row` draws here
        // unchanged: heading at origin − TITLE_DY, cards at origin + CARD_DY
        assert_eq!(shelved.shelf_origin(0), HEADER_H);
        assert_eq!(
            shelved.shelf_origin(3) - shelved.shelf_origin(2),
            crate::ui::consts::ROW_PITCH
        );

        // a grid with nothing in it draws no heading and no control row, so the block IS the grid
        let empty = Layout::new(true, 2, 0, false);
        assert_eq!(empty.grid_top(), empty.grid_block_top());
    }

    /// **`doc_to_grid` is the fix for this screen's sharpest geometry bug.** `browse::want` derived
    /// its page window straight from `SCROLL`, i.e. assuming scroll 0 is grid row 0 — so under a
    /// document with a header and twelve shelves it asked for pages thirteen rows into the catalog
    /// while row 0's own slots sat unloaded.
    #[test]
    fn the_page_window_is_grid_local_whatever_is_above_the_grid() {
        let rows = 1667; // a 10k-item section
        let flat = Layout::new(false, 0, rows, true);
        let deep = Layout::new(true, 12, rows, true);

        // at each document's own head, both ask for row 0 — the bug was that only the flat one did
        assert_eq!(flat.visible_rows(0.0).0, 0);
        assert_eq!(deep.visible_rows(0.0).0, 0);
        assert_eq!(
            deep.visible_rows(deep.grid_top()).0,
            0,
            "scrolled exactly to the grid's top, the first wanted row is still 0"
        );
        // …and one pitch further down, both have moved by exactly one row
        assert_eq!(
            deep.visible_rows(deep.grid_top() + PITCH).0,
            flat.visible_rows(flat.grid_top() + PITCH).0
        );
        // the window never runs past the catalog
        assert!(deep.visible_rows(1.0e9).1 <= rows);
    }

    /// **`max_scroll` and `row_reveal` are ONE invariant**, and this is the test that holds them to
    /// it: if they disagree the last row's caption sits off the panel, which is the bug the old
    /// hand-written `max_y`/`lo` pair carried a paragraph about.
    #[test]
    fn the_last_row_can_always_reach_its_own_caption() {
        for shelves in [0usize, 1, 12] {
            for rows in [1usize, 2, 40, 1667] {
                let lay = Layout::new(true, shelves, rows, true);
                let last = lay.row_reveal(rows - 1);
                assert!(
                    last <= lay.max_scroll() + 0.001,
                    "row_reveal must never ask past max_scroll ({shelves} shelves, {rows} rows)"
                );
                // the whole last row — card, label band and the air under it — is on the panel
                let bottom = CONTENT_TOP + lay.grid_top() + rows as f32 * PITCH - last;
                assert!(
                    bottom <= SCR_H + 0.001,
                    "the last row's caption is off the panel ({shelves} shelves, {rows} rows): {bottom}"
                );
            }
        }
    }

    /// Row snapping puts the focused row's OWN TOP EDGE at the viewport, clamped — not the minimal
    /// reveal `card_row::reveal` performs, which is right for a shelf inside a page and wrong for a
    /// wall of posters you walk down.
    #[test]
    fn row_snapping_targets_the_rows_own_top_edge() {
        let lay = Layout::new(false, 0, 40, true);
        // **Row 0 is the canvas's own exception**: its target is the grid BLOCK's top, so the
        // heading and the two chips directly above it stay on screen. Snapping past them would
        // scroll away the count and the controls the moment focus entered the grid.
        assert_eq!(lay.row_reveal(0), lay.grid_block_top());
        assert_ne!(
            lay.row_reveal(0),
            lay.grid_top(),
            "…which is NOT the row's own top edge — every other row is"
        );
        assert_eq!(lay.row_reveal(3), lay.grid_top() + 3.0 * PITCH);
        // …and the clamp is the document's, so the last rows share one resting scroll
        assert_eq!(lay.row_reveal(39), lay.max_scroll());
    }

    // ---- the focus projection ----------------------------------------------------------------

    /// **The zones a page actually DRAWS, across the shapes that broke the first draft's ladder.**
    /// An empty library draws no grid heading, no Sort, no Filter, no count and no rail, so a
    /// vertical walk must not be able to park on any of them.
    #[test]
    fn the_zone_projection_never_offers_an_undrawn_control() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("zones");
        _t.watching("u-zones");
        crate::browse::reset();

        // nothing discovered at all: the bar, and nothing else
        let bare = Layout::new(false, 0, 0, false);
        assert_eq!(zones(&bare), vec![Area::Tabs]);
        assert_eq!(step_zone(&bare, Area::Tabs, 1), None, "DOWN has nowhere to go");

        // shelves but an empty grid — the design draws no heading over nothing
        let shelves_only = Layout::new(true, 2, 0, false);
        let z = zones(&shelves_only);
        assert_eq!(
            z,
            vec![Area::Tabs, Area::LibChip, Area::Shelf],
            "the bar, the chip, then the shelf RUN as one rung"
        );
        assert!(!zone_drawn(&shelves_only, Area::Toolbar), "no Sort over an empty grid");
        assert!(!zone_drawn(&shelves_only, Area::Grid));
        assert!(!zone_drawn(&shelves_only, Area::Rail));

        // …and the full page. `zones` is a pure function of the LAYOUT — that is what lets this
        // hand it a shape and grade the answer, instead of the answer depending on whatever the
        // live store happens to hold.
        let full = Layout::new(true, 1, 40, true);
        let z = zones(&full);
        assert_eq!(z[0], Area::Tabs);
        assert_eq!(z[1], Area::LibChip);
        assert_eq!(z[2], Area::Shelf);
        assert!(z.contains(&Area::Toolbar) && z.contains(&Area::Grid));

        // **The run of shelves is ONE rung too, and for a harder reason than the bar's.** `Area`
        // names a KIND of zone, not an instance, so a list holding N `Area::Shelf` entries is a
        // list `step_zone` cannot walk: `position` finds the first of them whatever shelf focus is
        // actually on, and DOWN off the LAST shelf lands back on the FIRST instead of leaving the
        // run. Device-visible as a walk that never reaches the grid. The shelf INDEX is
        // `move_focus`'s to move; this projection only says where the run begins and ends.
        assert_eq!(
            step_zone(&full, Area::Shelf, 1),
            Some(Area::Toolbar),
            "DOWN off the last shelf leaves the run"
        );
        assert_eq!(
            step_zone(&shelves_only, Area::Shelf, 1),
            None,
            "…and with no grid under them, the run is the foot of the page"
        );
        assert_eq!(step_zone(&full, Area::Shelf, -1), Some(Area::LibChip));

        // the bar is ONE rung: the chip and the pills leave it together
        assert!(zone_drawn(&bare, Area::Chip), "the bar is always drawn");
        assert_eq!(
            step_zone(&full, Area::Tabs, 1),
            Some(Area::LibChip),
            "DOWN off the bar reaches the head of the document"
        );
        // the failure read-out replaces the grid's whole region and offers exactly one control
        let dead = Layout::failed(true, 2);
        let zf = zones(&dead);
        assert!(zf.contains(&Area::Status), "the read-out is a zone");
        assert!(!zf.contains(&Area::Grid) && !zf.contains(&Area::Toolbar));
        assert!(
            zf.contains(&Area::LibChip),
            "…and the chip survives it: navigation is how you leave a dead source"
        );
        crate::browse::reset();
    }

    /// **The play triangle is a PROMISE, and only a deck keeps it.** The app's one rule: a visual
    /// play indicator means the press starts the video, a progress bar means viewing progress, and
    /// a card with no play indicator navigates. Before it, `want_play` was
    /// `from_deck || kind == 3`, so an episode played immediately from ANY shelf — pressing a
    /// "Recently Released Episodes" tile started something the viewer was only browsing — while
    /// every landscape tile drew the amber ▶ whatever its press did.
    #[test]
    fn only_a_deck_promises_playback() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("promise");
        _t.watching("u-promise");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(12);
        crate::browse::section_hubs::seed_shelves_for_test(
            crate::browse::cur(),
            // a deck and a discovery shelf, side by side in one library
            &["movie.inprogress.1", "tv.recentlyreleased.1"],
            3,
        );
        crate::browse::section_hubs::seed_landscape_for_test(crate::browse::cur(), "The Bear");
        let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
        assert!(shelves[0].is_continue, "shelf 0 is the deck");
        assert!(!shelves[1].is_continue, "shelf 1 is a discovery shelf");

        // the ACTIVATION half: only the deck's tile reports `from_deck`, which is the one thing
        // `app.rs` turns into a play
        let lay = layout();
        enter_zone(&lay, Area::Shelf, 1);
        unsafe {
            SHELF_F = 0;
            SHELF_C = 0;
        }
        assert!(focused_from_deck(), "a deck tile plays");
        unsafe { SHELF_F = 1 };
        assert!(
            !focused_from_deck(),
            "…and a discovery tile navigates, however much it looks like the same object"
        );

        // the DRAW half reads the same flag, so the promise and the behaviour cannot disagree
        assert!(shelves[0].is_continue && !shelves[1].is_continue);
        crate::browse::reset();
    }

    /// **A queued SECTION switch is scoped to the table it was chosen against.** The grid half
    /// carried its `table_epoch` from the start; the section half was a bare index, and a bare index
    /// is not a name — `browse::reset` clears and RENUMBERS the table on a profile switch, so a
    /// switch queued before one and flushed after it lands on whatever library now happens to hold
    /// that number, which is a different person's.
    #[test]
    fn a_queued_section_switch_does_not_survive_the_table_being_renumbered() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("secepoch");
        _t.watching("u-secepoch");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(12);

        // a control leg first: within ONE table the switch commits
        let here = crate::browse::cur();
        let other = (0..crate::browse::section_count())
            .find(|i| *i != here)
            .expect("the seeded table has more than one library");
        request_section(other);
        apply_pending();
        assert_eq!(crate::browse::cur(), other, "an ordinary switch lands");

        // …and now the same press across a profile change. Queue a switch to a DIFFERENT library
        // than the cursor will hold after the reset, so the guard and its absence are observable:
        // `apply_section` no-ops on an index that already equals `cur`, which is how the first
        // version of this test managed to pass against the missing guard.
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(12);
        let before = crate::browse::cur();
        let away = (0..crate::browse::section_count())
            .find(|i| *i != before)
            .expect("more than one library");
        request_section(away);
        assert!(pending().section.is_some());

        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        assert_eq!(crate::browse::cur(), before, "the cursor is back at the head");
        apply_pending();
        assert_eq!(
            crate::browse::cur(),
            before,
            "an index chosen against the OLD table may not move the cursor in the new one"
        );

        // **Reset on the way OUT.** `request_section` arms the fader, and `apply_pending` does not
        // stop it — a fader left mid-phase is process-global state another module's test reads
        // (`a_shelf_commit_waits_for_an_armed_press_to_finish` asks `hidden_page()`, which is
        // exactly "is a PAGE-scoped fade running"). It passed alone and failed in the suite, which
        // is this module's documented pollution shape.
        clear();
        crate::browse::reset();
    }

    /// **The LIFTED copy of a focused shelf tile is the tile the page drew.** `popover::Opener`'s
    /// whole contract: the item context menu re-draws the card it was opened on over its own scrim,
    /// so the one thing on screen at full strength is the thing the menu is about. A landscape
    /// tile's identity — the show's name and its watch glyph — lives INSIDE the artwork, so a lift
    /// that skipped the state line put an anonymous frame of television there instead.
    ///
    /// Graded on the two draws sharing ONE geometry and one predicate, which is what the defect
    /// was: `draw_focused` ran on the lifted rect and the line did not run at all.
    #[test]
    fn the_lifted_shelf_tile_carries_its_state_line() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("lift");
        _t.watching("u-lift");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(12);
        crate::browse::section_hubs::seed_shelves_for_test(
            crate::browse::cur(),
            &["movie.inprogress.1"],
            3,
        );
        // make it a LANDSCAPE shelf, which is the case with a line to lose
        crate::browse::section_hubs::seed_landscape_for_test(crate::browse::cur(), "The Bear");

        let lay = layout();
        enter_zone(&lay, Area::Shelf, 1);
        unsafe {
            SHELF_F = 0;
            SHELF_C = 1;
            (*addr_of_mut!(SCROLL)).jump(lay.shelf_reveal(0));
        }

        // the lift draws from this rect, and it is the SCALED one — the same `draw_focused` uses,
        // so the line composed on it lands on the artwork rather than beside it
        let base = focused_shelf_base().expect("a focused shelf tile has a rect");
        assert!(base.w > 0.0 && base.h > 0.0);
        assert!(
            shelf_style(0).w <= base.w,
            "the base is the scaled rect: a focused tile is at or above its resting width"
        );

        // …and the shelf really is the landscape kind, which is the branch that owes a line
        let shelves = crate::browse::section_hubs::shelves(crate::browse::cur());
        assert!(shelves[0].landscape, "a landscape shelf");
        assert!(
            focused_item().is_some(),
            "…and the lift has the row its line is built from"
        );
        crate::browse::reset();
    }

    /// **What an episode tile says, and where.** The design's ruling (`Library Screens.dc.html` E)
    /// is that a still is the one tile in the app carrying no title of its own, so *whatever the
    /// poster prints, the still prints too*: the SHOW goes inside the artwork on every tile, and
    /// FOCUS reveals the EPISODE — its title over its `S3 · E4` address, never the show again.
    /// "One fact, one place", and it is also what keeps the shelf's own labels from disagreeing:
    /// the earlier build put the show on the focused rung and the episode under it, so an unfocused
    /// tile named nothing at all.
    #[test]
    fn a_focused_episode_tile_reveals_the_episode_and_not_the_show() {
        let ep = |s: c_int, e: c_int, title: &str, show: &str| PmsMovie {
            kind: 3,
            season_index: s,
            ep_index: e,
            title: title.into(),
            show_title: show.into(),
            ..Default::default()
        };
        let shelf = |items: Vec<PmsMovie>| crate::browse::section_hubs::Shelf {
            id: "tv.recentlyreleased".into(),
            title: "Recently Released Episodes".into(),
            is_continue: false,
            landscape: true,
            items,
        };

        // `TileLabel` holds `CString`s for the draw; these read them back as text
        let title = |l: &card_row::TileLabel| {
            l.title
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let caption = |l: &card_row::TileLabel| {
            l.caption
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };

        let dated = |m: PmsMovie| PmsMovie {
            aired: "2026-09-11".into(),
            ..m
        };

        let sh = shelf(vec![dated(ep(3, 4, "Violet", "The Bear"))]);
        let l = shelf_label(&sh, 0);
        assert_eq!(title(&l), "Violet", "the EPISODE's NAME takes the title rung");
        assert_eq!(
            caption(&l),
            "11 Sep 2026",
            "…over its one trailing FACT, which on a discovery shelf is the release date",
        );
        assert!(
            !title(&l).contains("The Bear") && !caption(&l).contains("The Bear"),
            "the show is printed on the artwork and must not be repeated below it"
        );
        assert!(
            !caption(&l).contains("S3") && !caption(&l).contains("E4"),
            "…and NEITHER is the address, which moved onto the artwork on 2026-09-05: four tiles \
             of one show differ only by number, so the number cannot be the thing behind focus"
        );

        // **Continue Watching trails time left instead**, because for a part-watched episode that
        // is the fact you came for. The shelf decides and not the item.
        let mut deck = shelf(vec![PmsMovie {
            dur_ns: 30 * 60 * 1_000_000_000,
            resume_ms: 6 * 60 * 1000,
            ..dated(ep(3, 4, "Violet", "The Bear"))
        }]);
        deck.is_continue = true;
        assert_eq!(caption(&shelf_label(&deck, 0)), "24 min left");
        // …and a deck tile never started has no time to report, so it falls back to the date
        // rather than to an empty rung.
        let mut next_up = shelf(vec![dated(ep(3, 5, "Ice Chips", "The Bear"))]);
        next_up.is_continue = true;
        assert_eq!(caption(&shelf_label(&next_up, 0)), "11 Sep 2026");

        // …an episode the server dated to nothing at all draws ONE rung, not an empty second one
        let sh = shelf(vec![ep(3, 4, "Violet", "The Bear")]);
        let l = shelf_label(&sh, 0);
        assert_eq!(title(&l), "Violet");
        assert_eq!(caption(&l), "");
        // …and an episode with no title of its own falls back to its address on the title rung
        let sh = shelf(vec![dated(ep(3, 4, "", "The Bear"))]);
        assert_eq!(title(&shelf_label(&sh, 0)), "S3 \u{b7} E4");
        // an episode whose title IS the show's says it once, as the address
        let sh = shelf(vec![dated(ep(2, 1, "X", "X"))]);
        assert_eq!(title(&shelf_label(&sh, 0)), "S2 \u{b7} E1");

        // …and a POSTER shelf is untouched: its tiles still name the item on focus
        let mut poster = shelf(vec![dated(ep(3, 4, "Violet", "The Bear"))]);
        poster.landscape = false;
        assert_eq!(title(&shelf_label(&poster, 0)), "Violet");
    }

    /// A landscape row is a SHORTER band, and the document has to know: a uniform pitch would leave
    /// a 139px hole under every episode shelf.
    #[test]
    fn an_episode_shelf_takes_a_shorter_band_than_a_poster_shelf() {
        use crate::ui::consts::ROW_PITCH;
        // graded at the FOCUSED band, where the two shapes' difference is the tile height alone
        let open = card_row::UNDER_LABEL_H;
        let poster = shelf_pitch_of(false, open);
        let landscape = shelf_pitch_of(true, open);
        assert_eq!(poster, ROW_PITCH);
        assert!(landscape < poster, "a landscape row is shorter");
        assert_eq!(poster - landscape, CARD_H - RowStyle::EPISODE.h);

        // …and the document sums the ACTUAL pitches, so a mixed page puts the grid where the
        // shelves above it really end
        let mixed = Layout::with_pitches(true, &[landscape, poster, landscape], 40, true);
        assert_eq!(mixed.shelf_origin(0), mixed.header_h());
        assert_eq!(mixed.shelf_origin(1), mixed.header_h() + landscape);
        assert_eq!(mixed.shelf_origin(2), mixed.header_h() + landscape + poster);
        assert_eq!(
            mixed.grid_block_top(),
            mixed.header_h() + 2.0 * landscape + poster
        );
        // the uniform case is unchanged, which is what every other geometry test asserts
        let uniform = Layout::new(true, 3, 40, true);
        assert_eq!(uniform.grid_block_top(), uniform.header_h() + 3.0 * poster);
    }

    /// **A card near the top edge is DRAWN, not culled.** The grid used to cull any row whose
    /// bottom edge had climbed above the tab track, which was free while an opaque scrim covered
    /// that band — the fixed toolbar's. With the toolbar gone and the track a CENTRED pill, the
    /// screen either side of it at that height is open, so the term made cards vanish in plain
    /// sight while scrolling. The shelves never had it and never showed the fault.
    #[test]
    fn a_card_climbing_past_the_tab_track_is_still_drawn() {
        use crate::ui::widgets::TOP_BAR_BOTTOM;
        // a grid row whose BOTTOM edge is above the track's bottom but still on the panel: the
        // exact band the old term threw away
        let y = TOP_BAR_BOTTOM - CARD_H - 8.0;
        assert!(y + CARD_H < TOP_BAR_BOTTOM, "…this is that band");
        assert!(y + CARD_H > 0.0, "…and it is still on the panel");
        assert!(
            block_on_screen(y, CARD_H),
            "a card the viewer can see must be drawn"
        );

        // …and a shelf band at the same height answers identically, which is the whole point of
        // there being one rule
        assert!(block_on_screen(y, crate::ui::consts::ROW_PITCH));

        // the rule still culls what is genuinely gone, above and below
        assert!(!block_on_screen(-CARD_H - GLOW_PAD - 1.0, CARD_H));
        assert!(!block_on_screen(SCR_H + GLOW_PAD + 1.0, CARD_H));
    }

    /// **A shelf tile is a CARD surface**, which is what arms the press-and-hold. It was not, and
    /// the consequence was invisible from here and obvious on the television: `app.rs` arms the
    /// press from `focus_is_card`, so a held OK on a shelf tile never became a hold — the key-up
    /// arrived as an ordinary tap and the tile opened its detail page. The item context menu was
    /// unreachable on the shelves, and with it *Remove from Continue Watching*, which is the one
    /// row a section deck exists to offer.
    #[test]
    fn a_shelf_tile_arms_the_hold_and_anchors_the_menu() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("shelf-hold");
        _t.watching("u-shelf-hold");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(12);
        crate::browse::section_hubs::seed_shelves_for_test(
            crate::browse::cur(),
            &["movie.inprogress.1", "movie.recentlyadded.1"],
            3,
        );
        let lay = layout();

        // the deck row first: a card, and one whose activation carries the deck flag
        enter_zone(&lay, Area::Shelf, 1);
        assert!(focus_is_card(), "a shelf tile arms the press");
        assert!(focused_item().is_some());
        assert!(
            focused_from_deck(),
            "…and the section's Continue Watching row says so, which is what puts Remove in the menu"
        );
        assert!(
            focused_card_rect().is_some(),
            "…and the menu has a tile to anchor beside"
        );

        // the second shelf is NOT a deck, so the same press must not claim it is
        unsafe { SHELF_F = 1 };
        assert!(focus_is_card());
        assert!(!focused_from_deck());

        // and the chrome is not a card surface, which is what keeps a press off the chip and pills
        enter_zone(&lay, Area::Tabs, 1);
        assert!(!focus_is_card(), "the tab strip arms nothing");
        crate::browse::reset();
    }

    /// **A store replaced by somebody else remounts the PAGE and lands on the new library's own
    /// coordinates.** The watchdog used to remount only `if !xf().is_swapping()`, which was how it
    /// told our own commit from a foreign one — at the price of being unable to act during a fade
    /// at all, so a full-document change absorbed inside a grid reload was never noticed. And when
    /// it did fire it only faded: the incoming library was mounted using the OUTGOING one's focus
    /// and scroll. Reachable when the favourites editor re-points `cur()` while the Library is
    /// frozen under Settings.
    #[test]
    fn a_foreign_store_change_remounts_the_whole_page() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("foreign");
        _t.watching("u-foreign");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);
        unsafe { EPOCH = crate::browse::query_gen() };

        // deep in the grid, and a GRID-scoped reload running — the case that used to be absorbed
        let lay = layout();
        enter_zone(&lay, Area::Grid, 1);
        unsafe {
            GR = 9;
            (*addr_of_mut!(SCROLL)).jump(lay.row_reveal(9));
        }
        request_grid(GridAct::Unwatched(true));
        assert_eq!(unsafe { addr_of!(SCOPE).read() }, Scope::Grid);
        assert!(xf().is_swapping());

        // …and somebody else replaces the store underneath
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        assert_ne!(
            crate::browse::query_gen(),
            unsafe { addr_of!(EPOCH).read() },
            "the store really did move"
        );

        watch_foreign_store();

        assert_eq!(
            unsafe { addr_of!(SCOPE).read() },
            Scope::Page,
            "a foreign change replaces the whole column, not the grid"
        );
        assert_eq!(
            unsafe { addr_of!(EPOCH).read() },
            crate::browse::query_gen(),
            "…and it is accounted for, so it fires exactly once"
        );
        // the new library's own view, not the old one's deep row
        assert_eq!(unsafe { addr_of!(GR).read() }, 0);
        assert!(unsafe { (*addr_of!(SCROLL)).pos } < 1.0);

        // …and a frame with nothing foreign in it does nothing at all
        let before = unsafe { addr_of!(SCOPE).read() };
        unsafe { SCOPE = Scope::Grid };
        watch_foreign_store();
        assert_eq!(
            unsafe { addr_of!(SCOPE).read() },
            Scope::Grid,
            "a quiet frame is not a remount"
        );
        let _ = before;

        take_pending();
        crate::browse::reset();
    }

    /// **A shelf commit does not land inside a press.** At the head a commit re-resolves focus onto
    /// the new first content zone, and `at_document_head` cannot tell "the head of a composed page"
    /// from "grid row 0 of a page that has no chip and no shelves yet" — with neither drawn, row
    /// 0's own reveal IS zero. The viewport does not move there, so the visible effect is only the
    /// ring stepping onto the first shelf of a page that has just composed itself, which is
    /// accepted. What is not is doing it under an ARMED press, whose deferred activation would then
    /// resolve against the zone focus was moved to rather than the card that was pressed.
    #[test]
    fn a_shelf_commit_waits_for_an_armed_press_to_finish() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("commitpress");
        _t.watching("u-commitpress");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);
        crate::ui::press::cancel();

        // the page a single-library household actually has on arrival: no chip, no shelves yet,
        // focus in the grid block at scroll 0 — where the head and grid row 0 are ONE coordinate
        let lay = layout();
        assert_eq!(lay.shelves, 0, "nothing has landed yet");
        enter_zone(&lay, Area::Grid, 1);
        unsafe { (*addr_of_mut!(SCROLL)).jump(0.0) };
        assert!(at_document_head());
        assert!(!hidden_page(), "the page is on screen");

        assert!(
            may_publish_shelves(),
            "a settled head with no press is when the page may compose itself"
        );

        // …and with a press armed on the card under the ring, it waits
        crate::ui::press::begin(0);
        assert!(crate::ui::press::is_live());
        assert!(
            !may_publish_shelves(),
            "the deferred activation would resolve against the zone focus was moved TO"
        );

        crate::ui::press::cancel();
        assert!(may_publish_shelves(), "…and once the press is over, it may");
        crate::browse::reset();
    }

    /// **A grid-scoped fade does NOT authorise a visible shelf commit.** The permission read every
    /// `Xfade` as a hidden page, which is wrong for exactly the scope that was added beside it: a
    /// `Scope::Grid` fade dips the grid block alone and holds the library chip and the shelves at
    /// full alpha — and those are what a shelf commit moves. So a staged refresh could publish in
    /// the middle of a sort, changing the shelf count and the pitches on screen.
    #[test]
    fn only_a_page_scoped_fade_counts_as_a_hidden_page() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("hiddenscope");
        _t.watching("u-hiddenscope");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);

        // a grid-scoped transaction: the shelves and the chip are still on screen
        request_grid(GridAct::Unwatched(true));
        assert!(xf().is_swapping());
        assert_eq!(unsafe { addr_of!(SCOPE).read() }, Scope::Grid);
        assert!(!hidden_page(), "the document is visible during a grid reload");

        // …and a page-scoped one really does replace the whole column
        take_pending();
        xf().cancel();
        request_section(crate::browse::cur());
        assert_eq!(unsafe { addr_of!(SCOPE).read() }, Scope::Page);
        assert!(hidden_page(), "a section transition replaces the document");

        take_pending();
        crate::browse::reset();
    }

    /// **A heading stops where the alphabet index begins, with the rail's own air between them.**
    /// The report was text rendering UNDER the letters; the fix is a right boundary rather than a
    /// narrower page, so this grades the boundary against the rail's drawn track rather than
    /// against a chosen inset. Nothing here can measure a STRING — `text::elide` reaches
    /// SDL2_ttf, which the host suite cannot link — so the pixel proof is the simulator's; what a
    /// host test can hold is that the number the draw is bounded by really is outside the rail.
    #[test]
    fn a_bounded_heading_stops_short_of_the_rail() {
        let right = MARGIN_X + heading_max_w();
        assert!(
            right <= SCR_W - MARGIN_X - RAIL_TRACK_W,
            "the heading's right edge ({right}) is inside the rail's track"
        );
        assert!(
            SCR_W - MARGIN_X - RAIL_TRACK_W - right >= theme::space::XS,
            "…and there is a rung of air between them, not a touching edge"
        );
        // …and it is not a GLOBAL narrowing: the bound IS the grid's own content column, the same
        // one the six poster columns fill exactly, so nothing outside the rail's band was given up
        assert!(
            (right - GRID_R).abs() < 0.01,
            "the bound is the content column's own edge"
        );
    }

    /// **A hover that moves the focus stop says so, so an armed press can be aborted.**
    /// `ui::press`'s contract is that focus cannot move mid-press; the nav keys pay it by calling
    /// `press::cancel` and hover owed the same. On this screen it did not: the Magic Remote's
    /// pointer wakes on the smallest movement, so drifting from one card to another during a press
    /// committed the click — or opened the press-and-hold menu — on a tile that was no longer the
    /// one being pressed.
    #[test]
    fn a_hover_that_moves_the_focus_stop_reports_it() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("hovermove");
        _t.watching("u-hovermove");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);
        crate::browse::section_hubs::seed_shelves_for_test(crate::browse::cur(), &["a", "b"], 4);

        // stand on a grid tile, then hover the one beside it: the stop moved
        let lay = layout();
        enter_zone(&lay, Area::Grid, 1);
        unsafe {
            GR = 0;
            GC = 0;
            // the walk sets a TARGET; the pointer reads the drawn frame, so park the document
            // where a stepped spring would have taken it
            (*addr_of_mut!(SCROLL)).jump(lay.row_reveal(0));
        }
        let (x, y) = {
            let r = focused_card_base().expect("a focused tile has a rect");
            (r.x + r.w + LGAP + 4.0, r.y + 4.0)
        };
        assert!(
            pointer_focus(x, y),
            "hovering the next card moves the stop, and a press armed on the last one must die"
        );

        // …and hovering the SAME card again is not a move, so it may not abort a live press
        let (x, y) = {
            let r = focused_card_base().expect("a focused tile has a rect");
            (r.x + 4.0, r.y + 4.0)
        };
        assert!(!pointer_focus(x, y), "staying put is not a move");

        // …nor is dead space: a miss leaves focus where it was
        assert!(!pointer_focus(2.0, SCR_H - 2.0), "a miss is not a move");

        crate::browse::reset();
    }

    /// **Coming back to a shelf comes back to the SHELF, not to the grid at the same scroll.**
    /// `browse` remembers a section's scroll and its GRID index — all there was to remember while
    /// the grid was the screen. Under one document those two name different blocks whenever the
    /// visitor left from a shelf: the scroll returned to the shelf while the band was seated in the
    /// grid, whose reveal target is thousands of pixels further down, so the page slid out from
    /// under the viewer on the frame after re-entry.
    ///
    /// The red here was SIMULATED rather than historical: the rule this grades is a new pure
    /// function, so the test cannot be compiled against the code that lacked it. Neutralised to the
    /// old behaviour (`scroll <= 0.5 ? first_content : Grid`) it fails on every shelf leg.
    #[test]
    fn re_entry_lands_on_the_block_the_restored_scroll_is_showing() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("reenter");
        _t.watching("u-reenter");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);
        crate::browse::section_hubs::seed_shelves_for_test(
            crate::browse::cur(),
            &["a", "b", "c", "d"],
            4,
        );
        let lay = layout();
        assert_eq!(lay.shelves, 4);

        // the head: a first visit restores `(0, 0)` and must open on the head of the document,
        // never on grid row 0 — whose target would drag the page past every shelf
        assert_eq!(
            lay.seat_for_scroll(0.0, 0).map(|(a, _)| a),
            lay.first_content(),
            "a first visit opens at the head"
        );

        // a shelf: the scroll the walk itself would have parked at, so the seat is that shelf and
        // its own index — and NOT the stale grid row saved beside it
        for i in 1..lay.shelves {
            assert_eq!(
                lay.seat_for_scroll(lay.shelf_reveal(i), 11),
                Some((Area::Shelf, i)),
                "shelf {i} comes back as shelf {i}"
            );
        }

        // …and a visit that really was in the grid still comes back to the grid, ON THAT ROW
        assert_eq!(
            lay.seat_for_scroll(lay.row_reveal(7), 7),
            Some((Area::Grid, 7)),
            "a grid row comes back to the grid"
        );

        // **The toolbar and grid row 0 SHARE a scroll coordinate** — `row_reveal(0)` IS
        // `grid_block_top()`, which is the Toolbar's own target. A visit that left from Sort/Filter
        // therefore restores a scroll naming row 0 while the saved grid row is still whatever deep
        // row was last walked; seating `Grid` and keeping that row asked the spring for it and slid
        // the document thousands of pixels on the frame after re-entry. The seat carries the row
        // the SCROLL is showing, so the answer does not move.
        assert_eq!(
            lay.seat_for_scroll(lay.grid_block_top(), 11),
            Some((Area::Grid, 0)),
            "a scroll showing the grid's head seats row 0, not the stale deep row"
        );

        crate::browse::reset();
    }

    /// **Changing Sort must not throw the page back to the recommendations.** The reported
    /// symptom, verbatim: "Changing Sort in All currently appears to reload/rebuild the entire
    /// table/page. As a result, the user is thrown all the way back to the top/recommendations and
    /// has to navigate back to All just to see the result of the sort they selected."
    ///
    /// The partial-update transaction alone does not fix it, which is what makes this test worth
    /// having. `browse::requery` empties the store synchronously on the press, so for the length of
    /// the fade the grid heading, its control row and the rail are all "controls acting on
    /// nothing" — and [`sync_readout_focus`]'s general clamp then correctly moves focus off a zone
    /// that is not drawn, to the head of the document, which is the library chip above every
    /// shelf. The scroll spring follows focus, and the page flies to the top.
    #[test]
    fn changing_sort_leaves_you_standing_on_the_control_row() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("sort-holds");
        _t.watching("u-sort-holds");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);
        crate::browse::section_hubs::seed_shelves_for_test(
            crate::browse::cur(),
            &["movie.inprogress.1", "movie.recentlyadded.1", "movie.genre.1"],
            4,
        );

        // stand on the grid's own Sort/Filter row, the way a user reaching Sort does
        let lay = layout();
        enter_zone(&lay, Area::Toolbar, 1);
        assert_eq!(area(), Area::Toolbar);

        // press Sort. The request arms a GRID-scoped fade…
        request_grid(GridAct::Sort {
            key: "titleSort".into(),
            desc: false,
        });
        assert_eq!(unsafe { addr_of!(SCOPE).read() }, Scope::Grid);
        assert!(xf().is_swapping(), "the grid is dissolving");

        // …and the re-query empties the store in the same breath, which is the state the clamp
        // then sees. (`requery` itself needs a live client; the wipe is what matters here.)
        crate::browse::seed_items_for_test(0);
        assert_eq!(crate::browse::total(), 0);

        sync_readout_focus();

        assert_eq!(
            area(),
            Area::Toolbar,
            "the control row is being REPLACED, not removed — focus must not be sent to the head \
             of the document, which is what threw the user back to the recommendations"
        );

        // and the block whose chrome that focus stands on is still part of the layout, so the
        // scroll target is the grid rather than the page head
        let mid = layout();
        assert!(
            mid.grid_head,
            "the grid block keeps its heading and control row for the length of its own reload"
        );
        assert!(
            mid.grid_block_top() > mid.head(),
            "the grid really is below the recommendations, so the two targets differ"
        );

        // the commit then lands the page on the SORTED grid's first row — not on the head of the
        // document, which is the whole of the report
        crate::browse::seed_items_for_test(120);
        grid_reset();
        let lay = layout();
        assert!(
            (unsafe { addr_of!(SCROLL).read() }.pos - lay.row_reveal(0)).abs() < 0.5,
            "the sorted grid is shown from its first row"
        );
        assert!(
            unsafe { addr_of!(SCROLL).read() }.pos > lay.head() + 1.0,
            "…which is not the top of the page"
        );

        crate::browse::reset();
    }

    /// **The `libosc` sweep really does cross the shelf/grid seam.** It could not before: the
    /// oscillator reversed on a 3 s clock at a 350 ms cadence, i.e. about eight presses a leg,
    /// while this document now spends one press on the chip and one per shelf before the grid
    /// begins. A twelve-shelf library therefore turned round in the middle of the shelves, and the
    /// `fps:library-scroll` scene — whose whole purpose is to sweep the seam between the last shelf
    /// and the poster wall — graded a sweep that never reached it.
    #[test]
    fn the_library_sweep_reaches_the_grid_and_comes_back() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("libosc");
        _t.watching("u-libosc");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        crate::browse::seed_items_for_test(120);
        crate::browse::section_hubs::seed_shelves_for_test(
            crate::browse::cur(),
            &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"],
            4,
        );
        assert_eq!(layout().shelves, 12, "the document really has a shelf run");

        // start where `enter` leaves the band: the head of the document
        let lay = layout();
        enter_zone(&lay, lay.first_content().expect("a page with content"), 1);

        // …and sweep. The cap is generous but FINITE: a sweep that cannot reach the grid is
        // exactly the defect, and a sweep that never turns round is the other one.
        let mut saw_grid = false;
        let mut back_at_head = false;
        for _ in 0..200 {
            osc_step();
            if area() == Area::Grid {
                saw_grid = true;
            }
            if saw_grid && area() == lay.first_content().unwrap() {
                back_at_head = true;
                break;
            }
        }
        assert!(saw_grid, "the sweep crossed the seam into the grid");
        assert!(back_at_head, "…and reversed at the foot and returned to the head");
        crate::browse::reset();
    }

    /// **The grid's rows and its FOCUSED card come out of one expression.** They did not: the
    /// focused card kept the pre-rewrite `GRID_TOP` constant, so under a document that opens with a
    /// chip and shelves its `on_axis` test put it off-panel and the tile you were standing on was
    /// the one tile not drawn. Caught in the simulator, not by any assertion here — which is why
    /// there is now one.
    #[test]
    fn the_focused_card_and_its_row_are_the_same_row() {
        // a plain grid: the row sits at the content edge when scrolled to its own top
        let flat = Layout::new(false, 0, 40, true);
        assert_eq!(flat.row_y(0, flat.grid_top()), CONTENT_TOP);

        // …and a document with a chip and three shelves above the grid answers the SAME way,
        // which the legacy constant could not: it is ~1.7k pixels short of this grid's origin.
        let deep = Layout::new(true, 3, 40, true);
        assert_eq!(deep.row_y(0, deep.grid_top()), CONTENT_TOP);
        assert!(
            deep.grid_top() - GRID_TOP > 1000.0,
            "the shelves really do move the grid's origin off the old constant"
        );
        // the legacy expression at the same scroll is far off the top of the panel — i.e. exactly
        // the `on_axis` rejection that made the focused card vanish
        let legacy = GRID_TOP - deep.grid_top();
        assert!(legacy < -CARD_H, "the old expression puts the focused row off-panel");

        // every row is one pitch below the last, in both documents
        assert_eq!(deep.row_y(1, 0.0) - deep.row_y(0, 0.0), PITCH);
        assert_eq!(flat.row_y(1, 0.0) - flat.row_y(0, 0.0), PITCH);
    }

    /// **The head chip and the grid's control row share one array and are TWO zones.** The
    /// toolbar's cursor may never rest on [`Chip::Source`]: the control row's draw skips it (it is
    /// drawn at the head of the document instead), so a `TOOL_F` of 0 with Source present is a
    /// toolbar with focus in it and nothing lit — which is exactly what the simulator showed
    /// before this, a page where DOWN off the last shelf reached Sort and Filter and neither
    /// responded.
    #[test]
    fn the_control_rows_cursor_never_rests_on_the_head_chip() {
        // two libraries: the head chip is present, so the row is [Source, Sort, Filter]
        assert_eq!(control_lo(&CHIPS_MANY), 1, "the first CONTROL is past the head chip");
        assert_eq!(control_n(&CHIPS_MANY), 2, "…and there are two of them, not three");
        assert_eq!(&CHIPS_MANY[control_lo(&CHIPS_MANY)], &Chip::Sort);

        // one library: no head chip in the array at all, so the row starts where it looks like it
        assert_eq!(control_lo(&CHIPS_ONE), 0);
        assert_eq!(control_n(&CHIPS_ONE), 2);

        // a failed listing keeps the chip that navigates AWAY from it and drops both controls —
        // `below_top_bar` and `sync_readout_focus` read this as an empty toolbar and send focus to
        // the read-out instead. `chips().len()` would have said 1 here and parked focus on a
        // control row that does not exist.
        assert_eq!(control_n(&CHIPS_SOURCE_ONLY), 0);
        assert_eq!(control_lo(&CHIPS_SOURCE_ONLY), 0, "…and degrades to 0 rather than panicking");
        assert_eq!(control_n(&CHIPS_NONE), 0);
    }

    /// The shelf cursor survives its shelf changing underneath — which on this endpoint is the
    /// ORDINARY refresh, not an edge case (`docs/pms-api.md` §3a: two hubs rotate their subject
    /// between requests), and which Mark Watched and Remove from Continue Watching also produce.
    #[test]
    fn the_shelf_cursor_follows_the_film_and_then_the_slot() {
        let _g = crate::testlock::serial();
        let _t = crate::plex::session::TempSession::new("shelf-focus");
        _t.watching("u-shelf-focus");
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();

        // no shelves at all: the rule is a no-op rather than a panic or a reset to 0
        unsafe {
            SHELF_F = 2;
            SHELF_C = 3;
        }
        shelf_focus_survives();
        assert_eq!(unsafe { (SHELF_F, SHELF_C) }, (2, 3), "nothing to re-resolve");
        crate::browse::reset();
    }

    // ---- the fade coordinator's precedence table ---------------------------------------------

    /// **A grid action queued during a page transition is carried to the TARGET, semantically.**
    ///
    /// The Library keeps its grid controls live while a page fades — a control that ignores a press
    /// is worse than one that acts a beat later — so a Sort chosen in library A really can commit
    /// against library B. That is only safe because the action names a stable server KEY and the
    /// section it is for: sort and genre menus are built per section from that section's own
    /// vectors, so an INDEX would select a different value there or silently no-op.
    #[test]
    fn a_grid_action_queued_during_a_page_fade_is_scoped_to_the_target() {
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        request_section(2);
        request_grid(GridAct::Sort {
            key: "addedAt".into(),
            desc: true,
        });
        let p = pending();
        let g = p.grid.clone().expect("a scoped grid action");
        assert_eq!(g.sec, 2, "the action belongs to the library we are arriving in");
        assert_eq!(
            g.act,
            GridAct::Sort {
                key: "addedAt".into(),
                desc: true
            },
            "…stated as a server key, not as a position in the OLD section's menu"
        );
        // **…and the SECTION SWITCH is still queued beside it.** A grid action arriving during a
        // page transition used to REPLACE it, so both were lost: the section was never applied, and
        // the action was then refused because its target was not the section still on screen.
        assert_eq!(
            p.section.map(|r| r.sec),
            Some(2),
            "the page transition survives the action queued against it"
        );
        clear();
    }

    /// …and the chip read-outs are scoped with it: what the user is looking at while the page fades
    /// is the library they are ARRIVING in, so an action queued against a different section must not
    /// relabel the one still on screen.
    #[test]
    fn a_queued_action_for_another_section_does_not_relabel_this_ones_chips() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let committed = crate::browse::unwatched();
        // queued against a section that is not the one on screen
        unsafe {
            PENDING = Pending {
                section: None,
                grid: Some(GridReq {
                    sec: crate::browse::cur() + 7,
                    epoch: crate::browse::table_epoch(),
                    act: GridAct::Unwatched(!committed),
                }),
            }
        };
        assert_eq!(
            view_unwatched(),
            committed,
            "the chip still describes the library it is drawn over"
        );
        clear();
    }

    /// **BACK before the floor CANCELS the transaction** — the queued action and the fade
    /// together. The rule is TEMPORAL, not spatial: before the commit nothing has happened and the
    /// user has changed their mind; after it the outgoing content is already gone, so cancelling
    /// would restore a page they have left, and BACK is the ordinary head-of-document move instead.
    #[test]
    fn back_before_the_floor_cancels_the_action_and_the_fade() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let menu0 = take_menu();
        set_menu(Menu::None);
        request_section(2);
        request_grid(GridAct::Sort {
            key: "addedAt".into(),
            desc: true,
        });
        assert!(xf().is_swapping(), "the fade is out and has not committed");

        assert!(back(), "BACK is consumed by the cancellation");
        assert!(pending().is_none(), "the queued action is discarded");
        clear();
        set_menu(menu0);
    }

    /// …and an OPEN MENU still takes BACK first, which is the existing precedence and is right: the
    /// unwatched switch deliberately leaves its Filter menu up after the press, so dismissing that
    /// menu is not the same gesture as withdrawing the switch. The action survives the menu.
    ///
    /// It is worth pinning because the two rules are one keypress apart and the wrong order would
    /// silently eat a press the user already saw acknowledged (`view_unwatched` flips on the press
    /// frame).
    #[test]
    fn an_open_menu_takes_back_first_and_the_queued_action_survives_it() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let menu0 = take_menu();
        let committed = crate::browse::unwatched();
        request_grid(GridAct::Unwatched(!committed));
        set_menu(Menu::Filter);

        assert!(back(), "the menu consumes it");
        assert!(!menu_open(), "…and closes");
        assert!(
            !pending().is_none(),
            "the switch the user already watched flip is still queued"
        );
        clear();
        set_menu(menu0);
    }

    /// An action whose target section is no longer the one on screen is REFUSED at the floor rather
    /// than applied to whatever now sits at that index — which is what `browse::reset` renumbering
    /// the table under a queued action would otherwise mean.
    #[test]
    fn an_action_whose_target_moved_is_refused_rather_than_misapplied() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        let before = crate::browse::unwatched();
        unsafe {
            PENDING = Pending {
                section: None,
                grid: Some(GridReq {
                    sec: crate::browse::cur() + 5,
                    epoch: crate::browse::table_epoch(),
                    act: GridAct::Unwatched(!before),
                }),
            }
        };
        apply_pending();
        assert_eq!(
            crate::browse::unwatched(),
            before,
            "a foreign target must change nothing here"
        );
        assert!(pending().is_none(), "…and the queue is drained either way");
        clear();
    }

    /// **A stale TABLE EPOCH is refused on its own**, even when the section index still matches.
    /// The guard read `epoch != … && sec != cur()`, which made the epoch half dead: `browse::reset`
    /// renumbers the table, and an action queued against index 0 before a profile switch names a
    /// DIFFERENT library at index 0 after it — the one case the index can never catch.
    #[test]
    fn a_queued_action_does_not_survive_the_table_being_renumbered() {
        let _s = crate::testlock::serial();
        let _g = PEND.lock().unwrap_or_else(|e| e.into_inner());
        let _t = crate::plex::session::TempSession::new("lib-epoch");
        _t.watching("u-lib-epoch");
        crate::browse::reset();
        // a REAL table, or the toggle below is a no-op and the assertion grades nothing — which is
        // exactly what the first version of this test did: it passed against the broken guard too.
        crate::browse::seed_two_source_table_for_test();
        clear();
        let before = crate::browse::unwatched();

        // the control leg FIRST: with a live epoch the same action really does land, so the
        // rejection below is the guard and not the seeding
        unsafe {
            PENDING = Pending {
                section: None,
                grid: Some(GridReq {
                    sec: crate::browse::cur(),
                    epoch: crate::browse::table_epoch(),
                    act: GridAct::Unwatched(!before),
                }),
            }
        };
        apply_pending();
        assert_eq!(
            crate::browse::unwatched(),
            !before,
            "control: a live action applies"
        );
        crate::browse::toggle_unwatched(); // back to where we started

        unsafe {
            PENDING = Pending {
                section: None,
                grid: Some(GridReq {
                    // the section on screen, so the INDEX guard cannot save us…
                    sec: crate::browse::cur(),
                    // …and an epoch from a table that no longer exists
                    epoch: crate::browse::table_epoch().wrapping_sub(1),
                    act: GridAct::Unwatched(!before),
                }),
            }
        };
        apply_pending();
        assert_eq!(
            crate::browse::unwatched(),
            before,
            "an action from a renumbered table must not be applied to whatever holds that index now"
        );
        clear();
        crate::browse::reset();
    }


    /// **The focused card's label must not enter the A–Z rail's band.** Photographed on the
    /// television 2026-09-05 and reported as *"does not respect the A–Z view"*: the last grid
    /// column's caption — `Wallace & Gromit: Vengeance Most Fowl` — ran straight through the rail's
    /// letters and off the panel's right edge.
    ///
    /// The grid itself always respected the band ([`GRID_R`] is derived from it, and
    /// `the_grid_fills_the_column_between_the_margin_and_the_rail` grades that). What did not was
    /// the LABEL, which is deliberately wider than the tile it sits under — `card_row`'s
    /// `under_budget` reserves two tiles and two gaps so a long title has somewhere to go — and was
    /// clamped only against the PANEL. On every other screen those two edges are the same line. On
    /// this one they are not, and this screen never said so.
    ///
    /// Graded through [`card_row::label_band`], the expression the draw itself uses, at both ends
    /// of the row: the last column must clear the rail, and the first must be unmoved, because a
    /// bound applied to the wrong edge would fix the photograph and shift every other label.
    #[test]
    fn the_focused_cards_label_stays_out_of_the_rails_band() {
        let p = crate::ui::Painter::root();
        let card = |c: usize| {
            Rect::new(
                MARGIN_X + c as f32 * (CARD_W + LGAP),
                GRID_TOP,
                CARD_W,
                CARD_H,
            )
        };

        let (x, w) = card_row::label_band(p, card(COLS - 1), &GRID_STYLE);
        assert!(
            x + w <= GRID_R + 0.01,
            "the last column's label runs to {} against a content edge of {GRID_R} — the rail's \
             band is {RAIL_BAND}px of it",
            x + w,
        );

        // …and the first column is untouched: the clamp binds on the right edge alone.
        let (x0, w0) = card_row::label_band(p, card(0), &GRID_STYLE);
        let (h0, _) = card_row::label_band(p, card(0), &RowStyle::HOME);
        assert_eq!(
            x0, h0,
            "a right-edge reserve must not move a left-edge label: {x0} vs {h0}",
        );
        assert!(x0 >= 0.0 && x0 + w0 <= GRID_R + 0.01);
    }

    /// **An open menu must not chase a chip the user cannot see.** Reported from the television
    /// 2026-09-05 as *"during filtering, menu explodes"*, with a photograph of the Filter panel's
    /// rows in one place and its glass body in another.
    ///
    /// The mechanism, from a `panel_rect` trace in the simulator: pressing *Unwatched only*
    /// re-queries the grid, the document re-seats, and the toolbar — which is INSIDE the scroll
    /// since this screen became one column — travels down the page. `panel_rect` re-read the chip
    /// every frame, so the panel sprang after it (529 → 671 → 808 over ~20 frames) while the table
    /// laid its rows out against the rect it had, and the two came apart mid-flight.
    ///
    /// **The reason it can never be right to follow is that the host page is FROZEN.** This
    /// popover is `caching_host`, so what is behind the scrim is a snapshot taken when the menu
    /// opened: the chip the viewer is looking at is at its OLD place, and the live one the anchor
    /// was tracking is not on the screen at all. A panel that follows the live chip walks away
    /// from the only chip anybody can see.
    ///
    /// So the anchor is latched at open. Graded by moving the chip out from under an open menu and
    /// asserting the panel does not move — and by checking the latch is dropped on close, or the
    /// next menu would open where the last one was.
    #[test]
    fn an_open_menus_anchor_does_not_follow_a_chip_that_scrolls_away() {
        let _g = crate::testlock::serial();
        unsafe {
            (*addr_of_mut!(TOOL_RECTS))[1] = Rect::new(MARGIN_X, 459.0, 300.0, TOOL_H);
            MENU = Menu::Filter;
        }
        let opened = panel_rect();
        assert!(
            (opened.y - (459.0 + TOOL_H + 18.0)).abs() < 0.01,
            "a menu opens under its chip: {} vs {}",
            opened.y,
            459.0 + TOOL_H + 18.0,
        );

        // …the grid re-queries under it and the toolbar travels down the document.
        unsafe { (*addr_of_mut!(TOOL_RECTS))[1] = Rect::new(MARGIN_X, 738.0, 300.0, TOOL_H) };
        assert_eq!(
            panel_rect().y,
            opened.y,
            "the panel followed the chip out from under the frozen page it is drawn on",
        );

        // …and the latch is released, or the NEXT menu opens where this one was.
        unsafe { MENU = Menu::None };
        close_menu();
        unsafe { MENU = Menu::Filter };
        assert!(
            (panel_rect().y - (738.0 + TOOL_H + 18.0)).abs() < 0.01,
            "a menu opened after the close must anchor to where the chip is NOW",
        );
        unsafe {
            MENU = Menu::None;
            (*addr_of_mut!(TOOL_RECTS))[1] = Rect::new(0.0, 0.0, 0.0, 0.0);
        }
    }

    /// **Every focused tile fills the two rungs the row reserves for it.** Reported from the
    /// television 2026-09-05: *"some rows have only one text label without detail label. We should
    /// either add something there or shrink row spacing."*
    ///
    /// It is the FIRST of those, and the reason is that the box is not this screen's to shrink:
    /// `consts::ROW_PITCH` reserves `card_row::UNDER_LABEL_H` — the two-rung block — for every
    /// shelf on Home and here, so a per-shelf shrink would make two rows of the same object
    /// different heights and leave this screen's grid disagreeing with the shelves above it. What
    /// was actually missing is that Home had carried a caption on every focused poster all along
    /// (`focused_caption`/`cw_caption`, private to that file) and the Library passed a bare title.
    ///
    /// The SHOW case is the one that needed more than a promotion, and it is why this test names
    /// three kinds: Home's rule gave the year to `kind == 0` alone, so a show — the whole of a TV
    /// library's *Recently Added* — captioned nothing on either screen. The grid on this very page
    /// has always captioned a show with its year, so widening the fallback makes one object read
    /// one way on one screen rather than inventing a rule for it.
    #[test]
    fn a_focused_poster_tile_always_fills_the_caption_rung_it_reserves() {
        let caption = |l: &card_row::TileLabel| {
            l.caption
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let poster = |m: PmsMovie, is_continue: bool| {
            let sh = crate::browse::section_hubs::Shelf {
                id: "x".into(),
                title: "Recently Added".into(),
                is_continue,
                landscape: false,
                items: vec![m],
            };
            caption(&shelf_label(&sh, 0))
        };

        // a SHOW — the case Home's own rule left blank, and the one in the report
        assert_eq!(
            poster(
                PmsMovie {
                    kind: 1,
                    title: "The Bear".into(),
                    year: 2022,
                    ..Default::default()
                },
                false
            ),
            "2022",
            "a show's poster reserved a caption rung and drew nothing into it",
        );
        // …a film, which Home already captioned
        assert_eq!(
            poster(
                PmsMovie {
                    kind: 0,
                    title: "Stardust".into(),
                    year: 2007,
                    ..Default::default()
                },
                false
            ),
            "2007",
        );
        // …and a DECK tile, which says what it is for instead: how much is left
        assert_eq!(
            poster(
                PmsMovie {
                    kind: 0,
                    title: "Stardust".into(),
                    year: 2007,
                    dur_ns: 60 * 60 * 1_000_000_000,
                    resume_ms: 35 * 60 * 1000,
                    ..Default::default()
                },
                true
            ),
            "25 min left",
        );
        // an item the server dated to nothing at all is still one rung, and that is honest
        assert_eq!(
            poster(
                PmsMovie {
                    kind: 1,
                    title: "Untitled".into(),
                    ..Default::default()
                },
                false
            ),
            "",
        );
    }
}
