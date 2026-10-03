//! ui/table.rs — a reusable, animated list/table widget (Apple-TV "settings" look).
//!
//! A single selectable list built from `Section`s of `Row`s. The focused row is drawn as a
//! light rounded "pill" that SLIDES between rows (a spring on its y), rows scroll with a spring
//! when they overflow the frame, and everything is drawn through a `Painter` so the owner can
//! fade/translate the whole thing in (the open animation). It knows nothing about playback or
//! metadata: the caller builds the sections, drives selection, and reads `sel` back — so the
//! same widget serves the in-player track menu and the Settings, Privacy and Legal surfaces.
#![allow(dead_code)]
use crate::ui::fit;
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::theme;
use crate::ui::{Painter, Rect, Spring};
use std::ffi::CString;

/// small trailing chip on a row (audio-description, forced, SDH, …)
pub enum Badge {
    Ad,
    Forced,
    Sdh,
    Cc,
    Text(String),
}
impl Badge {
    pub(crate) fn text(&self) -> &str {
        match self {
            Badge::Ad => plx_platform::i18n::msg::widgets_badge_ad(),
            Badge::Forced => plx_platform::i18n::msg::widgets_badge_forced(),
            Badge::Sdh => plx_platform::i18n::msg::widgets_badge_sdh(),
            Badge::Cc => plx_platform::i18n::msg::widgets_badge_cc(),
            Badge::Text(s) => s.as_str(),
        }
    }
}

/// Who wrote a text slot: the app's own catalog, or a server/user that can send anything (a machine
/// name, a plex.tv handle, a library someone else named). App text never elides at shipped sizes in
/// any shipped language; server text may overflow, so [`app_fit_failures`](TableView::app_fit_failures)
/// ignores it. Every slot defaults to `App`; a caller opts a slot into `Server`, so a forgotten
/// mark reports an overflow rather than hiding it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Origin {
    #[default]
    App,
    Server,
}

pub struct Row {
    pub label: String,
    pub label_origin: Origin,
    pub detail: String, // optional sub-line ("" = none)
    pub detail_origin: Origin,
    pub badges: Vec<Badge>,
    pub checked: bool, // shows the leading checkmark (the ACTIVE item)
    /// A **switch** rather than a choice: `Some(on)` states itself as the word `On`/`Off` at the
    /// row's TRAILING edge — never as a mark in the leading column. A mark says where you are, a
    /// word says what is set, and no row is allowed to say both; the on/off PAIR of marks this once
    /// drew (a ring, ticked when on) is gone from the design system, assets and all.
    pub toggle: Option<bool>,
    /// Any other trailing read-out, in the same slot and the same voice as [`Row::toggle`]'s
    /// `On`/`Off` — a row whose value is a word rather than a state ("English", "Title"). Wins over
    /// `toggle` if both are somehow set.
    pub value: Option<String>,
    pub value_origin: Origin,
    /// Quieten the read-out a further step (the value is context rather than the point).
    pub value_dim: bool,
    /// THE trailing accessory icon slot (an SVG asset, never a font glyph): the drill-in
    /// chevron (via the [`Row::chevron`] sugar) or e.g. the sort menu's direction chevron.
    pub ticon: Option<crate::ui::icons::Icon>,
    /// THE leading accessory icon — the SAME column the [`Row::checked`] checkmark occupies, for
    /// lists whose rows are ACTIONS rather than a picker's options (the item context menu's
    /// `[icon] [label]` rows). `checked` wins the slot when both are set: a picker's active mark is
    /// state, an action glyph is only decoration.
    pub licon: Option<crate::ui::icons::Icon>,
    pub dim: bool, // render dimmer (unavailable / de-emphasised)
    /// A **non-selectable hairline** that groups the rows above it from the rows below (the item
    /// menu's navigation-vs-state divider). It occupies a global row index like any other row, but
    /// [`TableView::move_sel`] steps OVER it, [`TableView::hit_row`] refuses it, and
    /// [`TableView::set_sections`] never lands the selection on one — so a caller can never focus
    /// a line that does nothing. (A second `Section` does the same job for a group boundary: every
    /// section after the first is preceded by a gap and a hairline, headed or not, so a
    /// `Row::separator` beside a section boundary would draw two rules.)
    pub sep: bool,
    /// The row's action is **destructive** — it ends or removes something (Sign out, Remove from
    /// Deck). Semantics only, never drawn: it exists so a menu never OPENS with its focus on one
    /// ([`TableView::opening_row`]). A stray OK on a freshly opened menu must be harmless; the row
    /// stays one press away, it is simply never where focus starts.
    pub destructive: bool,
    /// How many CAPTION lines a [`Row::note`] wraps to in the table's text column (`1` until
    /// [`TableView::fit_notes`] has measured it). A cache of a pure function of (text, width,
    /// measure), so it lives in a `Cell` and draw may refresh it through `&self`.
    pub(crate) note_lines: std::cell::Cell<u8>,
}
impl Row {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            label_origin: Origin::default(),
            detail: String::new(),
            detail_origin: Origin::default(),
            badges: Vec::new(),
            checked: false,
            toggle: None,
            value: None,
            value_origin: Origin::default(),
            value_dim: false,
            ticon: None,
            licon: None,
            dim: false,
            sep: false,
            destructive: false,
            note_lines: std::cell::Cell::new(1),
        }
    }
    /// A [`Row::note`]: `sep` with text (a bare `sep` is the hairline).
    pub(crate) fn is_note(&self) -> bool {
        self.sep && !self.label.is_empty()
    }
    /// The grouping hairline — a row that draws a rule and cannot be focused.
    pub fn separator() -> Self {
        Self {
            sep: true,
            ..Self::new("")
        }
    }
    /// **A non-selectable INFORMATIONAL line** — the same "cannot be focused" contract as
    /// [`Row::separator`] (skipped by every selection/hit walk that checks [`Self::sep`]), but it
    /// carries a label and draws quiet CAPTION text instead of a hairline. For a reason attached to
    /// the rows above it (e.g. "Not available with subtitles on."), left-aligned with their labels
    /// and WRAPPED to as many lines as the column needs ([`TableView::fit_notes`] sets the row's
    /// height) — never a hairline's own job of dividing two groups, and never a focusable row a
    /// stray OK could land on.
    pub fn note(text: impl Into<String>) -> Self {
        Self {
            sep: true,
            dim: true,
            ..Self::new(text)
        }
    }
    pub fn checked(mut self, v: bool) -> Self {
        self.checked = v;
        self
    }
    /// Mark [`Self::label`] as server/user text (a machine name, a library someone else named) —
    /// see [`Origin`].
    pub fn server_label(mut self) -> Self {
        self.label_origin = Origin::Server;
        self
    }
    /// Mark [`Self::detail`] as server/user text — see [`Row::server_label`].
    pub fn server_detail(mut self) -> Self {
        self.detail_origin = Origin::Server;
        self
    }
    /// Mark [`Self::value`] as server/user text — see [`Row::server_label`].
    pub fn server_value(mut self) -> Self {
        self.value_origin = Origin::Server;
        self
    }
    /// Mark this row a SWITCH at the given state — see [`Row::toggle`].
    pub fn toggle(mut self, on: bool) -> Self {
        self.toggle = Some(on);
        self
    }
    /// Give this row a trailing read-out — see [`Row::value`].
    pub fn value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }
    /// Quieten the read-out a step — see [`Row::value_dim`].
    pub fn value_dim(mut self, v: bool) -> Self {
        self.value_dim = v;
        self
    }
    /// The word this row's trailing slot shows, if any: an explicit [`Row::value`], else a switch's
    /// `On`/`Off`. ONE resolver, so the two can never both be drawn.
    fn readout(&self) -> Option<&str> {
        match (&self.value, self.toggle) {
            (Some(v), _) => Some(v.as_str()),
            (None, Some(on)) => Some(if on { plx_platform::i18n::msg::widgets_toggle_on() } else { plx_platform::i18n::msg::widgets_toggle_off() }),
            (None, None) => None,
        }
    }
    /// [`Origin`] of [`Self::readout`]: a switch's `On`/`Off` word is always app text.
    #[cfg(test)]
    fn readout_origin(&self) -> Origin {
        if self.value.is_some() { self.value_origin } else { Origin::App }
    }
    pub fn detail(mut self, d: impl Into<String>) -> Self {
        self.detail = d.into();
        self
    }
    pub fn badge(mut self, b: Badge) -> Self {
        self.badges.push(b);
        self
    }
    /// sugar: the "›" drill-in affordance is just the Chevron icon in the trailing slot
    pub fn chevron(mut self, v: bool) -> Self {
        if v {
            self.ticon = Some(crate::ui::icons::Icon::Chevron);
        }
        self
    }
    pub fn ticon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.ticon = Some(i);
        self
    }
    pub fn licon(mut self, i: crate::ui::icons::Icon) -> Self {
        self.licon = Some(i);
        self
    }
    pub fn dim(mut self, v: bool) -> Self {
        self.dim = v;
        self
    }
    /// Mark the row's action destructive — see [`Row::destructive`].
    pub fn destructive(mut self, v: bool) -> Self {
        self.destructive = v;
        self
    }
    /// `tall` is the TABLE's two-line measure — see [`TableView::tall_rows`]. A row cannot answer
    /// this alone: 92 and 98 are both correct, and which one applies is a property of the LIST.
    fn height_in(&self, tall: f32) -> f32 {
        if self.sep && self.label.is_empty() {
            SEP_H // the hairline
        } else if self.is_note() {
            // a `note`: its wrapped CAPTION lines plus air, never shorter than a plain row
            (f32::from(self.note_lines.get()) * NOTE_LEADING + 2.0 * NOTE_PAD).max(ROW_H)
        } else if self.detail.is_empty() {
            ROW_H // a plain row with no detail line
        } else {
            tall
        }
    }
}

pub struct Section {
    pub header: String, // "" = no header row
    pub header_origin: Origin,
    /// Right-aligned accessory on the HEADER line — a second fact about the group, one rung down
    /// and in the header's own dim ink ("Dolby Atmos"; the Sources list's owner handle beside a
    /// machine name). It is the last run on that line and is **elided** to the width
    /// [`TableView::header_columns`] resolves for it (its natural width capped at [`ACCESSORY_W`],
    /// and what the header leaves of the span), so a long plex.tv handle truncates on a character
    /// instead of colliding with the header or widening the panel — the rows below it are never
    /// touched.
    pub accessory: String,
    pub accessory_origin: Origin,
    /// The app-owned LEADING part of a [`Origin::Server`] accessory (the Sources list's
    /// `Not reachable ·` before a handle), which must fit its resolved column even though the
    /// whole run may not. Empty when the accessory is wholly one origin. Only the fit report reads it.
    pub accessory_app_prefix: String,
    /// Dim the WHOLE group — header, accessory and every row — at one alpha.
    ///
    /// Deliberately not [`Row::dim`] applied row by row: a row's dim is an ink role, and the state
    /// this expresses (an unreachable server, still granted and still pinned) is a fact about the
    /// GROUP, whose header would otherwise stay the brightest thing in the panel. One alpha over
    /// the lot is also what keeps a dimmed row's own read-out ("On") legible as itself: nothing was
    /// unpinned, so nothing may read as off.
    pub dim: bool,
    pub rows: Vec<Row>,
}
impl Section {
    pub fn new(header: impl Into<String>) -> Self {
        Self {
            header: header.into(),
            header_origin: Origin::default(),
            accessory: String::new(),
            accessory_origin: Origin::default(),
            accessory_app_prefix: String::new(),
            dim: false,
            rows: Vec::new(),
        }
    }
    pub fn accessory(mut self, a: impl Into<String>) -> Self {
        self.accessory = a.into();
        self
    }
    /// Mark [`Self::header`] as server/user text (a machine name) — see [`Origin`].
    pub fn server_header(mut self) -> Self {
        self.header_origin = Origin::Server;
        self
    }
    /// Mark [`Self::accessory`] as server/user text (a plex.tv handle) — see [`Origin`].
    pub fn server_accessory(mut self) -> Self {
        self.accessory_origin = Origin::Server;
        self
    }
    /// Declare the app-owned leading part of a [`Self::server_accessory`], text included in
    /// [`Self::accessory`] — see [`Section::accessory_app_prefix`].
    pub fn accessory_app_prefix(mut self, p: impl Into<String>) -> Self {
        self.accessory_app_prefix = p.into();
        self
    }
    /// Dim the whole group at [`GROUP_DIM_A`] — see [`Section::dim`].
    pub fn dim(mut self, v: bool) -> Self {
        self.dim = v;
        self
    }
    pub fn row(mut self, r: Row) -> Self {
        self.rows.push(r);
        self
    }
}

/// One row by GLOBAL index across `sections` — [`TableView::row_mut`]'s lookup for a list that has
/// not been handed to a [`TableView`] yet (`track_menu`'s style-lock pass edits its sections first).
pub(crate) fn row_mut_in(sections: &mut [Section], gi: usize) -> Option<&mut Row> {
    sections.iter_mut().flat_map(|s| s.rows.iter_mut()).nth(gi)
}

/// A [`Row::separator`]'s row height. The hairline sits on its centre line, so this IS the gap
/// between the two groups it divides — a gap between stacked blocks comes from a `space` rung, so
/// it is the rung, not a hand-tuned number.
const SEP_H: f32 = theme::space::MD;
/// The list's own vertical padding — air above the first row, air below the last.
/// [`TableView::update`] subtracts it from the frame height it is given, so a caller never derives
/// this number itself. It USED to be something every popover screen subtracted by hand
/// (`panel_h - 40.0`), which is exactly the footgun this constant's own doc used to warn about and
/// which half the callers fell into anyway — passing the raw frame height straight through, so the
/// scroll clamp thought the viewport was `PAD_V` taller than it is and stopped short of the true
/// bottom. Settings ▸ Privacy & data's *Delete all local data* row (last in its list) was the
/// reported case; `legal.rs`, `onboard.rs` and both of `consent.rs`'s tables had the identical bug.
pub const PAD_V: f32 = TOP_PAD + BOT_PAD;
/// A plain row (label only) — mockup rowBase padding 13 + 34px label.
///
/// `pub` for the same caller shape [`CONTENT_X`] is: a block that draws ROWS of its own on the
/// app's ground rather than mounting a [`TableView`] (`screens::search::render`'s `recents` — its rows are the
/// user's own words and have to stay editable in place). It re-derived this and the four constants
/// below from the mockup, so a row-height change here silently misaligned that block while both
/// modules' own tests stayed green.
pub const ROW_H: f32 = 60.0;
const ROW_H_TALL: f32 = 92.0; // a row that carries a detail sub-line (title HEADLINE + detail CAPTION)
/// **A CATALOG list's two-line row: 98, against a settings table's 92** — see
/// [`TableView::tall_rows`].
///
/// It arrived as the height of a row carrying a leading 54x81 POSTER, a slot this widget briefly
/// grew for `ui::filmography` and no longer has: the canvas retired the chip the next day ("a
/// poster at that size is a grey rectangle 222 times over") in favour of one large preview beside
/// the list. The chip went with it rather than being kept as a variant nobody draws — it is not an
/// answer waiting for its next caller, it is one that was measured and rejected. The taller ROW
/// survives it, because that was never about the poster: it is what a `BODY` title over a
/// `CAPTION` sub-line wants when the list IS the screen.
pub const ROW_H_ART: f32 = 98.0;
const ROW_SUB_GAP: f32 = 15.0; // title baseline → detail cap-top, in a two-line row
/// Panel header ("AUDIO"/"SUBTITLES", a server over its libraries). 40px in BOTH size classes and
/// whatever the header's own size — the band is fixed so a size change cannot reflow a panel.
/// Tight under the caps on purpose: the header belongs to the rows below it, so the band under its
/// caps ([`HDR_INK_PAD`]) is shorter than the air above them (half [`DIV_H`] + the cap inset).
/// `pub` for the [`ROW_H`] caller shape.
pub const HDR_H: f32 = 40.0;
/// Measure a [`Section::accessory`] is elided to. It is the LAST run on the header line, so it is
/// the one that gives way: a 34-character plex.tv handle truncates on a character and the library
/// names underneath keep their full width.
const ACCESSORY_W: f32 = 320.0;
/// The alpha a [`Section::dim`]med group is drawn at — the same weight an unreachable tab pill
/// takes, applied once over header and rows together.
const GROUP_DIM_A: f32 = 0.52;
/// A note's line pitch and its air above/below the block: one line is exactly [`ROW_H`].
const NOTE_LEADING: f32 = 32.0;
const NOTE_PAD: f32 = 14.0;
const DIV_H: f32 = 24.0; // gap + hairline between sections
/// Extra air between a page title band and the hairline under it (owner, 2026-10-01: "a bigger gap
/// under the title"): the title sits on its own, then the divider, then the rows.
const TITLE_GAP: f32 = 12.0;
/// The list's own air above its first row. `pub` because a panel that stacks something ABOVE the
/// list (the Sources panel's level band) has to subtract it to put the SEAM on the space scale —
/// otherwise the two paddings add and the gap lands between rungs.
pub const TOP_PAD: f32 = 20.0;
pub const BOT_PAD: f32 = 20.0;
/// Distance from a table frame's top edge to the cap-top of its first section label.
///
/// Route screens use this to align that label with the narrative title in the neighbouring
/// column.  Keeping the relationship here means a future table-padding retune cannot silently
/// break every two-column screen.
pub const FIRST_HEADER_CAP_OFFSET: f32 = TOP_PAD + HEADER_CAP_INSET;
const HEADER_CAP_INSET: f32 = 8.0;
/// Pill (row) inset from the panel's left/right. `pub` for the [`ROW_H`] caller shape.
pub const SIDE: f32 = 12.0;
const CONTENT_PAD: f32 = 20.0; // text/check padding inside the pill (mockup rowBase 13px 20px)
/// Where a row's own content starts, measured from the panel's left edge. Exposed so a panel that
/// draws chrome ABOVE the list (the Sources panel's level pills) can start it on the same line the
/// rows do, instead of re-deriving two private constants and drifting from them.
pub const CONTENT_X: f32 = SIDE + CONTENT_PAD;

/// Narrowest a popover menu panel gets (only to avoid a degenerate sliver around one short row).
pub const MENU_MIN_W: f32 = 300.0;
/// Widest any popover menu panel may be — the one shared cap ([`TableView::menu_panel_width`]).
/// Content wider than this ellipsizes on screen and FAILS the per-menu localization tests, so a
/// translation that needs more room is caught in `cargo test`, not on a customer's TV.
pub const MENU_MAX_W: f32 = 650.0;
const CHECK_W: f32 = 32.0; // leading check column
const GAP: f32 = 16.0; // check→label gap
/// Focused-row pill corner radius. `pub` for the [`ROW_H`] caller shape.
pub const PILL_RAD: f32 = 18.0;
/// Pill inset from the row's top/bottom. `pub` for the [`ROW_H`] caller shape.
pub const PILL_INSET: f32 = 3.0;
const PANEL_BG: [f32; 4] = theme::SURFACE_PANEL; // opaque panel colour — fade masks + badge knockout
/// Air between two chips of one right-aligned badge run (a subtitle row's `FORCED` + `SDH`).
const BADGE_GAP: f32 = 10.0;
const ACCESSORY_GAP: f32 = 14.0;
/// The air a HUGGED panel keeps between a row's label and its trailing read-out — the spacing
/// scale's label→value rung ([`theme::space::MD`], 24px). [`TableView::measured_width`] budgets it,
/// so at its own width every row shows at least this much space. It is the panel-sizing target,
/// not the layout's floor: a crowded row (a table wider content than its frame) may still squeeze
/// the pair down to [`ACCESSORY_GAP`] ([`TableView::row_columns_under`]) before the value elides.
const ROW_VALUE_GAP: f32 = theme::space::MD;
/// The empty band under a row's lowest ink, which every row kind leaves: a plain row's label is
/// centred in `ROW_H` with 13px under it, and a two-line row's centred pair leaves ~12.
const ROW_INK_PAD: f32 = 12.0;
/// A section header's ink ends at its CAPTION caps below `HEADER_CAP_INSET`.
const HDR_INK_PAD: f32 = HDR_H - HEADER_CAP_INSET - theme::size::CAPTION as f32;

/// The bottom-edge fade shared by rows and headers ([`TableView::bottom_edge_alpha`]): opaque
/// while `ink_bot` has `pad + BOT_PAD` of viewport below it, transparent once the edge reaches it.
fn edge_alpha(ink_bot: f32, vis_bot: f32, pad: f32) -> f32 {
    ((vis_bot - ink_bot) / (pad + BOT_PAD)).clamp(0.0, 1.0)
}

/// A row's two text columns as [`TableView::row_columns`] resolves them: the primary label's (and
/// its sub-line's) width, and the trailing value's, which the value is elided to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RowColumns {
    pub label_w: f32,
    pub value_w: f32,
}

/// Which text slot a [`FitIssue`] names.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FitRole {
    Label,
    Detail,
    Value,
    Header,
    Accessory,
    Note,
    /// The page title band ([`TableView::set_title`]).
    Title,
}

/// One text slot [`TableView::fit_report`] found would end in an ellipsis at its resolved column.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct FitIssue {
    pub role: FitRole,
    pub origin: Origin,
    pub text: String,
    pub natural: f32,
    pub budget: f32,
}
#[cfg(test)]
impl FitIssue {
    fn describe(&self) -> String {
        format!("{:?} {:?} is {:.0}px in a {:.0}px column", self.role, self.text, self.natural, self.budget)
    }
}

/// Slack, in px, when comparing a resolved column with a run's natural width: `fit::two_runs`
/// derives the secondary as `span - primary - gap`, which float rounding can leave a hair under the
/// width it was given.
#[cfg(test)]
const FIT_EPS: f32 = 0.01;

/// Fail a text-fit test with every finding [`TableView::app_fit_failures`] collected.
#[cfg(test)]
pub(crate) fn assert_no_fit_failures(out: &[String]) {
    assert!(out.is_empty(), "text the television would end in an ellipsis:\n  {}", out.join("\n  "));
}
const ACCESSORY_ICON_W: f32 = 26.0;

/// The horizontal space a row's trailing icon takes from the content edge: its INK width (per
/// [`crate::ui::icons::ink_x`]) plus [`ACCESSORY_GAP`]. The icon is drawn so its ink, not its
/// 26px box, ends on the content edge — the same edge a flush read-out ends on — so the eye sees
/// one right edge whatever the glyph's own side bearing.
fn ticon_slot_w(icon: crate::ui::icons::Icon) -> f32 {
    let (l, r) = crate::ui::icons::ink_x(icon);
    (r - l) * ACCESSORY_ICON_W + ACCESSORY_GAP
}

/// Where the trailing icon's box starts, as an offset from the content edge (<= 0): the box is
/// placed so the glyph's ink right edge lands ON the content edge.
fn ticon_box_dx(icon: crate::ui::icons::Icon) -> f32 {
    -crate::ui::icons::ink_x(icon).1 * ACCESSORY_ICON_W
}
/// The trailing read-out's WEIGHT: `size::LABEL` **bold**, which is what the `PlxNative Design
/// System`'s `TableView` authors it as (`var(--font-weight-bold) var(--size-label)`) and what its
/// prose says in words. The product drew it regular until 2026-08-21 — a rung below the row's
/// HEADLINE label in size, a step behind it in ink, AND a weight lighter, which is three
/// de-emphases stacked on the one run that answers the row's question. The read-out is not a
/// caption of the label; it is the value, and it has to hold its own beside the badge run next to
/// it. The step back is the INK's alone.
///
/// ONE constant, because the three calls at the draw site have to agree: the cap band the run is
/// centred on, the advance width it adds to `trailing`, and the paint. Bold glyphs are WIDER than
/// their regular twins, so measuring on one flag and painting on the other under-counts `trailing`
/// — and `trailing` is the LABEL's elision budget (`text_w` below), so the label would be elided
/// as though the read-out were narrower than it is and run right into it. The BADGE run is not at
/// risk: it is placed outside the read-out, while the complete trailing-width calculation
/// reserves both runs before measuring the label.
const VALUE_BOLD: std::os::raw::c_int = 1;

/// The next layout stamp: unique across tables, so a cache keyed on one cannot be fooled by two
/// tables that happen to have mutated the same number of times.
fn next_rev() -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct TableView {
    pub sections: Vec<Section>,
    pub sel: i32, // global index across all sections' rows (headers are not selectable)
    /// Does the LIST hold focus? `false` draws no selection pill at all — the "nothing selected"
    /// mode this widget did not have.
    ///
    /// It exists for a panel with a control OUTSIDE the list (the Sources panel's Browse / On Home
    /// segments): while focus is up there, a table that always paints its selection puts a second
    /// accent capsule on screen, and the two are indistinguishable. `sel` is REMEMBERED across the
    /// trip — you come back to the row you left, so this suppresses the pill rather than clearing
    /// the selection.
    ///
    /// Defaults to `true`, so every panel whose list is the only focusable thing is unchanged.
    pub list_focused: bool,
    /// compact size class: BODY regular row LABELS instead of the default HEADLINE bold (HEADLINE-bold
    /// rows overwhelmed the small account popover, a panel of one-word actions). Set it on every
    /// ACTION menu — the item context menu, account, more, and the library sort/filter/genre menus
    /// all do; only a PICKER of title+detail rows stays on the default.
    ///
    /// It no longer affects HEADERS: those are CAPS at CAPTION in both classes, because the caps
    /// are what make a header a label and a size that varied could tie with its own rows.
    pub compact: bool,
    /// see [`TableView::tall_rows`]
    tall: bool,
    /// Semantic ink for section labels. Ambient routes can raise this role for contrast without
    /// replacing the shared table header renderer or changing row/detail hierarchy.
    pub header_ink: [f32; 4],
    /// The floor [`Self::menu_panel_width`] clamps to; [`MENU_MIN_W`] unless a panel family sets a
    /// wider one (the in-player popovers: [`theme::layout::PLAYER_MENU_MIN_W`]).
    pub min_panel_w: f32,
    // the highlight pill's top and bottom edges spring INDEPENDENTLY (content coords), so moving
    // to a taller/shorter row morphs the pill smoothly instead of snapping its height.
    hl_top: Spring,
    hl_bot: Spring,
    scroll: Spring,
    /// A drill-in page's title band, see [`TableView::set_title`].
    title: Option<String>,
    /// Re-stamped by every mutation that can change the table's measured size (new sections, a new
    /// title, a row edited in place), so a panel that caches its layout target can tell whether
    /// the table it measured is still the table it holds ([`Self::layout_rev`]). The stamp comes
    /// from one process-wide counter ([`next_rev`]), not a per-table one: a panel swaps whole
    /// tables in and out ([`Self::blank_like`]), and two tables each on their second mutation must
    /// not look like the same layout.
    rev: u32,
}
/// [`TableView::walk`]'s event index for the hairline that divides a section from the one above
/// it (`-1` is a header, `>= 0` a row). Emitted for EVERY section after the first, headed or not.
const WALK_DIVIDER: i32 = -2;
/// [`TableView::walk`]'s event index for the page title band ([`TableView::set_title`]); it is
/// followed by a [`WALK_DIVIDER`] event, so the title reads as the page's own heading.
const WALK_TITLE: i32 = -3;
/// The glyph that opens a page title: the drill-in's "back" mark, the mirror of a row's chevron.
const TITLE_BACK_GLYPH: &str = "\u{2039}";
impl TableView {
    pub(crate) const MOTION_SHAPE: &'static str = "TableViewMotion{sel:i32,list_focused:bool,compact:bool,tall:bool,header_ink:[f32;4],hl_top:Spring{pos:f32,vel:f32},hl_bot:Spring{pos:f32,vel:f32},scroll:Spring{pos:f32,vel:f32}}";

    /// The owner records its row data separately. These fields determine layout, the selected
    /// face and subsequent motion; no text/texture cache or renderer pointer is traversed.
    pub(crate) fn write_motion(&self, c: &mut plx_machine::machine::Canon) {
        let Self { sections: _, sel, list_focused, compact, tall, header_ink, hl_top, hl_bot, scroll, title: _, min_panel_w: _, rev: _ } = self;
        c.u32(*sel as u32).bool(*list_focused).bool(*compact).bool(*tall);
        for component in header_ink { c.f32(*component); }
        for spring in [hl_top, hl_bot, scroll] { c.f32(spring.pos).f32(spring.vel); }
    }

    /// **Two-line rows at the CATALOG measure (98) rather than the settings one (92).**
    ///
    /// Opt-in per table, because both are right: a settings row is a line of chrome in a panel, and
    /// a row in a list that IS the screen — `ui::filmography`'s credits — carries a `BODY` title
    /// over a `CAPTION` sub-line and wants the air. The canvas states 98 for that list and 92 is
    /// what every other table here has always drawn.
    pub fn tall_rows(&mut self, v: bool) {
        self.tall = v;
    }
    fn tall_row_h(&self) -> f32 {
        if self.tall {
            ROW_H_ART
        } else {
            ROW_H_TALL
        }
    }
    pub const fn new() -> Self {
        Self {
            sections: Vec::new(),
            sel: 0,
            list_focused: true,
            compact: false,
            tall: false,
            header_ink: theme::TEXT_TERTIARY,
            min_panel_w: MENU_MIN_W,
            hl_top: Spring::at(0.0),
            hl_bot: Spring::at(0.0),
            scroll: Spring::at(0.0),
            title: None,
            rev: 0,
        }
    }

    /// This table drawn WITHOUT the focus pill (and without the focused-row ink flip): the page
    /// that is leaving a transition, where only the arriving page owns the selection.
    pub(crate) fn unfocused(mut self) -> Self {
        self.list_focused = false;
        self
    }

    /// **An empty table dressed like this one**: the same size class, header ink, row height and
    /// panel-width floor, with no rows, no title and every spring at rest. A panel that moves its
    /// outgoing page aside for a transition (`ui::panel_motion`) swaps this in for the page it
    /// builds next, so the next page needs no re-configuration and the old one is kept whole, to
    /// draw once more, without a `Clone` of every row.
    pub(crate) fn blank_like(&self) -> Self {
        let mut t = Self::new();
        t.list_focused = self.list_focused;
        t.compact = self.compact;
        t.tall = self.tall;
        t.header_ink = self.header_ink;
        t.min_panel_w = self.min_panel_w;
        t
    }

    /// A counter that moves whenever the table's measured size could have: see the field.
    pub(crate) fn layout_rev(&self) -> u32 {
        self.rev
    }

    /// The row under the pointer in a `frame`-anchored draw (screen coords), or None — popover
    /// click support (hover→focus, click→commit) shares the draw's own layout walk.
    /// **Where row `i` is on screen** — the exact inverse of [`Self::hit_row`], walked the same
    /// way so the two can never disagree about a row's band.
    ///
    /// It exists for `ui::popover::Opener`: a context menu anchors beside the element it was opened
    /// from, and a caller that measured that band itself would be a second layout of this widget.
    /// Answers `None` for a header, a hairline, or a row scrolled out of the frame — all three are
    /// cases where there is nothing on screen to anchor to.
    pub fn row_rect(&self, frame: Rect, i: i32) -> Option<Rect> {
        self.row_frame(frame, i)
            .filter(|r| r.y + r.h > frame.y && r.y < frame.y + frame.h)
    }

    /// Row `i`'s frame under the live scroll WHETHER OR NOT it is inside the viewport — the one
    /// walk [`row_rect`](Self::row_rect), [`hit_row`](Self::hit_row) and `ui::geom::Table::place`
    /// share (spec §7.1). `None` for a header index, a separator, or an index out of range.
    pub fn row_frame(&self, frame: Rect, i: i32) -> Option<Rect> {
        let top0 = frame.y + TOP_PAD;
        let scroll = self.scroll.pos;
        let mut out = None;
        self.walk(|cy, gi, _| {
            if gi != i || gi < 0 || self.rows_at(gi).sep {
                return;
            }
            let sy = top0 + cy - scroll;
            let h = self.rows_at(gi).height_in(self.tall_row_h());
            out = Some(Rect::new(frame.x + SIDE, sy, frame.w - 2.0 * SIDE, h));
        });
        out
    }

    /// The next SELECTABLE row from `i` in direction `delta` (−1/+1; 0 answers `i` itself if
    /// selectable), stepping over separators as `move_sel` does; `None` at the ends.
    pub fn next_selectable(&self, i: i32, delta: i32) -> Option<i32> {
        let n = self.n_rows();
        if n == 0 || i < 0 || i >= n {
            return None;
        }
        if delta == 0 {
            return (!self.rows_at(i).sep).then_some(i);
        }
        let mut j = i + delta;
        while j >= 0 && j < n {
            if !self.rows_at(j).sep {
                return Some(j);
            }
            j += delta;
        }
        None
    }

    /// The nearest selectable row to `i` (what `set_sections` and `move_sel` settle on).
    pub fn settle(&self, i: i32) -> i32 {
        self.settle_sel(i)
    }
    pub fn hit_row(&self, frame: Rect, mx: f32, my: f32) -> Option<i32> {
        if !frame.contains(mx, my) {
            return None;
        }
        let top0 = frame.y + TOP_PAD;
        let scroll = self.scroll.pos;
        let mut hit = None;
        self.walk(|cy, gi, _| {
            if gi < 0 || self.rows_at(gi).sep {
                return; // headers and grouping hairlines are not click targets
            }
            let sy = top0 + cy - scroll;
            if my >= sy && my <= sy + self.rows_at(gi).height_in(self.tall_row_h()) {
                hit = Some(gi);
            }
        });
        hit
    }

    /// **A drill-in page's title band**, drawn as "‹ TITLE" in the section-header caps caption at
    /// the very top of the content and followed by the same boundary gap a section divider has
    /// ([`DIV_H`] around a hairline), so it reads as the page's own heading rather than as a
    /// section caption glued to the first row. `None` (the default) draws nothing. The band is
    /// part of [`Self::measured_height`], [`Self::measured_width`] and [`Self::fit_report`]; it is
    /// not a hit target here (the owner that pops on it registers its own stop). Design record:
    /// `docs/player-submenus.md`.
    ///
    /// The band moves every row down (or up) by [`HDR_H`] + [`TITLE_GAP`] + [`DIV_H`], so when it appears or goes
    /// the pill springs are re-jumped to the selected row's new place: installing the title after
    /// the sections must not leave the pill a band off until the spring catches up. The scroll is
    /// left alone (a [`Self::restore_sections`] puts its own back after).
    pub fn set_title(&mut self, title: Option<String>) {
        self.rev = next_rev();
        let moved = self.title.is_some() != title.is_some();
        self.title = title;
        if moved && self.n_rows() > 0 {
            let top = self.row_top(self.sel);
            self.hl_top.jump(top + PILL_INSET);
            self.hl_bot.jump(top + self.row_height(self.sel) - PILL_INSET);
        }
    }

    /// The title band's rect in screen space for a table drawn into `frame`: under the current
    /// scroll, exactly where [`Self::draw`] puts it, and clipped to `frame` as the draw is. The
    /// strip spans the pill's width ([`SIDE`] inset) across [`HDR_H`], for a pointer-only "back"
    /// target. `None` without a title, or once it has scrolled fully out of the viewport. (A
    /// section-0 dim never touches it: the title is the page's, not the section's.)
    pub fn title_rect(&self, frame: Rect) -> Option<Rect> {
        self.title.as_ref()?;
        let band = Rect::new(frame.x + SIDE, frame.y + TOP_PAD - self.scroll.pos, frame.w - 2.0 * SIDE, HDR_H);
        let seen = band.intersect(frame);
        (seen.w > 0.0 && seen.h > 0.0).then_some(seen)
    }

    /// The title set by [`Self::set_title`].
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The title band's own run, upper-cased, and the x offset it starts at from the content's
    /// left edge: past the back glyph and the check-column→label [`GAP`].
    fn title_run(&self, measure: &dyn plx_machine::machine::Measure) -> Option<(String, f32)> {
        let title = self.title.as_ref()?;
        let glyph = measure.width_str(TITLE_BACK_GLYPH, theme::size::CAPTION, false);
        Some((title.to_uppercase(), glyph + GAP))
    }

    /// Replace the contents and reinstate a SAVED view: the selection (`None`: the
    /// [`Self::opening_row`]) snaps with no pill slide and the scroll is put back at `scroll` (read
    /// it with [`Self::scroll_pos`]; clamped by the next [`Self::update`]). The pop half of a
    /// drill-in: the page below comes back exactly where it was left.
    pub fn restore_sections(&mut self, sections: Vec<Section>, sel: Option<i32>, scroll: f32) {
        self.set_sections_or_open(sections, sel, false);
        self.scroll.jump(scroll);
    }

    /// [`Self::set_sections`] with an optional selection: `None` lands on [`Self::opening_row`]
    /// of the NEW sections. Unlike [`Self::open_sections`] it takes `slide`, and with `slide ==
    /// true` it keeps the scroll, which is what a same-page refresh wants.
    pub fn set_sections_or_open(&mut self, sections: Vec<Section>, sel: Option<i32>, slide: bool) {
        self.sections = sections;
        let sel = sel.unwrap_or_else(|| self.opening_row());
        let sections = std::mem::take(&mut self.sections);
        self.set_sections(sections, sel, slide);
    }

    /// replace the contents and re-anchor selection. `slide=false` snaps the pill to the new
    /// selection (use when the whole list changed, e.g. Audio↔Subtitles); `true` lets it glide.
    pub fn set_sections(&mut self, sections: Vec<Section>, sel: i32, slide: bool) {
        self.rev = next_rev();
        self.sections = sections;
        let n = self.n_rows();
        self.sel = if n == 0 { 0 } else { self.settle_sel(sel) };
        if !slide {
            let top = self.row_top(self.sel);
            self.hl_top.jump(top + PILL_INSET);
            self.hl_bot
                .jump(top + self.row_height(self.sel) - PILL_INSET);
            self.scroll.jump(0.0);
        }
    }

    /// **Open** a menu on `sections`: [`Self::set_sections`] with the selection on
    /// [`Self::opening_row`] and the pill snapped there. Every menu that has no prior selection to
    /// restore opens through this, so none can open with its focus on a destructive action.
    pub fn open_sections(&mut self, sections: Vec<Section>) {
        self.sections = sections;
        let sel = self.opening_row();
        let sections = std::mem::take(&mut self.sections);
        self.set_sections(sections, sel, false);
    }

    /// Where a menu's focus STARTS: the first selectable row that is not [`Row::destructive`]; the
    /// first selectable row when every row is destructive (the menu still has to focus something,
    /// and then the one action on offer is the one asked for); `0` for an empty table.
    pub fn opening_row(&self) -> i32 {
        let n = self.n_rows();
        let selectable = || (0..n).filter(|&i| !self.rows_at(i).sep);
        selectable()
            .find(|&i| !self.rows_at(i).destructive)
            .or_else(|| selectable().next())
            .unwrap_or(0)
    }

    pub fn n_rows(&self) -> i32 {
        self.sections.iter().map(|s| s.rows.len()).sum::<usize>() as i32
    }

    /// The LAST **selectable** row's global index, or `None` for a table with none.
    ///
    /// Not `n_rows() - 1`: [`Row::separator`] rows occupy an index but cannot be landed on, so on
    /// a list that ends in one the last index is a row the selection can never reach. The route
    /// family's "DOWN off the last row enters the action band" rule
    /// ([`crate::ui::route_screen`]'s rule 2) is graded against this, so the band stays reachable
    /// whatever the list ends with.
    pub fn last_row(&self) -> Option<i32> {
        (0..self.n_rows()).rev().find(|&i| !self.rows_at(i).sep)
    }

    /// Is the selection on the last selectable row? — rule 2's own predicate.
    pub fn at_last_row(&self) -> bool {
        self.last_row() == Some(self.sel)
    }

    /// Does the row at `i` OPEN something — i.e. does it wear the drill-in chevron?
    ///
    /// The route family's rule 8 (RIGHT enters nested content) is a statement about the chevron a
    /// row already draws, so it is read off that rather than kept as a second per-screen list that
    /// can drift from what is painted. A toggle row, which changes a value in place, answers
    /// `false`, and RIGHT does nothing on it.
    pub fn row_opens(&self, i: i32) -> bool {
        i >= 0
            && i < self.n_rows()
            && matches!(self.rows_at(i).ticon, Some(crate::ui::icons::Icon::Chevron))
    }

    /// the full drawn height of the content (headers + rows + top/bottom padding) — the owner
    /// sizes its panel to this (clamped) so the panel hugs the list, tvOS-style.
    pub fn measured_height(&self) -> f32 {
        if self.n_rows() == 0 {
            return 120.0;
        }
        self.content_h() + TOP_PAD + BOT_PAD
    }

    /// Intrinsic panel width: the width at which EVERY header, label, sub-line and trailing value
    /// resolves to its full natural width under [`Self::row_columns_under`] /
    /// [`Self::header_columns`] with the device's [`fit::HEADROOM`] to spare. It is the INVERSE of
    /// those two layouts and is built from the same pieces (`row_runs`, [`Self::trailing_width`],
    /// [`ROW_VALUE_GAP`], [`ACCESSORY_GAP`], the hug margin), so a panel of exactly this width can
    /// never elide a run the layout would have kept. Action menus size their panel with it.
    pub(crate) fn measured_width(&self, measure: &dyn plx_machine::machine::Measure) -> f32 {
        let h = fit::HEADROOM;
        let mut width: f32 = 0.0;
        if let Some((title, x0)) = self.title_run(measure) {
            width = 2.0 * CONTENT_X + x0 + measure.width_str(&title, theme::size::CAPTION, false) / h;
        }
        for section in &self.sections {
            if !section.header.is_empty() {
                let header = measure.width_str(&section.header.to_uppercase(), theme::size::CAPTION, false);
                let band = if section.accessory.is_empty() {
                    header / h
                } else {
                    let accessory = measure
                        .width_str(&section.accessory, theme::size::MICRO, false)
                        .min(ACCESSORY_W)
                        .max(Self::accessory_floor(section, measure));
                    (header + accessory) / h + ACCESSORY_GAP
                };
                width = width.max(2.0 * CONTENT_X + band);
            }
            for row in section.rows.iter().filter(|row| !row.sep) {
                let (label, value) = self.row_runs(row, measure);
                let detail = measure.width_str(&row.detail, theme::size::CAPTION, false);
                let primary = (label / h).max(detail / h);
                let text = match value {
                    Some(value) => primary + ROW_VALUE_GAP + value / h,
                    None => primary,
                };
                let fixed = self.trailing_width(row, measure) - Self::value_slot(row, measure);
                width = width.max(2.0 * CONTENT_X + CHECK_W + GAP + fixed + text);
            }
        }
        width.ceil()
    }

    /// The shared popover-menu width rule: the table's own [`Self::measured_width`] (longest
    /// header / label / detail / trailing value plus `2 * CONTENT_X` of padding and the check
    /// column) clamped to [[`Self::min_panel_w`] (default [`MENU_MIN_W`]), [`MENU_MAX_W`]]. Every TableView-in-popover menu sizes
    /// its panel with this; none carries a width constant of its own.
    ///
    /// [`MENU_MAX_W`] is a localization CATCH, not a silent ellipsis: each menu's fit test grades
    /// its shipped languages at the cap, and [`Self::menu_cap_failure`] flags any draft whose
    /// `measured_width` exceeds it.
    pub(crate) fn menu_panel_width(&self, measure: &dyn plx_machine::machine::Measure) -> f32 {
        self.measured_width(measure).clamp(self.min_panel_w.min(MENU_MAX_W), MENU_MAX_W)
    }

    /// Test-side half of the cap: `Err` names the offending width when this table would need a
    /// panel wider than [`MENU_MAX_W`] (`what` = language tag + menu/row context).
    #[cfg(test)]
    pub(crate) fn menu_cap_failure(&self, measure: &dyn plx_machine::machine::Measure, what: &str) -> Option<String> {
        let w = self.measured_width(measure);
        (w > MENU_MAX_W).then(|| format!("{what}: measured_width {w} > MENU_MAX_W {MENU_MAX_W}"))
    }

    /// Every text slot (label, detail sub-line, trailing value, section header, section accessory)
    /// that a `frame_w`-wide table would end in an ellipsis, with `headroom` of its resolved column
    /// to spare, each tagged with its [`Origin`]. Measure with the device's own advances
    /// (`fontcov::advances::ShippedMeasure`).
    ///
    /// Label and Detail are checked against their column shrunk by `headroom`. Value, Header and
    /// Accessory are the secondary run of a `two_runs` pair, which resolves a fitting secondary to
    /// exactly its natural width, so shrinking the column would fail every fitting run. Those roles
    /// instead re-resolve the pair with EVERY run's natural width divided by `headroom` (glyphs
    /// running wider than the measure predicts, the primary still claiming its share first) and
    /// report the secondary only if its own inflated width no longer fits.
    #[cfg(test)]
    pub(crate) fn fit_report(&self, frame_w: f32, measure: &dyn plx_machine::machine::Measure, headroom: f32) -> Vec<FitIssue> {
        let (size, bold) = self.label_style();
        let mut out = Vec::new();
        self.fit_notes(frame_w, measure);
        if let Some((title, x0)) = self.title_run(measure) {
            let natural = measure.width_str(&title, theme::size::CAPTION, false);
            let budget = (frame_w - 2.0 * CONTENT_X - x0).max(0.0);
            if budget + FIT_EPS < natural / headroom {
                out.push(FitIssue { role: FitRole::Title, origin: Origin::App, text: title, natural, budget });
            }
        }
        for section in &self.sections {
            if !section.header.is_empty() {
                let header_text = section.header.to_uppercase();
                let header_nat = measure.width_str(&header_text, theme::size::CAPTION, false);
                let (header_w, accessory_w) = Self::header_columns(section, frame_w, measure, headroom);
                if header_w + FIT_EPS < header_nat / headroom {
                    out.push(FitIssue { role: FitRole::Header, origin: section.header_origin, text: header_text, natural: header_nat, budget: header_w });
                }
                if !section.accessory.is_empty() {
                    let accessory_nat = measure.width_str(&section.accessory, theme::size::MICRO, false).min(ACCESSORY_W);
                    if accessory_w + FIT_EPS < accessory_nat / headroom {
                        out.push(FitIssue { role: FitRole::Accessory, origin: section.accessory_origin, text: section.accessory.clone(), natural: accessory_nat, budget: accessory_w });
                    }
                    if !section.accessory_app_prefix.is_empty() {
                        let prefix_nat = measure.width_str(&section.accessory_app_prefix, theme::size::MICRO, false);
                        if accessory_w + FIT_EPS < prefix_nat / headroom {
                            out.push(FitIssue { role: FitRole::Accessory, origin: Origin::App, text: section.accessory_app_prefix.clone(), natural: prefix_nat, budget: accessory_w });
                        }
                    }
                }
            }
            for row in section.rows.iter().filter(|r| r.is_note()) {
                // a note WRAPS, so it fits unless one unbreakable word is wider than its column
                let budget = Self::note_column_w(frame_w) * headroom;
                let widest = row
                    .label
                    .split(|c: char| c.is_whitespace() && c != '\u{a0}')
                    .filter(|w| !w.is_empty())
                    .map(|w| (measure.width_str(w, theme::size::CAPTION, false), w))
                    .fold((0.0f32, ""), |a, b| if b.0 > a.0 { b } else { a });
                if widest.0 > budget {
                    out.push(FitIssue { role: FitRole::Note, origin: row.label_origin, text: widest.1.to_string(), natural: widest.0, budget });
                }
            }
            for row in section.rows.iter().filter(|r| !r.sep) {
                let label_budget = self.row_columns(row, frame_w, measure).label_w * headroom;
                let lw = measure.width_str(&row.label, size, bold);
                if lw > label_budget {
                    out.push(FitIssue { role: FitRole::Label, origin: row.label_origin, text: row.label.clone(), natural: lw, budget: label_budget });
                }
                let dw = measure.width_str(&row.detail, theme::size::CAPTION, false);
                if dw > label_budget {
                    out.push(FitIssue { role: FitRole::Detail, origin: row.detail_origin, text: row.detail.clone(), natural: dw, budget: label_budget });
                }
                if let Some(value) = row.readout() {
                    let value_nat = measure.width_str(value, theme::size::LABEL, VALUE_BOLD != 0);
                    let value_w = self.row_columns_under(row, frame_w, measure, headroom).value_w;
                    if value_w + FIT_EPS < value_nat / headroom {
                        out.push(FitIssue { role: FitRole::Value, origin: row.readout_origin(), text: value.to_string(), natural: value_nat, budget: value_w });
                    }
                }
            }
        }
        out
    }

    /// [`Self::fit_report`] at the device's advances and headroom, reduced to the [`Origin::App`]
    /// findings as `"{tag}: {finding}"` lines. A server-owned overflow is not this app's text to
    /// fix; a caller that wants it too reads `fit_report` directly.
    #[cfg(test)]
    pub(crate) fn app_fit_failures(&self, frame_w: f32, tag: &str) -> Vec<String> {
        use plx_base::fontcov::advances::ShippedMeasure;
        use crate::ui::fit::HEADROOM;
        self.fit_report(frame_w, &ShippedMeasure, HEADROOM)
            .iter()
            .filter(|i| i.origin == Origin::App)
            .map(|i| format!("{tag}: {}", i.describe()))
            .collect()
    }

    /// [`Self::app_fit_failures`] graded at the width this table's popover ACTUALLY gets
    /// ([`Self::menu_panel_width`] at the device's advances), not at the [`MENU_MAX_W`] ceiling: a
    /// panel that hugs its content must not elide any of it.
    #[cfg(test)]
    pub(crate) fn app_fit_failures_hugged(&self, tag: &str) -> Vec<String> {
        let w = self.menu_panel_width(&plx_base::fontcov::advances::ShippedMeasure);
        self.app_fit_failures(w, &format!("{tag} @hugged {w}"))
    }

    /// Where a row's read-out ends (its right edge) in a `frame_w`-wide table — the x offset from
    /// the panel's left, the same `text_right - trailing` the draw uses.
    #[cfg(test)]
    fn value_right_edge(&self, row: &Row, frame_w: f32) -> f32 {
        let mut trailing = row.ticon.map_or(0.0, ticon_slot_w);
        if !row.badges.is_empty() {
            trailing += row.badges.iter().map(|b| crate::ui::widgets::badge_w(b.text(), None, &plx_base::fontcov::advances::ShippedMeasure)).sum::<f32>()
                + BADGE_GAP * (row.badges.len() - 1) as f32 + ACCESSORY_GAP;
        }
        frame_w - SIDE - CONTENT_PAD - trailing
    }

    /// The text column a [`Row::note`] wraps in: the ROW-LABEL column (past the check column), so
    /// the note reads as attached to the rows above it rather than as a second header.
    fn note_column_w(frame_w: f32) -> f32 {
        (frame_w - 2.0 * CONTENT_X - CHECK_W - GAP).max(0.0)
    }

    /// **Measure every [`Row::note`] against a `frame_w`-wide table**, storing the wrapped line
    /// count [`Row::height_in`] reads back. `Row::height_in` has no measure, so this is the one
    /// place a note's height is decided; [`Self::draw`] runs it before it walks, and an owner that
    /// sizes its panel from [`Self::measured_height`] (the track menu) runs it first. Idempotent.
    pub(crate) fn fit_notes(&self, frame_w: f32, measure: &dyn plx_machine::machine::Measure) {
        let w = Self::note_column_w(frame_w);
        for row in self.sections.iter().flat_map(|s| s.rows.iter()).filter(|r| r.is_note()) {
            let lines = crate::ui::text_view::TextView::new(&row.label, theme::size::CAPTION, theme::TEXT_TERTIARY)
                .with_measure(measure)
                .line_count(w);
            row.note_lines.set(lines.clamp(1, u8::MAX as usize) as u8);
        }
    }

    fn label_style(&self) -> (std::os::raw::c_int, bool) {
        if self.compact { (theme::size::BODY, false) } else { (theme::size::HEADLINE, true) }
    }

    fn trailing_width(&self, row: &Row, measure: &dyn plx_machine::machine::Measure) -> f32 {
        let mut width = row.ticon.map_or(0.0, ticon_slot_w);
        if !row.badges.is_empty() {
            width += row.badges.iter().map(|badge| crate::ui::widgets::badge_w(badge.text(), None, measure)).sum::<f32>()
                + BADGE_GAP * (row.badges.len() - 1) as f32 + ACCESSORY_GAP;
        }
        if let Some(value) = row.readout() {
            width += measure.width_str(value, theme::size::LABEL, VALUE_BOLD != 0) + ACCESSORY_GAP;
        }
        width
    }

    /// The same label budget used by rendering and intrinsic-width checks.
    pub(crate) fn label_width(&self, row: &Row, frame_w: f32, measure: &dyn plx_machine::machine::Measure) -> f32 {
        self.row_columns(row, frame_w, measure).label_w
    }

    /// A row's label and trailing-value columns, resolved by priority — content hugging and
    /// compression resistance, measured rather than assumed from English lengths.
    ///
    /// When both runs fit, the value takes its natural width and the label (with its sub-line)
    /// every pixel left. When they do not, the LABEL is the primary read: it keeps its natural
    /// width up to [`fit::ROW_PRIMARY_SHARE`] of the row's text span and the value gives way first,
    /// elided to what is left (see [`fit::two_runs`]).
    pub(crate) fn row_columns(&self, row: &Row, frame_w: f32, measure: &dyn plx_machine::machine::Measure) -> RowColumns {
        self.row_columns_under(row, frame_w, measure, 1.0)
    }

    /// [`Self::row_columns`] with both natural widths divided by `headroom` — `1.0` renders, and
    /// [`Self::fit_report`] passes the device's headroom to model glyphs wider than the measure.
    /// The label is hugged with [`fit::HUG_MARGIN`] before dividing.
    fn row_columns_under(&self, row: &Row, frame_w: f32, measure: &dyn plx_machine::machine::Measure, headroom: f32) -> RowColumns {
        let fixed = self.trailing_width(row, measure) - Self::value_slot(row, measure);
        let span = (frame_w - 2.0 * CONTENT_X - CHECK_W - GAP - fixed).max(0.0);
        let (label, value) = self.row_runs(row, measure);
        let Some(value) = value else {
            return RowColumns { label_w: span, value_w: 0.0 };
        };
        let (label_w, value_w) = fit::two_runs(span, ACCESSORY_GAP, fit::Pair {
            primary_nat: label / headroom,
            secondary_nat: value / headroom,
            primary_share: fit::ROW_PRIMARY_SHARE,
        });
        RowColumns { label_w, value_w }
    }

    /// A row's two runs at their natural widths, exactly as [`Self::row_columns_under`] feeds them
    /// to [`fit::two_runs`] at headroom `1.0` and [`Self::measured_width`] sums them: the label
    /// hugged with [`fit::HUG_MARGIN`] (whole pixels), and the trailing value if it has one.
    fn row_runs(&self, row: &Row, measure: &dyn plx_machine::machine::Measure) -> (f32, Option<f32>) {
        let (size, bold) = self.label_style();
        (
            (measure.width_str(&row.label, size, bold) * fit::HUG_MARGIN).ceil(),
            row.readout().map(|v| measure.width_str(v, theme::size::LABEL, VALUE_BOLD != 0)),
        )
    }

    /// The trailing value's natural slot (run + its gap), `0` for a row without one.
    fn value_slot(row: &Row, measure: &dyn plx_machine::machine::Measure) -> f32 {
        row.readout().map_or(0.0, |v| measure.width_str(v, theme::size::LABEL, VALUE_BOLD != 0) + ACCESSORY_GAP)
    }

    /// The width a server-header section guarantees its app-owned accessory text (hugged like a
    /// row label), `0` when the header is not a server one yielding to app text. Shared by
    /// [`Self::header_columns`] and [`Self::measured_width`].
    fn accessory_floor(section: &Section, measure: &dyn plx_machine::machine::Measure) -> f32 {
        let app_text = if !section.accessory_app_prefix.is_empty() {
            Some(section.accessory_app_prefix.as_str())
        } else if section.accessory_origin == Origin::App {
            Some(section.accessory.as_str())
        } else {
            None
        };
        match app_text.filter(|_| section.header_origin == Origin::Server) {
            Some(t) => (measure.width_str(t, theme::size::MICRO, false) * fit::HUG_MARGIN).ceil(),
            None => 0.0,
        }
    }

    /// A [`Section::header`]'s (uppercased, as drawn) and [`Section::accessory`]'s columns, the
    /// header band's counterpart of [`Self::row_columns_under`]: the header is the primary, the
    /// accessory (pre-capped at [`ACCESSORY_W`]) the secondary, so both elide on their own side of
    /// the gap and never overlap. Without an accessory the header keeps the whole span.
    ///
    /// **A SERVER header yields to app-owned accessory text.** When the header is
    /// [`Origin::Server`] and the accessory is app text (or leads with an
    /// [`Section::accessory_app_prefix`]), the roles swap: the accessory is the primary, guaranteed
    /// at least that app text's width (hugged, like a row label), and the server header elides
    /// first. The accessory then elides only its server tail.
    fn header_columns(section: &Section, frame_w: f32, measure: &dyn plx_machine::machine::Measure, headroom: f32) -> (f32, f32) {
        let span = (frame_w - 2.0 * CONTENT_X).max(0.0);
        if section.accessory.is_empty() {
            return (span, 0.0);
        }
        let header_nat = measure.width_str(&section.header.to_uppercase(), theme::size::CAPTION, false) / headroom;
        let accessory_nat = measure.width_str(&section.accessory, theme::size::MICRO, false).min(ACCESSORY_W) / headroom;
        let floor = Self::accessory_floor(section, measure) / headroom;
        if floor == 0.0 {
            return fit::two_runs(span, ACCESSORY_GAP, fit::Pair {
                primary_nat: header_nat,
                secondary_nat: accessory_nat,
                primary_share: fit::ROW_PRIMARY_SHARE,
            });
        }
        let share = floor.max(span * (1.0 - fit::ROW_PRIMARY_SHARE)) / span.max(1.0);
        let (accessory_w, header_w) = fit::two_runs(span, ACCESSORY_GAP, fit::Pair {
            primary_nat: accessory_nat.max(floor),
            secondary_nat: header_nat,
            primary_share: share.min(1.0),
        });
        (header_w, accessory_w)
    }

    /// How opaque a row spanning `y..y + h` is drawn at the viewport's bottom edge `vis_bot`.
    ///
    /// The draw hard-clips to its frame, and at the TOP that is right (a row scrolling away under
    /// the crumb band). At the BOTTOM it cut the next row mid-glyph wherever the frame happened to
    /// end — the Settings column at the safe area, a popover at its height cap. So a row fades as
    /// the edge climbs from the list's own bottom air ([`BOT_PAD`]) up through its empty lower
    /// band ([`ROW_INK_PAD`]), and is fully transparent by the time the edge reaches its ink: the
    /// scissor only ever cuts padding or nothing visible. A row resting with the list's bottom
    /// air below it — where the last row settles — is whole.
    pub(crate) fn bottom_edge_alpha(&self, y: f32, h: f32, vis_bot: f32) -> f32 {
        edge_alpha(y + h - ROW_INK_PAD, vis_bot, ROW_INK_PAD)
    }

    /// The nearest **selectable** row to `i`: `i` itself when it is one, else the first non-separator
    /// after it, else the last one before it — so selection cannot come to rest on a grouping
    /// hairline, whoever set it. The one exception is a list that is ALL separators, which has no
    /// selectable row to offer and gets `i` back; that is degenerate rather than defended against
    /// (nothing can commit — `on_ok` reads no action off a hairline — and nothing panics).
    fn settle_sel(&self, i: i32) -> i32 {
        let n = self.n_rows();
        if n == 0 {
            return 0;
        }
        let i = i.clamp(0, n - 1);
        if !self.rows_at(i).sep {
            return i;
        }
        (i + 1..n)
            .find(|&j| !self.rows_at(j).sep)
            .or_else(|| (0..i).rev().find(|&j| !self.rows_at(j).sep))
            .unwrap_or(i)
    }

    /// Move the selection `delta` **selectable** rows: a separator is skipped rather than landed on,
    /// so one DOWN across the item menu's divider lands on the first state action, not on the rule.
    /// A step that would run off either end is dropped (the selection stays put), matching the old
    /// clamp.
    pub fn move_sel(&mut self, delta: i32) {
        let n = self.n_rows();
        if n == 0 {
            return;
        }
        let step = if delta < 0 { -1 } else { 1 };
        let mut cur = self.settle_sel(self.sel);
        for _ in 0..delta.abs() {
            let mut j = cur + step;
            while j >= 0 && j < n && self.rows_at(j).sep {
                j += step;
            }
            if j < 0 || j >= n {
                break;
            }
            cur = j;
        }
        self.sel = cur;
    }

    /// walk the visual layout top-to-bottom, invoking `f(content_y, global_row_index_or_-1, sec)`
    /// once per header (gi = -1) and once per row (gi >= 0). Cheap; the lists are short.
    fn walk(&self, mut f: impl FnMut(f32, i32, usize)) {
        let mut y = 0.0f32;
        let mut gi = 0i32;
        if self.title.is_some() {
            f(y, WALK_TITLE, 0);
            y += HDR_H + TITLE_GAP + DIV_H;
            f(y, WALK_DIVIDER, 0);
        }
        for (si, sec) in self.sections.iter().enumerate() {
            // an EMPTY section (a `Form` whose items all fell through) has nothing to divide from
            // the one above: no hairline, no gap, and it must not lengthen the content
            if si > 0 && !sec.rows.is_empty() {
                y += DIV_H;
                f(y, WALK_DIVIDER, si);
            }
            if !sec.header.is_empty() {
                f(y, -1, si);
                y += HDR_H;
            }
            for row in &sec.rows {
                f(y, gi, si);
                y += row.height_in(self.tall_row_h());
                gi += 1;
            }
        }
    }

    fn row_top(&self, target: i32) -> f32 {
        let mut out = 0.0;
        self.walk(|y, gi, _| {
            if gi == target {
                out = y;
            }
        });
        out
    }
    fn row_height(&self, target: i32) -> f32 {
        let mut n = 0i32;
        for sec in &self.sections {
            for row in &sec.rows {
                if n == target {
                    return row.height_in(self.tall_row_h());
                }
                n += 1;
            }
        }
        ROW_H
    }
    fn content_h(&self) -> f32 {
        // the LAST ROW's bottom, not the last walk event's: a trailing header or divider event
        // (an empty section) is not content
        let mut h = 0.0;
        self.walk(|y, gi, _| {
            if gi >= 0 {
                h = y;
            }
        });
        h + self.row_height(self.n_rows() - 1)
    }

    /// `frame_h` is the SAME rect height passed to [`Self::draw`]/[`Self::hit_row`] — this
    /// function subtracts [`PAD_V`] itself, so a caller must not subtract it a second time.
    pub fn update(&mut self, dt: f32, frame_h: f32) {
        if self.n_rows() == 0 {
            return;
        }
        let visible_h = (frame_h - PAD_V).max(0.0);
        let top = self.row_top(self.sel);
        let rh = self.row_height(self.sel);
        // top and bottom edges spring independently → the pill stretches/morphs between rows
        self.hl_top.step(top + PILL_INSET, 360.0, dt);
        self.hl_bot.step(top + rh - PILL_INSET, 360.0, dt);
        // keep the selection (plus a row of context) inside the viewport
        let content_h = self.content_h();
        let max_scroll = (content_h - visible_h).max(0.0);
        let mut sc = self.scroll.pos;
        if top - rh < sc {
            sc = top - rh;
        }
        if top + 2.0 * rh > sc + visible_h {
            sc = top + 2.0 * rh - visible_h;
        }
        sc = sc.clamp(0.0, max_scroll);
        self.scroll.step(sc, 300.0, dt);
    }

    /// The highlight springs as `(top position, top velocity, bottom position, bottom velocity)`.
    /// Test-only: screens need to prove they actually advance the shared table motion without
    /// exposing its implementation as product API.
    #[cfg(test)]
    pub(crate) fn highlight_motion(&self) -> (f32, f32, f32, f32) {
        (
            self.hl_top.pos,
            self.hl_top.vel,
            self.hl_bot.pos,
            self.hl_bot.vel,
        )
    }

    /// The scroll spring's live position, in content coordinates.
    ///
    /// It was `#[cfg(test)]` — the tests are still its main reader, for the reason below — until a
    /// caller needed to draw a SCROLL RAIL beside the list (`ui::filmography`). A rail is the one
    /// thing outside this widget that has to know where the scroll actually is; everything else it
    /// exposes is about rows.
    ///
    /// For tests: this is what a regression on the `update(dt, frame_h)` contract shows up as
    /// first — the pill motion test cannot see it, since a 2-row list never scrolls.
    pub(crate) fn scroll_pos(&self) -> f32 {
        self.scroll.pos
    }

    pub fn draw(&self, p: Painter, frame: Rect, measure: &dyn plx_machine::machine::Measure) {
        if self.n_rows() == 0 {
            Label::new(
                plx_platform::i18n::msg::widgets_tracks_empty_c().as_ptr(),
                theme::size::BODY,
                theme::TEXT_TERTIARY,
            )
            .h(HAlign::Center)
            .draw(p, frame);
            return;
        }
        self.fit_notes(frame.w, measure);
        // Hard-clip everything below to the panel frame: the list overflows, so a partial edge row is
        // cut cleanly at the frame instead of poking over the video / control buttons — and, unlike the
        // old fade masks, a tall two-line edge row is cut uniformly (the fade left its title bright but
        // faded its detail line, which read as a broken clip). A `ClipScope`, so it intersects with an
        // enclosing panel clip and restores it when this fn returns.
        let _clip = crate::ui::screen::ClipScope::open_in(p, frame);
        let top0 = frame.y + TOP_PAD;
        let scroll = self.scroll.pos;
        let vis_top = frame.y;
        let vis_bot = frame.y + frame.h;

        let white = theme::TEXT_PRIMARY;
        let dimc = theme::TEXT_TERTIARY; // was #8a8a8e; unified onto the tertiary grey
        let ink = crate::ui::ACCENT_INK; // text/glyph over the light pill
        let content_x = frame.x + SIDE + CONTENT_PAD;
        let text_right = frame.x + frame.w - SIDE - CONTENT_PAD;

        // ---- sliding pill (under the rows) — warm off-white; top/bottom edges morph independently ----
        // It follows its GROUP's dim: the focused row of an unreachable server must not be the one
        // bright thing in a panel that is telling you the server is unreachable.
        let py0 = top0 + self.hl_top.pos - scroll;
        let py1 = top0 + self.hl_bot.pos - scroll;
        let pill = Rect::new(
            frame.x + SIDE,
            py0,
            frame.w - 2.0 * SIDE,
            (py1 - py0).max(1.0),
        );
        if self.list_focused && pill.y + pill.h > vis_top && pill.y < vis_bot {
            self.group_painter(p, self.section_of_row(self.sel)).rrect(
                pill,
                PILL_RAD,
                PILL_RAD,
                crate::ui::ACCENT,
            );
        }

        // ---- headers + rows ----
        self.walk(|cy, gi, si| {
            // ONE alpha over the whole group (see `Section::dim`) — pushed here, at the top of the
            // walk, so header, accessory, rows, marks and read-outs can never dim out of step.
            let sy = top0 + cy - scroll;
            let p = self.event_painter(p, gi, si);
            if gi == WALK_DIVIDER {
                // the hairline between this section and the one above, headed or not, centred in
                // the `DIV_H` gap; it fades with the group like everything else in it
                let y = sy - DIV_H * 0.5;
                if y + 2.0 > vis_top && y < vis_bot {
                    p.rect(
                        Rect::new(content_x, y, frame.w - 2.0 * (SIDE + CONTENT_PAD), 2.0),
                        0.0,
                        theme::HAIRLINE,
                        theme::HAIRLINE,
                        0.0,
                    );
                }
                return;
            }
            if gi == WALK_TITLE {
                if sy + HDR_H > vis_top && sy < vis_bot {
                    let p = p.alpha(edge_alpha(sy + HDR_H - HDR_INK_PAD, vis_bot, HDR_INK_PAD));
                    let hsz = theme::size::CAPTION;
                    let (cap_top, baseline) = plx_gfx::text::text_cap_band(hsz, 0);
                    let band = Rect::new(content_x, sy + HEADER_CAP_INSET, (text_right - content_x).max(0.0), baseline - cap_top);
                    if let Some((title, x0)) = self.title_run(measure) {
                        let glyph = CString::new(TITLE_BACK_GLYPH).unwrap_or_default();
                        Label::new(glyph.as_ptr(), hsz, self.header_ink).v(VAlign::CapTop).draw(p, band);
                        let text_band = Rect::new(band.x + x0, band.y, (band.w - x0).max(0.0), band.h);
                        let text = plx_gfx::text::elide_by(&title, text_band.w, false, |t| measure.width_str(t, hsz, false));
                        if let Ok(cs) = CString::new(text) {
                            Label::new(cs.as_ptr(), hsz, self.header_ink).v(VAlign::CapTop).draw(p, text_band);
                        }
                    }
                }
                return;
            }
            if gi == -1 {
                // panel/section header; scissor-clipped to `frame`
                if sy + HDR_H > vis_top && sy < vis_bot {
                    // fades out at the bottom edge before its caps are cut (`bottom_edge_alpha`)
                    let p = p.alpha(edge_alpha(sy + HDR_H - HDR_INK_PAD, vis_bot, HDR_INK_PAD));
                    let sec = &self.sections[si];
                    // **CAPS at CAPTION, one size in BOTH size classes.** The caps are what make a
                    // header read as a label rather than as a row, which is why the size stops
                    // varying: at HEADLINE — what the non-compact class used to draw — a header
                    // ties with or outweighs the rows it heads, and on the Sources panel (a
                    // two-line picker, so non-compact) that put the machine names on screen bigger
                    // than the libraries they name. Design system, `TableView.prompt.md`.
                    //
                    // `to_uppercase`, not `to_ascii_uppercase`: a header is a machine name or a
                    // library name and can be any script — the share measured here is Cyrillic.
                    let hsz = theme::size::CAPTION;
                    let cap_y = sy + HEADER_CAP_INSET;
                    let (cap_top, baseline) = plx_gfx::text::text_cap_band(hsz, 0);
                    let header_band = Rect::new(
                        content_x,
                        cap_y,
                        (text_right - content_x).max(0.0),
                        baseline - cap_top,
                    );
                    let (header_w, accessory_w) = Self::header_columns(sec, frame.w, measure, 1.0);
                    let header_text = plx_gfx::text::elide_by(&sec.header.to_uppercase(), header_w, false, |t| {
                        measure.width_str(t, hsz, false)
                    });
                    if let Ok(cs) = CString::new(header_text) {
                        Label::new(cs.as_ptr(), hsz, self.header_ink)
                            .v(VAlign::CapTop)
                            .draw(p, header_band);
                    }
                    // trailing accessory, one rung down and on the header's own baseline. Elided,
                    // because it is the run that gives way (see `Section::accessory`).
                    if !sec.accessory.is_empty() {
                        // MICRO, a rung below the header: "the header names the group, the
                        // accessory only qualifies it, so it never ties with the header"
                        // (`TableView.prompt.md`). At CAPTION the two were the same size and a
                        // plex.tv handle read as loud as the machine it hangs off.
                        let asz = theme::size::MICRO;
                        let a = plx_gfx::text::elide_by(&sec.accessory, accessory_w, false, |t| {
                            measure.width_str(t, asz, false)
                        });
                        if let Ok(ac) = CString::new(a) {
                            Label::new(ac.as_ptr(), asz, self.header_ink)
                                .h(HAlign::Right)
                                .v(VAlign::Baseline)
                                .draw(p, header_band);
                        }
                    }
                }
                return;
            }
            let row = self.rows_at(gi);
            let h = row.height_in(self.tall_row_h());
            if sy + h < vis_top || sy > vis_bot {
                return; // fully scrolled out; a partial edge row is drawn and scissor-clipped to `frame`
            }
            // …and at the BOTTOM edge it is faded out before the scissor can reach its ink
            let edge = self.bottom_edge_alpha(sy, h, vis_bot);
            if edge <= 0.0 {
                return;
            }
            let p = p.alpha(edge);
            if row.sep && row.label.is_empty() {
                // grouping hairline, on the row's centre line and inset to the label column so it
                // reads as a divider between groups rather than a full-bleed panel rule
                p.rect(
                    Rect::new(
                        content_x,
                        sy + h * 0.5 - 1.0,
                        frame.w - 2.0 * (SIDE + CONTENT_PAD),
                        2.0,
                    ),
                    0.0,
                    theme::HAIRLINE,
                    theme::HAIRLINE,
                    0.0,
                );
                return;
            }
            if row.sep {
                // a `note` row: quiet CAPTION text WRAPPED in the row-label column, never a mark or
                // a pill — it cannot be focused (skipped by every `sep` check above), so it never
                // draws over the sliding highlight either. Its height is `Row::height_in`'s.
                let nsz = theme::size::CAPTION;
                let x = content_x + CHECK_W + GAP;
                let view = crate::ui::text_view::TextView::new(&row.label, nsz, dimc)
                    .with_measure(measure)
                    .leading(NOTE_LEADING);
                let w = (text_right - x).max(0.0);
                let lines = view.line_count(w);
                // centre the INK (first cap-top to last baseline), as the one-line note was
                let ink_h = (lines.saturating_sub(1)) as f32 * NOTE_LEADING + measure.cap_h(nsz);
                view.draw(p, Rect::new(x, sy + (h - ink_h) * 0.5, w, ink_h));
                return;
            }
            // **`list_focused` gates the INK, not just the pill.** `ink` is near-black and is only
            // legible ON the accent pill; suppressing the pill while still flipping the ink drew
            // black-on-panel rows, which is what the owner saw the moment focus moved to the
            // Sources panel's level pills. The two are one decision and are read from one flag.
            let focused = gi == self.sel && self.list_focused;
            let base = if focused {
                ink
            } else if row.dim {
                dimc
            } else {
                white
            };
            let row_bg = if focused { crate::ui::ACCENT } else { PANEL_BG };
            let cyc = sy + h * 0.5; // row vertical center

            // Leading column (SVG): the PICKER's tick, or an ACTION's glyph — one or the other, and
            // never a switch. A mark here says WHERE YOU ARE; what a row is SET to is a word at the
            // trailing edge (below), and no row is allowed to say both.
            let label_x = content_x + CHECK_W + GAP;
            let lead = if row.checked {
                Some(crate::ui::icons::Icon::Check)
            } else {
                row.licon
            };
            if let Some(li) = lead {
                let cs = 26.0f32;
                let cr = Rect::new(content_x + (CHECK_W - cs) * 0.5, cyc - cs * 0.5, cs, cs);
                crate::ui::icons::draw(p, li, cr, base);
            }
            // trailing accessory (SVG)
            let mut trailing = 0.0f32;
            if let Some(ti) = row.ticon {
                let cs = ACCESSORY_ICON_W;
                let cr = Rect::new(text_right + ticon_box_dx(ti), cyc - cs * 0.5, cs, cs);
                crate::ui::icons::draw(p, ti, cr, base);
                trailing = ticon_slot_w(ti);
            }
            // PLACE 4 — the badge run, RIGHT-ALIGNED at the trailing edge and the outermost of the
            // three trailing runs (the design system's cell is a flex row whose label block takes
            // all the slack, so `badge` — written last — is flush right, with the read-out beside
            // it). It was drawn INLINE, starting at the label's own drawn right edge, which gave a
            // chip the design pins flush a per-ROW x: a column of resolution classes, or of
            // FORCED/SDH/codec tags, landed wherever each label happened to end, so the panel read
            // as ragged text rather than as a column you can compare straight down.
            //
            // Centred on the ROW BOX (`cyc`), not on the title's cap band: the chip is a sibling of
            // the whole label block, so on a two-line row it sits level with the pair rather than
            // ~16px high, level with the first line.
            if !row.badges.is_empty() {
                let run: f32 = row
                    .badges
                    .iter()
                    .map(|b| crate::ui::widgets::badge_w(b.text(), None, measure))
                    .sum::<f32>()
                    + BADGE_GAP * (row.badges.len() - 1) as f32;
                let mut bx = text_right - trailing - run;
                for b in row.badges.iter() {
                    // the shared chip leaf in this row's contextual colours: the row's own ink for
                    // the label, the design's keyline for the ring (the pill's near-black ink over
                    // a focused row, where a 55%-white stroke would vanish), the row's ground
                    // knocked out of the interior
                    let sty = crate::ui::widgets::BadgeStyle::Outlined {
                        col: base,
                        border: if focused { base } else { theme::OVERLAY_BORDER },
                        bg: row_bg,
                    };
                    bx += crate::ui::widgets::badge(p, bx, cyc, b.text(), None, sty, measure) + BADGE_GAP;
                }
                trailing += run + ACCESSORY_GAP;
            }
            // Trailing VALUE — the read-out that says what this row is set to ("On"/"Off" for a
            // switch, or any word). One step behind the label in ink, so the label is what you read
            // and the value is what you check; over the focused row's near-white pill that step is
            // an alpha of the pill's own ink rather than a grey, which would go muddy on it. That
            // step is the ink's ALONE — the run itself is bold (see [`VALUE_BOLD`], which all three
            // calls below take so the measure and the paint can never be two different faces).
            if let Some(v) = row.readout() {
                let ink = row_value_ink(row, focused);
                // the value gives way before the label (`row_columns`): elided to its column
                let vsz = theme::size::LABEL;
                let value_w = self.row_columns(row, frame.w, measure).value_w;
                let v = plx_gfx::text::elide_by(v, value_w, false, |t| {
                    measure.width_str(t, vsz, VALUE_BOLD != 0)
                });
                if let Ok(vc) = std::ffi::CString::new(v) {
                    let vy = plx_gfx::text::text_vcenter_y(vsz, VALUE_BOLD, cyc);
                    p.text(
                        vc.as_ptr(),
                        text_right - trailing,
                        vy,
                        vsz,
                        ink,
                        2,
                        VALUE_BOLD,
                    );
                }
            }
            // Single-line rows centre their label on the row by cap band. Two-line rows stack a
            // title over a detail sub-line: lay the pair out off both cap bands and centre it in the
            // tall row, with an explicit gap between the title baseline and the detail cap-top
            // (layout ≠ paint — the old fixed offsets left the two lines cramped after centring).
            let two_line = !row.detail.is_empty();
            // two-line rows follow the table's size class too (compact = BODY regular titles)
            let (tsz, tbold) = if self.compact {
                (theme::size::BODY, 0)
            } else {
                (theme::size::HEADLINE, 1)
            };
            let (title_y, detail_y) = if two_line {
                let (t_top, t_base) = plx_gfx::text::text_cap_band(tsz, tbold);
                let (d_top, d_base) = plx_gfx::text::text_cap_band(theme::size::CAPTION, 0);
                let (t_cap, d_cap) = (t_base - t_top, d_base - d_top); // cap heights
                let pair_gap = ROW_SUB_GAP; // title baseline → detail cap-top
                let pair_top = sy + (h - (t_cap + pair_gap + d_cap)) * 0.5; // title cap-top
                (pair_top - t_top, pair_top + t_cap + pair_gap - d_top)
            } else {
                (0.0, 0.0) // unused single-line; Label centres the label
            };
            // PLACE 2 — the label over its optional sub-line. Compact tables read their single-line
            // labels at BODY regular.
            let (lsz, lbold) = if self.compact {
                (theme::size::BODY, 0)
            } else {
                (theme::size::HEADLINE, 1)
            };
            let text_w = self.label_width(row, frame.w, measure);
            let lbl = plx_gfx::text::elide_by(&row.label, text_w, false, |t| {
                measure.width_str(t, lsz, lbold != 0)
            });
            if let Ok(cs) = CString::new(lbl) {
                if two_line {
                    p.text(cs.as_ptr(), label_x, title_y, tsz, base, 0, tbold);
                } else {
                    let mut lab = Label::new(cs.as_ptr(), lsz, base);
                    if lbold == 1 {
                        lab = lab.bold();
                    }
                    lab.draw(p, Rect::new(label_x, sy, 0.0, h));
                }
            }
            // detail sub-line, elided (long Cyrillic descriptors would run off the edge) — to the
            // SAME right edge as the label above it, because the design system puts the pair in one
            // `flex:1` box. It used to elide against the bare row width, which was harmless only
            // while the badges sat on the title's line: right-aligned and row-centred, a chip now
            // occupies the sub-line's band too, and an unbounded sub-line would run under it.
            if !row.detail.is_empty() {
                let sub = if focused {
                    theme::scrim_black(0.6)
                } else {
                    dimc
                };
                let detail = plx_gfx::text::elide_by(&row.detail, text_w, false, |t| {
                    measure.width_str(t, theme::size::CAPTION, false)
                });
                if let Ok(cd) = CString::new(detail) {
                    p.text(
                        cd.as_ptr(),
                        label_x,
                        detail_y,
                        theme::size::CAPTION,
                        sub,
                        0,
                        0,
                    );
                }
            }
        });
    }

    /// The painter for one walk event: its section's ([`Self::group_painter`]), except the page
    /// title band and ITS hairline (the walk's first two events, the only divider tagged section 0
    /// since a section never divides from nothing above it), which belong to the page and so must
    /// never be dimmed by a dimmed first section.
    fn event_painter(&self, p: Painter, gi: i32, si: usize) -> Painter {
        if gi == WALK_TITLE || (gi == WALK_DIVIDER && si == 0) {
            p
        } else {
            self.group_painter(p, si)
        }
    }

    /// `p`, dimmed if section `si` is — the ONE place [`Section::dim`] becomes an alpha.
    fn group_painter(&self, p: Painter, si: usize) -> Painter {
        if self.sections.get(si).is_some_and(|s| s.dim) {
            p.alpha(GROUP_DIM_A)
        } else {
            p
        }
    }

    /// Which section global row `gi` belongs to (0 when it belongs to none — an empty table).
    fn section_of_row(&self, gi: i32) -> usize {
        let mut n = 0i32;
        for (si, sec) in self.sections.iter().enumerate() {
            n += sec.rows.len() as i32;
            if gi < n {
                return si;
            }
        }
        0
    }

    /// **One row, for an in-place edit** — a read-out that changes while the list does not (the
    /// Subtitles panel's Color value), so a press re-writes that row alone instead of rebuilding
    /// every section. `None` past the end. The caller must not change the row's height class.
    pub(crate) fn row_mut(&mut self, gi: i32) -> Option<&mut Row> {
        self.rev = next_rev();
        usize::try_from(gi).ok().and_then(|gi| row_mut_in(&mut self.sections, gi))
    }

    fn rows_at(&self, gi: i32) -> &Row {
        let mut n = 0i32;
        for sec in &self.sections {
            for row in &sec.rows {
                if n == gi {
                    return row;
                }
                n += 1;
            }
        }
        // unreachable for a valid gi (draw early-returns when there are no rows)
        self.sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .next()
            .unwrap()
    }
}

/// The ink of a row's trailing read-out ([`Row::readout`]): one step behind the row's label, so it
/// follows the row's [`Row::dim`] as well as its own [`Row::value_dim`]. A dim row's label is
/// already [`theme::TEXT_TERTIARY`], so its read-out takes [`theme::ROW_VALUE_INK_DIM`] below
/// that; drawn at the live row's ink, a step that cannot be taken (the Timing section's Earlier at
/// its floor) still announced itself at the trailing edge. Over the focused pill a dim row's
/// read-out takes the quiet rung.
fn row_value_ink(row: &Row, focused: bool) -> [f32; 4] {
    match (focused, row.dim, row.value_dim) {
        (true, false, false) => theme::ROW_VALUE_INK_ON,
        (true, _, _) => theme::ROW_VALUE_INK_ON_DIM,
        (false, true, _) => theme::ROW_VALUE_INK_DIM,
        (false, false, false) => theme::TEXT_SECONDARY,
        (false, false, true) => theme::TEXT_TERTIARY,
    }
}

#[cfg(test)]
mod tests {
    /// **A dimmed row's read-out dims with it.** The Timing section's Earlier row at the
    /// selected kind's floor is `dim`, and its "−0.1 s" read-out was drawn at the SAME ink as the
    /// live Later row's — so the step that could not be taken still announced itself at the
    /// trailing edge. The read-out stays one step behind its label: a dim label is
    /// [`theme::TEXT_TERTIARY`], so a dim row's value sits below that.
    #[test]
    fn a_dim_rows_readout_follows_the_row_dim() {
        let live = Row::new("Later").value("+0.1 s").value_dim(true);
        let dimmed = Row::new("Earlier").value("\u{2212}0.1 s").value_dim(true).dim(true);
        assert_ne!(
            row_value_ink(&dimmed, false),
            row_value_ink(&live, false),
            "a dim row's read-out must not keep the live row's ink",
        );
        assert!(
            row_value_ink(&dimmed, false)[3] < theme::TEXT_TERTIARY[3],
            "a dim row's read-out sits a step behind its dim label",
        );
        // an undimmed, unquietened read-out is unchanged
        let plain = Row::new("Audio").value("English");
        assert_eq!(row_value_ink(&plain, false), theme::TEXT_SECONDARY);
        assert_eq!(row_value_ink(&plain, true), theme::ROW_VALUE_INK_ON);
        // over the focused pill a dim row's read-out takes the quiet rung
        let dim_plain = Row::new("Audio").value("English").dim(true);
        assert_eq!(row_value_ink(&dim_plain, true), theme::ROW_VALUE_INK_ON_DIM);
    }

    #[test]
    fn table_motion_canonical_state_covers_hidden_spring_velocity_and_layout_flags() {
        fn hash(table: &super::TableView) -> u64 {
            let mut c = plx_machine::machine::Canon::new();
            table.write_motion(&mut c);
            c.finish()
        }
        let baseline = hash(&super::TableView::new());
        for field in 0..7 {
            let mut table = super::TableView::new();
            match field {
                0 => table.hl_top.vel = 1.0,
                1 => table.hl_bot.vel = 1.0,
                2 => table.scroll.vel = 1.0,
                3 => table.sel = 7,
                4 => table.list_focused = !table.list_focused,
                5 => table.compact = !table.compact,
                6 => table.tall = !table.tall,
                _ => unreachable!(),
            }
            assert_ne!(hash(&table), baseline, "field {field}");
            assert_eq!(hash(&table), hash(&table), "reading state must not advance animation");
        }
    }

    use super::*;

    /// ONE trailing read-out per row, resolved in ONE place. The rule the design system states and
    /// this widget enforces is that **a mark says where you are and a word says what is set, and no
    /// row is allowed to say both** — so a switch's state is the WORD `On`/`Off` at the trailing
    /// edge (there is no ring/ticked-ring pair any more, assets and all), a leading tick is not a
    /// read-out at all, and a row carrying both a `value` and a `toggle` draws exactly one of them.
    ///
    /// Worth pinning even though it is four lines of `match`: `library.rs`'s Sources test spells the
    /// `On`/`Off` mapping out a second time to grade the row MODEL, and this is the assertion that
    /// the copy it grades is the copy the widget actually draws.
    #[test]
    fn a_row_states_exactly_one_trailing_read_out() {
        assert_eq!(
            Row::new("Unwatched only").toggle(true).readout(),
            Some("On")
        );
        assert_eq!(
            Row::new("Unwatched only").toggle(false).readout(),
            Some("Off")
        );
        assert_eq!(Row::new("Genre").value("All").readout(), Some("All"));
        // both set: the explicit word wins and the switch's is NOT drawn beside it
        assert_eq!(
            Row::new("Genre").value("Comedy").toggle(true).readout(),
            Some("Comedy")
        );
        // a leading mark is a different column with a different job — it states no value
        assert_eq!(Row::new("English").checked(true).readout(), None);
        assert_eq!(Row::new("Chapters").readout(), None);
    }

    /// The shared table promises a travelling focus pill. A caller that changes `sel` but forgets
    /// to call `update` gets exactly the reported failure: new-row ink with the old pill, followed
    /// by a later jump. Pin both halves of the motion contract here — moving and genuinely resting.
    /// **The last SELECTABLE row is not the last index.** `route_screen`'s rule 2 (DOWN off the
    /// last row enters the action band) is graded on this, so on a list that ends in a grouping
    /// hairline — which `move_sel` can never land on — the band would be unreachable if the
    /// predicate were `sel == n_rows() - 1`.
    #[test]
    fn the_last_selectable_row_is_never_a_grouping_hairline() {
        let mut t = TableView::new();
        t.set_sections(
            vec![Section::new("S")
                .row(Row::new("a"))
                .row(Row::new("b"))
                .row(Row::separator())],
            0,
            false,
        );
        assert_eq!(t.n_rows(), 3);
        assert_eq!(t.last_row(), Some(1), "the hairline is not a landable row");
        t.sel = 1;
        assert!(t.at_last_row());
        t.sel = 0;
        assert!(!t.at_last_row());

        let empty = TableView::new();
        assert_eq!(empty.last_row(), None);
        assert!(!empty.at_last_row());
    }

    /// **A menu never opens with its focus on a destructive action.** The opening row steps over
    /// destructive rows and separators; when every row is destructive it is the first anyway.
    #[test]
    fn a_menu_opens_on_its_first_non_destructive_row() {
        let mut t = TableView::new();
        t.open_sections(vec![Section::new("S")
            .row(Row::new("Sign out").destructive(true))
            .row(Row::separator())
            .row(Row::new("Settings"))]);
        assert_eq!(t.sel, 2);
        assert_eq!(t.opening_row(), 2);

        t.open_sections(vec![Section::new("S")
            .row(Row::separator())
            .row(Row::new("Remove").destructive(true))]);
        assert_eq!(t.sel, 1, "all destructive: the first selectable row anyway");

        t.open_sections(vec![Section::new("S").row(Row::new("a")).row(Row::new("b"))]);
        assert_eq!(t.sel, 0, "an ordinary menu still opens on its first row");

        t.open_sections(Vec::new());
        assert_eq!(t.sel, 0);
    }

    /// **A row OPENS something exactly when it wears the drill-in chevron.** `route_screen`'s
    /// rule 8 is read off the painted affordance rather than kept as a second per-screen list.
    #[test]
    fn a_row_opens_something_exactly_when_it_wears_the_drill_in_chevron() {
        let mut t = TableView::new();
        t.set_sections(
            vec![Section::new("S")
                .row(Row::new("a door").chevron(true))
                .row(Row::new("a switch").toggle(true))
                .row(Row::new("a plain row"))],
            0,
            false,
        );
        assert!(t.row_opens(0));
        assert!(!t.row_opens(1), "a switch changes a value in place");
        assert!(!t.row_opens(2));
        assert!(!t.row_opens(-1), "and an out-of-range ask answers no rather than panicking");
        assert!(!t.row_opens(99));
    }

    #[test]
    fn the_focus_pill_runs_between_rows_and_goes_quiet_at_rest() {
        let _serial = plx_base::testlock::serial();
        let mut t = TableView::new();
        t.set_sections(
            vec![Section::new("").row(Row::new("One")).row(Row::new("Two"))],
            0,
            false,
        );
        let start = t.highlight_motion();
        t.move_sel(1);
        t.update(1.0 / 60.0, t.measured_height());
        let running = t.highlight_motion();
        assert!(
            running.0 > start.0,
            "the pill did not leave its old row: {running:?}"
        );
        assert!(
            running.1.abs() > 0.0,
            "a travelling edge must carry velocity: {running:?}"
        );

        for _ in 0..240 {
            t.update(1.0 / 60.0, t.measured_height());
        }
        let resting = t.highlight_motion();
        let target = t.row_top(1) + PILL_INSET;
        assert!(
            (resting.0 - target).abs() < 0.01,
            "pill stopped away from row 1: {resting:?}"
        );
        assert!(
            resting.1.abs() < 0.01,
            "settled pill still reports motion: {resting:?}"
        );
    }

    /// **Regression for the Settings ▸ Privacy & data report**: "Delete all local data" (the last
    /// row) never scrolled fully into view. `update`'s `frame_h` is the SAME height passed to
    /// `draw`/`hit_row` — a caller that (wrongly) subtracted [`PAD_V`] before calling `update`, or
    /// an `update` that (wrongly) failed to subtract it internally, makes the scroll clamp believe
    /// the viewport is `PAD_V` taller than the clipped frame it is actually drawn into, so it stops
    /// short of the true bottom by exactly that amount — the last row settles PARTIALLY behind the
    /// clip. This overflows a small frame on purpose and settles on the last row.
    #[test]
    fn scrolling_to_the_last_row_reveals_it_fully_above_the_bottom_pad() {
        let _serial = plx_base::testlock::serial();
        let mut t = TableView::new();
        let mut sec = Section::new("S");
        for i in 0..20 {
            sec = sec.row(Row::new(format!("row {i}")));
        }
        t.set_sections(vec![sec], 0, false);
        let content_h = t.measured_height() - PAD_V;
        let frame_h = 300.0; // far shorter than the 20-row content, so this frame must scroll
        assert!(content_h > frame_h, "test needs overflowing content");

        t.move_sel(t.n_rows() - 1);
        for _ in 0..300 {
            t.update(1.0 / 60.0, frame_h);
        }

        // The frame passed to `draw` reserves TOP_PAD above row 0 and BOT_PAD below the last row —
        // so at rest the last row's bottom (`content_h`, in content coordinates) must land exactly
        // `BOT_PAD` above the clipped frame's bottom edge, not merely somewhere inside it.
        let last_row_bottom_on_screen = TOP_PAD + content_h - t.scroll_pos();
        let want = frame_h - BOT_PAD;
        assert!(
            (last_row_bottom_on_screen - want).abs() < 0.5,
            "last row settled at {last_row_bottom_on_screen}, wanted {want} \
             (frame_h={frame_h}, content_h={content_h}, scroll={})",
            t.scroll_pos()
        );
    }

    /// Owner reports: the Settings table (issue 10) and the player's Quality popover (its last
    /// rung) ended in a row cut mid-glyph by the viewport's bottom edge. A row crossing that edge
    /// must be transparent before the edge reaches its ink, and a row resting with the list's own
    /// bottom air below it must be whole. Swept over every scroll a DOWN walk passes through, in
    /// both a Settings-sized frame and a popover-sized one, with plain and two-line rows.
    #[test]
    fn no_row_is_drawn_cut_mid_glyph_at_the_bottom_edge() {
        let _serial = plx_base::testlock::serial();
        for (frame_h, detail_every) in [(300.0f32, 2usize), (520.0, 3), (455.0, 0)] {
            let mut t = TableView::new();
            let mut sec = Section::new("S");
            for i in 0..18 {
                let mut row = Row::new(format!("row {i}")).value("value");
                if detail_every > 0 && i % detail_every == 0 { row = row.detail("detail"); }
                sec = sec.row(row);
            }
            t.set_sections(vec![sec, Section::new("T").row(Row::new("tail"))], 0, false);
            let frame = Rect::new(0.0, 100.0, 700.0, frame_h);
            let bottom = frame.y + frame.h;
            let mut crossed = 0;
            for step in 0..t.n_rows() {
                if step > 0 { t.move_sel(1); }
                for _ in 0..12 { t.update(1.0 / 60.0, frame_h); }
                for i in 0..t.n_rows() {
                    let Some(r) = t.row_frame(frame, i) else { continue };
                    let a = t.bottom_edge_alpha(r.y, r.h, bottom);
                    if r.y < bottom && r.y + r.h > bottom {
                        crossed += 1;
                        assert!(a == 0.0 || r.y + r.h - ROW_INK_PAD <= bottom,
                            "row {i} at {}..{} is drawn at {a} while the edge {bottom} cuts its ink", r.y, r.y + r.h);
                    }
                    if r.y + r.h + BOT_PAD <= bottom {
                        assert_eq!(a, 1.0, "row {i} resting above the bottom air is whole");
                    }
                }
            }
            assert!(crossed > 0, "the premise: rows do cross the bottom edge at {frame_h}");
        }
    }

    /// A long (uppercased) header and its right-aligned accessory never claim overlapping pixels,
    /// for any header/accessory pair.
    #[test]
    fn a_long_header_never_overlaps_its_accessory() {
        use crate::ui::fixture::FixtureMeasure;
        let frame_w = 700.0;
        let long_header = "a machine name so long it would eat the whole header band by itself";
        let cases = [
            (long_header, "owner@example.com"),
            (long_header, "x"),
            ("short", "a handle also long enough to matter here"),
            ("S", ""),
        ];
        for (header, accessory) in cases {
            let sec = Section::new(header).accessory(accessory);
            let (header_w, accessory_w) = TableView::header_columns(&sec, frame_w, &FixtureMeasure, 1.0);
            let span = frame_w - 2.0 * CONTENT_X;
            assert!(header_w >= 0.0 && accessory_w >= 0.0, "{header:?}/{accessory:?}: negative column");
            if accessory.is_empty() {
                assert_eq!(accessory_w, 0.0);
                continue;
            }
            assert!(
                header_w + ACCESSORY_GAP + accessory_w <= span + 0.01,
                "{header:?}/{accessory:?}: header_w={header_w} + gap + accessory_w={accessory_w} overruns the {span}px band — they would overlap",
            );
        }
    }

    /// `row_columns`' fallback for a value nothing shortened: the value is elided to what is left
    /// beside its label, but never to nothing.
    #[test]
    fn row_columns_still_elides_an_unshortened_long_value() {
        use plx_base::fontcov::advances::ShippedMeasure as M;
        use plx_machine::machine::Measure;
        use crate::ui::route_screen::RouteLayout;
        let frame_w = RouteLayout::screen().sectioned_table().w;
        // Too long to fit beside its label.
        const LONG_VALUE: &str = "This value is deliberately far too long to sit beside its label";
        let mut table = TableView::new();
        table.compact = false;
        let rows = [
            Row::new("Quality").value("Not set").chevron(true),
            Row::new("Direct Play").value(LONG_VALUE).chevron(true),
        ];
        table.set_sections(vec![rows.into_iter().fold(Section::new(""), Section::row)], 0, false);
        let direct_play_row = &table.sections[0].rows[1];
        let cols = table.row_columns(direct_play_row, frame_w, &M);
        let natural = M.width_str(LONG_VALUE, theme::size::LABEL, true);
        assert!(cols.value_w < natural, "the premise: the long value does not fit beside its label");
        assert!(cols.value_w > 0.0, "a value keeps a visible slot");
    }

    /// `fit_report` does not flag a short value that fits, and still flags one that does not.
    #[test]
    fn fit_report_does_not_flag_a_short_value_that_fits() {
        use plx_base::fontcov::advances::ShippedMeasure as M;
        use crate::ui::fit::HEADROOM;
        let frame_w = 700.0;
        let mut table = TableView::new();
        table.compact = false;
        let rows = [
            Row::new("Unwatched only").value("Off"),
            Row::new("Direct Play").value(
                "This value is deliberately far too long to sit beside its label in a 700px frame",
            ),
        ];
        table.set_sections(vec![rows.into_iter().fold(Section::new(""), Section::row)], 0, false);
        let issues = table.fit_report(frame_w, &M, HEADROOM);
        let short_flagged = issues.iter().any(|i| i.role == FitRole::Value && i.text == "Off");
        assert!(!short_flagged, "a short value that fits must not be reported: {issues:?}");
        let long_flagged = issues.iter().any(|i| i.role == FitRole::Value && i.text.starts_with("This value"));
        assert!(long_flagged, "a genuinely too-long value must still be reported: {issues:?}");
    }

    /// **Every row's LAST trailing element ends on one right edge** (the row content's right edge).
    /// On a row with a trailing icon that is the icon; on a row without one it is the read-out
    /// itself, flush — no empty column is held open. And a hugged panel must show every run whole.
    #[test]
    fn a_sections_values_share_a_right_edge_and_the_hugged_panel_elides_nothing() {
        use plx_base::fontcov::advances::ShippedMeasure as M;
        use plx_machine::machine::Measure;
        let mut table = TableView::new();
        table.compact = false;
        let section = Section::new("")
            .row(Row::new("Unwatched only").value("Off"))
            .row(Row::new("Genre").value("All").chevron(true))
            .row(Row::new("Lone").chevron(true));
        table.set_sections(vec![section], 0, false);
        let content_right = 800.0 - SIDE - CONTENT_PAD;
        let (off, genre, lone) = (&table.sections[0].rows[0], &table.sections[0].rows[1], &table.sections[0].rows[2]);
        // the chevron's INK ends on the content edge (its box overhangs by the right-side bearing)
        let ink_right = |row: &Row| {
            let icon = row.ticon.unwrap();
            content_right + ticon_box_dx(icon) + crate::ui::icons::ink_x(icon).1 * ACCESSORY_ICON_W
        };
        let last_edge = |row: &Row| if row.ticon.is_some() { ink_right(row) } else { table.value_right_edge(row, 800.0) };
        let near = |a: f32, b: f32| (a - b).abs() <= 0.5;
        assert!(
            near(table.value_right_edge(off, 800.0), ink_right(genre)),
            "'Off' ends at {} but the chevron ink at {}",
            table.value_right_edge(off, 800.0),
            ink_right(genre)
        );
        assert!(near(ink_right(genre), content_right), "chevron ink {} is not on the content edge {content_right}", ink_right(genre));
        assert!(near(last_edge(off), last_edge(lone)), "a value row and a lone-chevron row end on different edges");
        assert!(
            near(table.value_right_edge(genre, 800.0), content_right - ticon_slot_w(genre.ticon.unwrap())),
            "a value beside a chevron stops one ink-width + gap short of the edge"
        );
        let w = table.menu_panel_width(&M);
        let failures = table.app_fit_failures_hugged("filter");
        assert!(failures.is_empty(), "elision at the hugged width {w}: {failures:?}");
        let (size, bold) = table.label_style();
        for (row, icon) in [(off, 0.0), (genre, ticon_slot_w(crate::ui::icons::Icon::Chevron))] {
            let span = w - 2.0 * CONTENT_X - CHECK_W - GAP - icon;
            let value = row.readout().unwrap();
            let gap = span - M.width_str(&row.label, size, bold) - M.width_str(value, theme::size::LABEL, VALUE_BOLD != 0);
            assert!(gap >= theme::space::MD - 0.01, "{}: label to value air {gap} < the MD rung", row.label);
        }
    }

    const ES_NOTE: &str = "No se puede cambiar mientras Realzar diálogos o Normalizar volumen esté activado.";

    fn note_table(note: &str) -> TableView {
        let mut t = TableView::new();
        t.set_sections(
            vec![Section::new("S").row(Row::new("Color").value("Blanco")).row(Row::note(note))],
            0,
            false,
        );
        t
    }

    /// **A note wraps inside the row-label column and its row grows to the wrapped lines** — the
    /// Spanish Subtitles note ran off the panel as one clipped line at a fixed `ROW_H`.
    #[test]
    fn a_long_note_wraps_and_its_row_grows_with_the_lines() {
        use plx_base::fontcov::advances::ShippedMeasure as M;
        let t = note_table(ES_NOTE);
        let before = t.measured_height();
        t.fit_notes(620.0, &M);
        let note = &t.sections[0].rows[1];
        assert!(note.note_lines.get() >= 2, "the premise: it does not fit one line of the column");
        let h = note.height_in(t.tall_row_h());
        assert!(h > ROW_H, "note height {h} must grow past ROW_H");
        assert!((t.measured_height() - before - (h - ROW_H)).abs() < 0.01, "layout routes through the same height");
        let short = note_table("Nope.");
        short.fit_notes(620.0, &M);
        assert_eq!(short.sections[0].rows[1].height_in(short.tall_row_h()), ROW_H, "one line keeps the ROW_H measure");
    }

    /// **`fit_report` judges notes**: a wrapped note fits, one unbreakable word wider than the
    /// column does not.
    #[test]
    fn fit_report_covers_note_rows() {
        use plx_base::fontcov::advances::ShippedMeasure as M;
        use crate::ui::fit::HEADROOM;
        let ok = note_table(ES_NOTE).fit_report(620.0, &M, HEADROOM);
        assert!(ok.iter().all(|i| i.role != FitRole::Note), "a wrappable note fits: {ok:?}");
        let word = "Superextraordinariamente".repeat(4);
        let bad = note_table(&word).fit_report(620.0, &M, HEADROOM);
        assert!(bad.iter().any(|i| i.role == FitRole::Note), "an unbreakable overlong word must be reported: {bad:?}");
    }

    /// **Every section after the first is divided from the one above, headed or not.**
    #[test]
    fn a_headerless_section_after_the_first_gets_a_divider() {
        let mut t = TableView::new();
        t.set_sections(
            vec![
                Section::new("A").row(Row::new("a")),
                Section::new("").row(Row::new("b")),
                Section::new("C").row(Row::new("c")),
            ],
            0,
            false,
        );
        let (mut dividers, mut headers) = (0, 0);
        t.walk(|_, gi, _| match gi {
            WALK_DIVIDER => dividers += 1,
            -1 => headers += 1,
            _ => {}
        });
        assert_eq!(headers, 2);
        assert_eq!(dividers, 2, "one hairline per section boundary, headed or not");
    }

    /// **A header sits closer to its rows than to the divider above it**: the dead band under the
    /// caps is shorter than the band above them (half the divider gap plus the cap inset).
    #[test]
    fn a_header_reads_as_belonging_to_its_rows() {
        let above = DIV_H * 0.5 + HEADER_CAP_INSET;
        assert!(HDR_INK_PAD < above, "space under the caps ({HDR_INK_PAD}) must be under the space above ({above})");
        assert!(HDR_INK_PAD >= PILL_INSET * 2.0, "the focused pill of the first row must clear the caps");
    }

    /// **A title band adds the header band plus a section-boundary gap** to the content, and is
    /// part of neither the rows nor the selection.
    #[test]
    fn a_title_adds_a_header_band_and_a_divider_gap() {
        let mut t = TableView::new();
        t.set_sections(vec![Section::new("").row(Row::new("a")).row(Row::new("b"))], 0, false);
        let bare = t.measured_height();
        t.set_title(Some("Style".into()));
        assert_eq!(t.measured_height(), bare + HDR_H + TITLE_GAP + DIV_H);
        let (mut titles, mut dividers, mut first_row_y) = (0, 0, None);
        t.walk(|y, gi, _| match gi {
            WALK_TITLE => titles += 1,
            WALK_DIVIDER => dividers += 1,
            0 => first_row_y = Some(y),
            _ => {}
        });
        assert_eq!((titles, dividers), (1, 1), "the title is followed by the hairline a section boundary has");
        assert_eq!(first_row_y, Some(HDR_H + TITLE_GAP + DIV_H));
        t.set_title(None);
        assert_eq!(t.measured_height(), bare);
    }

    /// **A trailing empty headerless section is nothing**: no divider, no height, no over-scroll
    /// (a `Form` section whose items all fell through stays visible but empty).
    #[test]
    fn a_trailing_empty_section_adds_nothing() {
        let rows = || vec![Section::new("A").row(Row::new("a")), Section::new("B").row(Row::new("b"))];
        let count = |t: &TableView| {
            let mut d = 0;
            t.walk(|_, gi, _| d += (gi == WALK_DIVIDER) as i32);
            d
        };
        let mut bare = TableView::new();
        bare.set_sections(rows(), 0, false);
        let mut t = TableView::new();
        let mut with = rows();
        with.push(Section::new(""));
        t.set_sections(with, 0, false);
        assert_eq!(t.measured_height(), bare.measured_height());
        assert_eq!(count(&t), count(&bare));
        assert_eq!(t.content_h(), bare.content_h());
    }

    /// **A title installed AFTER the sections still finds the pill on its row**: the band moves
    /// every row, so the springs re-snap instead of gliding from the old place.
    #[test]
    fn set_title_after_the_sections_resnaps_the_pill() {
        let mut t = TableView::new();
        t.set_sections(vec![Section::new("").row(Row::new("a")).row(Row::new("b"))], 1, false);
        t.set_title(Some("Style".into()));
        let top = t.row_top(1);
        assert_eq!(t.hl_top.pos, top + PILL_INSET);
        assert_eq!(t.hl_bot.pos, top + t.row_height(1) - PILL_INSET);
        t.set_title(None);
        assert_eq!(t.hl_top.pos, t.row_top(1) + PILL_INSET);
    }

    /// **The title band has a rect that follows the scroll, and no section dims it.**
    #[test]
    fn title_rect_follows_scroll_and_the_title_is_never_dimmed() {
        let frame = Rect::new(10.0, 20.0, 400.0, 300.0);
        let mut t = TableView::new();
        t.set_sections(vec![Section::new("").row(Row::new("a")).dim(true)], 0, false);
        assert_eq!(t.title_rect(frame), None);
        t.set_title(Some("Style".into()));
        let r = t.title_rect(frame).expect("band");
        assert_eq!((r.x, r.y, r.h), (frame.x + SIDE, frame.y + TOP_PAD, HDR_H));
        t.scroll.jump(10.0);
        assert_eq!(t.title_rect(frame).map(|r| r.y), Some(frame.y + TOP_PAD - 10.0));
        t.scroll.jump(HDR_H + 50.0);
        assert_eq!(t.title_rect(frame), None, "scrolled out of the viewport");
        // the dim is the section's alone: title and its hairline keep the page's painter
        let (mut page, mut rows) = (Vec::new(), Vec::new());
        t.walk(|_, gi, si| {
            let o = t.event_painter(Painter::root(), gi, si).opacity();
            if gi == WALK_TITLE || gi == WALK_DIVIDER { page.push(o) } else { rows.push(o) }
        });
        assert_eq!(page, [1.0, 1.0], "title and its hairline");
        assert_eq!(rows, [GROUP_DIM_A], "the dimmed section's row");
    }

    /// **The fit gate reports a title that cannot fit**, at the width the localized text needs.
    #[test]
    fn fit_report_covers_the_title_band() {
        use plx_base::fontcov::advances::ShippedMeasure as M;
        use crate::ui::fit::HEADROOM;
        let mut t = TableView::new();
        t.set_sections(vec![Section::new("").row(Row::new("a"))], 0, false);
        t.set_title(Some("Idioma de los subtitulos disponibles en otros idiomas".into()));
        let narrow = t.fit_report(300.0, &M, HEADROOM);
        assert!(narrow.iter().any(|i| i.role == FitRole::Title), "a long title must be reported: {narrow:?}");
        let wide = t.fit_report(t.menu_panel_width(&M).max(t.measured_width(&M)), &M, HEADROOM);
        assert!(wide.iter().all(|i| i.role != FitRole::Title), "a panel sized from measured_width fits its title: {wide:?}");
    }
}
