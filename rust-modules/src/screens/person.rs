//! The owned person page for restructure phase 7. It preserves the legacy page's measured layout,
//! shelves, biography panel and animation, while navigation and focus are now explicit contracts:
//! the engine is the only cursor, card keys preserve item identity across store reshapes, and page
//! changes leave through `ContentReq` effects. Filmography is a separate modal `Screen`; this page
//! presents it and never imports or stores a sibling screen.
//!
//! **The biography sheet left in phase 10** (`screens::person_bio`, `ContentPanel::Bio`): it is a
//! `Style::Alert` surface on the container tree, so its input, its phase and its teardown are the
//! container's and this page neither intercepts keys for it nor hides it. What stays here is the
//! GATE — the panel is offered exactly when the `MORE` mark is drawn, and both read
//! `bio_is_truncated` over this header's own column width.

use std::ffi::CString;

use crate::person::{Person, NSHELF};
use crate::plex::ServerId;
use crate::pms::PmsMovie;
use crate::stores::person::PersonCmd;
use crate::ui::card_row::{self, CardRow, RowStyle};
use crate::ui::consts::*;
use crate::ui::label::{HAlign, Label, VAlign};
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, GroupId, Handled, InputEvent, InputKind, Key, Leave,
    LogicalState, Machine, Measure, Tick,
};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::screen::{
    Activate, At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusSource, Focusable,
    GroupKind, GroupSpec, HitSource, Hover, Link, Placed, RenderStrategy, Screen, ScreenEvent,
    Seat, Step, Stop,
};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{AmbientWash, Art, PageGround, StatusKind, StatusOverlay};
use crate::ui::{Column, Env, Painter, Rect, ScrollColumn, View};

use super::registry::{
    AppFx, CardIdentity, ContentArg, ContentLike, ContentReq, PageMemory, PersonMemory,
};

// -------------------------------------------------------------------------------------------
// element-key + group-id namespace (module doc: "Element-key namespace")
// -------------------------------------------------------------------------------------------

const HEADER_ELEM: u32 = 0;
const ENTRY_ELEM: u32 = 1;
const FIRST_CARD_ELEM: u32 = 0x1000;

const HEADER_GROUP: GroupId = GroupId(1);
/// `GroupId(0)` — matches the container's own fresh-mount default target (`stack.rs::fresh`),
/// which is what lets this screen mount focused on the entry row (module doc, and the legacy
/// page's own "opens on the Filmography entry, not the header or the first shelf" rule) with no
/// correction of its own, exactly as `screens::login::LoginScreen` reused the same group id for
/// its own single control for the same reason.
const ENTRY_GROUP: GroupId = GroupId(0);
const SHELF_GROUP: [GroupId; NSHELF] = [GroupId(2), GroupId(3)];

/// Where an element key falls in this screen's own row model — the one place the `u32` namespace
/// is decoded, so `on_header`/`focus_pos`'s legacy jobs (deriving "which row" from a struct field)
/// become "which row" derived from the key the engine handed back instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Located {
    Header,
    Entry,
    Shelf(usize, usize),
}

// -------------------------------------------------------------------------------------------
// geometry constants — verbatim from `ui/person.rs` (the port this screen replaces); the values
// are the shipped, photographed design, not a re-derivation
// -------------------------------------------------------------------------------------------

const PORTRAIT_EXP: f32 = 320.0;
const PORTRAIT_BARE: f32 = 220.0;
const PORTRAIT_RES: (std::os::raw::c_int, std::os::raw::c_int) = (300, 300);
const HEADER_TOP: f32 = 96.0;
const BAND_GAP: f32 = theme::space::XL;
const META_GAP: f32 = theme::space::LG;
const LIFE_GAP: f32 = theme::space::SM;
const BIO_GAP: f32 = theme::space::LG;
const SHELF_COUNT_GAP: f32 = theme::space::SM;
const fn text_w_const(d: f32) -> f32 {
    SCR_W - MARGIN_X - (MARGIN_X + d + BAND_GAP)
}
const BIO_W: f32 = text_w_const(PORTRAIT_EXP);
const BIO_LINES: usize = 3;
const BIO_LEAD: f32 = 40.0;
const HL_PAD_X: f32 = 26.0;
const HL_PAD_Y: f32 = 24.0;
const MORE: &std::ffi::CStr = c"MORE";
const BIO_MORE_GAP: f32 = theme::space::LG;
const BAND_GAP_TO_SHELF: f32 = theme::space::XL;
const SHELF_GAP: f32 = UNDER_LABEL_AIR;
const SHELF_LABEL_H: f32 = TITLE_DY + CARD_DY;
const SHELF_STYLE: RowStyle = RowStyle::HOME;
const TOP_MARGIN: f32 = HEADER_TOP;
const BOTTOM_PAD: f32 = crate::ui::consts::MARGIN_Y;

const ENTRY_H: f32 = 60.0;
const ENTRY_OUTER_PAD: f32 = theme::space::MD;
const ENTRY_MARK: f32 = 24.0;
const ENTRY_MARK_INK: (f32, f32) = crate::ui::icons::ink_x(crate::ui::icons::Icon::Chevron);
const ENTRY_MARK_BEARING_L: f32 = ENTRY_MARK * ENTRY_MARK_INK.0;
const ENTRY_MARK_BEARING_R: f32 = ENTRY_MARK * (1.0 - ENTRY_MARK_INK.1);
const ENTRY_CHEVRON_GAP: f32 = theme::space::SM;
const ENTRY_GAP: f32 = theme::space::LG;
const ENTRY_RUN_GAP: f32 = theme::space::XS;

const AMB_HEADER_W: [f32; 4] = [0.10, 0.06, 0.02, 0.03];
const AMB_CARD_W: [f32; 4] = PageGround::CARD_W;

// -------------------------------------------------------------------------------------------
// pure store-shape predicates (ported from `ui/person.rs`, `Scene`-independent already there)
// -------------------------------------------------------------------------------------------

/// The shelf kinds that have content, in flow order, and how many.
fn present(p: &Person) -> ([usize; NSHELF], usize) {
    let mut v = [0usize; NSHELF];
    let mut n = 0;
    for k in 0..NSHELF {
        if !p.shelf(k).is_empty() {
            v[n] = k;
            n += 1;
        }
    }
    (v, n)
}

fn nshelves(p: &Person) -> usize {
    present(p).1
}

/// Is there a real, resolved Filmography entry to enter? (`p.credited` gates it — see
/// [`entry_reachable`] for the PENDING half this alone cannot answer.) Pure over the two facts
/// that decide it, so [`has_entry_of`] is testable with no `Person` in scope — `Person`'s own
/// `credits`/`srcs`/`roster_gen` fields are private to `crate::person`, so a host test outside
/// that module cannot build one by hand; the two existing `#[cfg(test)]` seams it exposes
/// (`install_for_test`/`install_credits_for_test`) both force `credited = true`, so the PENDING
/// half of this predicate can only be exercised through its pure form today.
fn has_entry_of(credited: bool, filmography_total: usize) -> bool {
    credited && filmography_total > 0
}

fn has_entry(p: &Person) -> bool {
    has_entry_of(p.credited, crate::person::filmography_total(p))
}

/// **Should the entry row be OFFERED at all right now** — present or merely still pending an
/// answer? This is `groups()`'s own question, not `has_entry`'s: the legacy `Scene::on_entry` HELD
/// focus on the entry row through the whole credits load (mounting there — module doc — "the one
/// landing spot data arriving underneath cannot move") and only gave it up once the answer was
/// genuinely in. Under the engine this is expressed as "does the entry GROUP exist yet", which the
/// reconcile step (mirroring legacy's `clamp_focus`) evicts focus from the moment this turns false
/// while `has_entry` is also false.
///
/// Mirrors legacy's release condition exactly, inverted: `clamp_focus` released `on_entry` when
/// `(p.credited || p.guid.is_empty()) && !has_entry(p)`, i.e. it stayed held while
/// `!(p.credited || p.guid.is_empty()) || has_entry(p)`.
fn entry_reachable_of(credited: bool, guid_empty: bool, has_entry: bool) -> bool {
    has_entry || !(credited || guid_empty)
}

fn entry_reachable(p: &Person) -> bool {
    entry_reachable_of(p.credited, p.guid.is_empty(), has_entry(p))
}

// -------------------------------------------------------------------------------------------
// header measurement (ported verbatim from `ui/person.rs`; `sc.roles_c`/`sc.life_c` become
// explicit parameters so the pure half stays testable with no `PersonScreen` in scope)
// -------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct HeaderFlow {
    bio_truncated: bool,
    exp_h: f32,
    exp_d: f32,
    portrait_y: f32,
    name_y: f32,
    meta_y: Option<f32>,
    life_y: Option<f32>,
    bio_y: Option<f32>,
    entry_y: Option<f32>,
}

/// Pure over the facts it needs, so it is testable with no `Person`/`PersonScreen` in scope — see
/// [`has_entry_of`]'s doc for why that matters here specifically (the PENDING header state, which
/// this decides the placeholder geometry for, has no live `Person` seam to exercise it through).
fn header_flow(
    pending: bool,
    bio: &str,
    roles_c: &std::ffi::CStr,
    life_c: &std::ffi::CStr,
    has_entry: bool,
    measure: &dyn Measure,
) -> HeaderFlow {
    let mut f = HeaderFlow::default();
    let mut y = measure.cap_h(theme::size::DISPLAY);
    if pending || !roles_c.to_bytes().is_empty() {
        y += META_GAP;
        f.meta_y = Some(y);
        y += measure.cap_h(theme::size::LABEL);
    }
    if pending || !life_c.to_bytes().is_empty() {
        y += if f.meta_y.is_some() {
            LIFE_GAP
        } else {
            META_GAP
        };
        f.life_y = Some(y);
        y += measure.cap_h(theme::size::LABEL);
    }
    if pending || !bio.is_empty() {
        y += BIO_GAP;
        f.bio_y = Some(y);
        y += if pending && bio.is_empty() {
            BIO_LEAD * BIO_LINES as f32
        } else {
            let view = bio_view(bio, 1.0, measure);
            f.bio_truncated = view.truncates(BIO_W);
            view.measure_h(BIO_W)
        };
    }
    if has_entry {
        y += ENTRY_GAP;
        f.entry_y = Some(y);
        y += ENTRY_H;
    }
    let bare = !pending && f.meta_y.is_none() && f.life_y.is_none() && f.bio_y.is_none();
    f.exp_d = if bare { PORTRAIT_BARE } else { PORTRAIT_EXP };
    f.exp_h = y.max(f.exp_d);
    if bare {
        f.portrait_y = (f.exp_h - f.exp_d) * 0.5;
        let ty = (f.exp_h - y) * 0.5;
        f.name_y = ty;
        for v in [&mut f.meta_y, &mut f.life_y, &mut f.bio_y]
            .into_iter()
            .flatten()
        {
            *v += ty;
        }
    }
    f
}

fn bio_view<'a>(bio: &'a str, a: f32, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(
        bio,
        theme::size::BODY,
        theme::with_a(theme::TEXT_READING, a),
    )
    .with_measure(measure)
    .leading(BIO_LEAD)
    .max_lines(BIO_LINES)
    .fade_last(measure.width(MORE, theme::size::BODY, true) + BIO_MORE_GAP)
}

fn text_w(d: f32) -> f32 {
    SCR_W - MARGIN_X - col_x(d)
}

fn col_x(d: f32) -> f32 {
    MARGIN_X + d + BAND_GAP
}

fn cstr_elide(s: &str, w: f32, sz: std::os::raw::c_int, bold: std::os::raw::c_int, measure: &dyn Measure) -> CString {
    if s.is_empty() {
        return CString::default();
    }
    let elided = crate::text::elide_by(s, w, false, |t| measure.width_str(t, sz, bold != 0));
    CString::new(elided).unwrap_or_default()
}

/// Is there more biography than the header shows? Shared by the truncation mark and the (deferred,
/// out-of-scope) bio panel's own gate.
fn bio_is_truncated(p: &Person, measure: &dyn Measure) -> bool {
    !p.bio.is_empty() && bio_view(&p.bio, 1.0, measure).truncates(BIO_W)
}

fn shelf_block_h_at(band: f32) -> f32 {
    SHELF_LABEL_H + CARD_H + band
}

fn reveal_block(cur: f32, top: f32, h: f32, content: f32) -> f32 {
    let lo = top + h - (SCR_H - BOTTOM_PAD);
    let hi = top - TOP_MARGIN;
    card_row::reveal(cur, lo, hi, (content - SCR_H).max(0.0))
}

// -------------------------------------------------------------------------------------------
// the scroll flow — a `Column` view parameterised by the frame's own `focus_child`, since focus
// is the engine's rather than a `Scene` field (module doc)
// -------------------------------------------------------------------------------------------

/// Flow child 0 = the band (header + entry pill); 1.. = the present shelves — mirrors
/// `ui/person.rs`'s `impl Column for Scene`, with the live-vs-destination band split retained as
/// the `settled` flag (`Settled` there, folded into one type here since the only thing that ever
/// differed between them was which `under_band` a shelf reports).
struct Flow<'a> {
    screen: &'a PersonScreen,
    person: &'a Person,
    /// Which flow child holds focus THIS FRAME (0 = band, i+1 = the i'th present shelf), derived
    /// once by the caller from `cx.focus.current` — see [`PersonScreen::flow_child`].
    focus_child: Option<usize>,
    /// The engine's current element, kept only in this stack-lived draw view so the band can tell
    /// its header and entry controls apart without storing a cursor on the screen.
    focus_elem: Option<u32>,
    /// `true` measures every shelf's band at its DESTINATION (open on the focused shelf, closed on
    /// the rest) rather than wherever its spring has it — what a scroll TARGET must be computed
    /// against (`ui/person.rs`'s `Settled`/`scroll_target` doc has the full argument).
    settled: bool,
}

impl Column for Flow<'_> {
    fn len(&self) -> usize {
        1 + nshelves(self.person)
    }
    fn height(&self, i: usize) -> f32 {
        if i == 0 {
            return self.screen.header.exp_h;
        }
        let band = if self.settled {
            card_row::under_band((self.focus_child == Some(i)) as i32 as f32)
        } else {
            let (kinds, n) = present(self.person);
            match kinds[..n].get(i - 1) {
                Some(&kind) => self.screen.shelves[kind].under_band(),
                None => 0.0,
            }
        };
        shelf_block_h_at(band)
    }
    fn gap_before(&self, i: usize) -> f32 {
        if i == 1 {
            BAND_GAP_TO_SHELF
        } else {
            SHELF_GAP
        }
    }
    fn focus_child(&self) -> Option<usize> {
        self.focus_child
    }
    fn draw_child(&self, i: usize, env: &Env, p: Painter, measure: &dyn crate::ui::machine::Measure) {
        debug_assert!(
            !self.settled,
            "the settled flow is a measurement, never a draw"
        );
        if i == 0 {
            self.screen.draw_header(p, self.person, self.focus_elem, measure);
            return;
        }
        let (kinds, n) = present(self.person);
        if let Some(&kind) = kinds[..n].get(i - 1) {
            self.screen
                .draw_shelf(p, env, self.person, kind, self.focus_child == Some(i), measure);
        }
    }
}

fn content_h(col: &ScrollColumn, c: &impl Column) -> f32 {
    let last = c.len().saturating_sub(1);
    col.child_top(c, last) + c.height(last) + BOTTOM_PAD
}

fn scroll_target(col: &ScrollColumn, live: &Flow<'_>, settled: &Flow<'_>) -> f32 {
    let Some(fi) = live.focus_child else {
        return 0.0;
    };
    reveal_block(
        col.scroll.pos,
        col.child_top(settled, fi),
        settled.height(fi),
        content_h(col, settled),
    )
}

/// The ambient wash's target corners — the focused poster's `UltraBlurColors` while a card holds
/// focus, else a faint warm header tint.
fn amb_target(p: &Person, focused: Option<&PmsMovie>) -> [[f32; 4]; 4] {
    match focused.filter(|m| m.has_blur) {
        Some(m) => AmbientWash::keyed(m.blur, AMB_CARD_W),
        None => {
            let _ = p; // kept for symmetry with the legacy signature / future per-person tinting
            AmbientWash::target([theme::WASH_WARM; 4], AMB_HEADER_W)
        }
    }
}

// -------------------------------------------------------------------------------------------
// the stable-key shelf `Focusable`
// -------------------------------------------------------------------------------------------

/// One shelf's `Focusable` geometry. `keys[col]` is stable for the catalog item's identity, so a
/// landing may move an item between shelves without moving the engine cursor off that item.
struct OffsetShelf<'a> {
    row: &'a CardRow,
    keys: Vec<u32>,
    row_y: f32,
    group: GroupId,
    entry: EntryId,
    extent: Rect,
}

impl<'a> OffsetShelf<'a> {
    fn col_of(&self, elem: u32) -> Option<usize> {
        self.keys.iter().position(|&key| key == elem)
    }
}

impl<H: ContentLike> Focusable<H> for OffsetShelf<'_> {
    fn groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: self.extent,
            len: self.keys.len(),
            elem: ElemKind::Card,
        });
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        self.col_of(*key).map(|_| self.group)
    }
    fn neighbour(
        &self,
        key: crate::ui::machine::FocusKey<u32>,
        dir: Dir,
        _cx: &Cx<'_, H>,
    ) -> Step<u32> {
        let Some(i) = self.col_of(key.elem) else {
            return Step::Edge;
        };
        match dir {
            Dir::Left if i > 0 => Step::Move(shelf_key_from(self, i - 1)),
            Dir::Right if i + 1 < self.keys.len() => Step::Move(shelf_key_from(self, i + 1)),
            _ => Step::Edge,
        }
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let i = self.col_of(*key)?;
        let pitch = SHELF_STYLE.w + SHELF_STYLE.gap;
        let rest = card_row::tile_rect(
            i,
            SHELF_STYLE.margin_x,
            pitch,
            self.row.scroll_x(),
            self.row_y,
            (SHELF_STYLE.w, SHELF_STYLE.h),
        );
        let s = match at {
            At::Drawn => self.row.scale(i),
            At::SpringTarget => {
                if self.row.focus() == i as i32 {
                    SHELF_STYLE.focus_scale
                } else {
                    1.0
                }
            }
        };
        Some(Placed {
            rect: rest.scaled(s),
            rest_rect: rest,
            clip: self.extent,
            index: Some(i as u32),
        })
    }
    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        _cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        let i = self.col_of(want.elem).unwrap_or(0);
        shelf_key_from(self, i.min(self.keys.len().saturating_sub(1)))
    }
    fn seat(
        &self,
        _g: GroupId,
        from: Placed,
        _cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        let pitch = SHELF_STYLE.w + SHELF_STYLE.gap;
        let cx_ = from.rect.x + from.rect.w * 0.5;
        let guess = ((cx_ - SHELF_STYLE.margin_x + self.row.scroll_x()) / pitch).max(0.0) as usize;
        let from_i = from.index.map_or(guess, |i| i as usize);
        let i = card_row::column_near_x(
            cx_,
            SHELF_STYLE.margin_x,
            pitch,
            SHELF_STYLE.w,
            self.row.scroll_x(),
            self.keys.len(),
            from_i,
        );
        shelf_key_from(self, i)
    }
}

fn shelf_key_from(s: &OffsetShelf<'_>, col: usize) -> crate::ui::machine::FocusKey<u32> {
    crate::ui::machine::FocusKey {
        entry: s.entry,
        elem: s.keys.get(col).copied().unwrap_or(HEADER_ELEM),
    }
}

// -------------------------------------------------------------------------------------------
// the screen
// -------------------------------------------------------------------------------------------

/// The person / actor page (restructure spec §13, phase 7).
pub(crate) struct PersonScreen {
    entry: EntryId,
    // ---- identity, fixed at construction; re-issued to the store on `Enter` (module doc: "an
    // Enter, fresh or restored, re-opens" — the single-slot `crate::person` store is only ever
    // one person's, so returning to a covered instance must re-claim it) ----
    sid: ServerId,
    key: String,
    guid: String,
    name: String,
    thumb: String,

    // ---- logical state the engine cannot derive ----
    /// Set the moment an explicit D-pad press lands on (or stays on) the header — never by a data
    /// landing that merely leaves the header as the only row. See [`bio_mark_visible`].
    header_marked: bool,
    /// Stable item-key interning. This maps identities to engine elements; it never stores which
    /// element is focused.
    card_keys: Vec<CardIdentity>,
    next_card_elem: u32,
    /// One-shot return hydration. While set, a known engine card key may remain unpublished until
    /// that card's own source answers; the engine remains the sole owner of the key itself.
    return_pending: bool,

    // ---- render cache: animation (never hashed; a spring position is not logical state) ----
    shelves: [CardRow; NSHELF],
    scroll: ScrollColumn,
    amb: PageGround,
    /// Skeleton spinner clock, in ms — cached each tick from [`spin_phase`](Self::spin_phase)'s
    /// `advance`.
    spin_ms: f32,
    /// The underlying clock for [`spin_ms`](Self::spin_ms) (`motion::Phase`, phase 12 D4): reports
    /// `Motion` from inside its own `advance` rather than the raw `+= dt` this used to be, with
    /// `fx.note(Motion)` a separate line further down `tick`.
    spin_phase: crate::ui::motion::Phase,

    // ---- render cache: baked text runs + measured flow, rebuilt only when the store lands ----
    name_c: CString,
    roles_c: CString,
    life_c: CString,
    shelf_count_c: [CString; NSHELF],
    entry_count_c: CString,
    header: HeaderFlow,
    header_dirty: bool,
}

impl LogicalState for PersonScreen {
    fn write(&self, w: &mut Canon) {
        w.u32(self.entry.0)
            .u32(self.sid.raw() as u32)
            .str(&self.key)
            .str(&self.guid)
            .str(&self.name)
            .str(&self.thumb)
            .bool(self.header_marked)
            .bool(self.return_pending)
            .u32(self.next_card_elem)
            .u32(self.card_keys.len() as u32);
        for card in &self.card_keys {
            w.u32(card.sid.raw() as u32).str(&card.rk).u32(card.elem);
        }
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "person sid={} key={} marked={} return_pending={} card_keys={} next_elem={}",
            self.sid.raw(),
            self.key,
            self.header_marked,
            self.return_pending,
            self.card_keys.len(),
            self.next_card_elem
        ));
    }
}

impl PersonScreen {
    pub(crate) const SHAPE: &'static str = "PersonScreen{entry:EntryId,sid:ServerId,key:String,guid:String,name:String,thumb:String,header_marked:bool,return_pending:bool,next_card_elem:u32,card_keys:[{sid:ServerId,rk:String,elem:u32}]}";

    /// Mount a person from the header a cast row (or, later, the Filmography route) handed in —
    /// mirrors `PersonCmd::Open`'s fields exactly. Issues the store command directly; see the
    /// module doc for why an `Enter` (fresh OR restored) does so again rather than assuming the
    /// single-slot store still holds this instance's data.
    pub(crate) fn new(
        entry: EntryId,
        sid: ServerId,
        key: String,
        guid: String,
        name: String,
        thumb: String,
    ) -> Self {
        let mut s = Self {
            entry,
            sid,
            key,
            guid,
            name,
            thumb,
            header_marked: false,
            card_keys: Vec::new(),
            next_card_elem: FIRST_CARD_ELEM,
            return_pending: false,
            shelves: [CardRow::new(); NSHELF],
            scroll: ScrollColumn::new(HEADER_TOP, TOP_MARGIN),
            amb: PageGround::new(),
            spin_ms: 0.0,
            spin_phase: crate::ui::motion::Phase::default(),
            name_c: CString::default(),
            roles_c: CString::default(),
            life_c: CString::default(),
            shelf_count_c: [CString::default(), CString::default()],
            entry_count_c: CString::default(),
            header: HeaderFlow::default(),
            header_dirty: true,
        };
        s.request_store();
        s
    }

    /// Claim the legacy single-slot store for this identity. Restores call this only when another
    /// person actually displaced the slot; render springs and shell scroll deliberately survive.
    fn request_store(&mut self) {
        crate::stores::person::apply(PersonCmd::Open {
            sid: self.sid,
            key: self.key.clone(),
            guid: self.guid.clone(),
            name: self.name.clone(),
            thumb: self.thumb.clone(),
        });
        self.header_dirty = true;
        if let Some(p) = self.person() {
            let k = amb_target(p, None);
            self.amb.jump_target(k);
        }
    }

    fn person(&self) -> Option<&'static Person> {
        crate::person::current().filter(|p| {
            crate::plex::same_item((p.sid, p.key.as_str()), (self.sid, self.key.as_str()))
        })
    }

    fn sync_card_keys(&mut self) {
        let Some(p) = self.person() else {
            return;
        };
        for item in (0..NSHELF).flat_map(|kind| p.shelf(kind).iter()) {
            if self.card_keys.iter().any(|k| {
                crate::plex::same_item((k.sid, k.rk.as_str()), (item.sid, item.rk.as_str()))
            }) {
                continue;
            }
            let elem = self.next_card_elem;
            self.next_card_elem = self
                .next_card_elem
                .checked_add(1)
                .expect("person element-key space exhausted");
            self.card_keys.push(CardIdentity {
                sid: item.sid,
                rk: item.rk.clone(),
                elem,
            });
        }
    }

    pub(crate) fn restore(&mut self, memory: &PersonMemory) {
        // A live covered body can intern a landing after the request-time snapshot. Merge the
        // frozen registry; replacing it would rewind those identities and could reuse an elem.
        for saved in &memory.card_keys {
            if self.card_keys.iter().any(|card| {
                crate::plex::same_item((card.sid, card.rk.as_str()), (saved.sid, saved.rk.as_str()))
            }) {
                continue;
            }
            assert!(
                !self.card_keys.iter().any(|card| card.elem == saved.elem),
                "restored person key collision"
            );
            self.card_keys.push(saved.clone());
        }
        let after_last = self
            .card_keys
            .iter()
            .map(|card| card.elem)
            .max()
            .and_then(|elem| elem.checked_add(1))
            .unwrap_or(FIRST_CARD_ELEM);
        self.next_card_elem = memory.next_card_elem.max(FIRST_CARD_ELEM).max(after_last);
        self.header_marked |= memory.header_marked;
    }

    fn memory(&self) -> PersonMemory {
        PersonMemory {
            card_keys: self.card_keys.clone(),
            next_card_elem: self.next_card_elem,
            header_marked: self.header_marked,
        }
    }

    fn elem_for(&self, item: &PmsMovie) -> Option<u32> {
        self.card_keys
            .iter()
            .find(|k| crate::plex::same_item((k.sid, k.rk.as_str()), (item.sid, item.rk.as_str())))
            .map(|k| k.elem)
    }

    fn card_for_elem(&self, elem: u32) -> Option<&CardIdentity> {
        self.card_keys.iter().find(|card| card.elem == elem)
    }

    /// Retire return hydration only after the engine's saved card has an answer from its own
    /// source. A page-level `landed` is intentionally not consulted: another server may already
    /// have populated a shelf while this card's server is still resolving or retrying.
    fn settle_return_pending<H: ContentLike>(&mut self, cx: &Cx<'_, H>) {
        if !self.return_pending {
            return;
        }
        let Some(elem) = cx
            .focus
            .current
            .filter(|focus| focus.entry == self.entry)
            .map(|focus| focus.elem)
        else {
            return;
        };
        let Some(sid) = self.card_for_elem(elem).map(|card| card.sid) else {
            self.return_pending = false;
            return;
        };
        let Some(person) = self.person() else {
            return;
        };
        if self.locate(person, elem).is_some() || !crate::person::media_resolving(person, sid) {
            self.return_pending = false;
        }
    }

    fn locate(&self, p: &Person, elem: u32) -> Option<Located> {
        if elem == HEADER_ELEM {
            return Some(Located::Header);
        }
        if elem == ENTRY_ELEM {
            return Some(Located::Entry);
        }
        let id = self.card_keys.iter().find(|k| k.elem == elem)?;
        for kind in 0..NSHELF {
            if let Some(col) = p.shelf(kind).iter().position(|m| {
                crate::plex::same_item((m.sid, m.rk.as_str()), (id.sid, id.rk.as_str()))
            }) {
                return Some(Located::Shelf(kind, col));
            }
        }
        None
    }

    fn shelf_key(&self, p: &Person, kind: usize, col: usize) -> crate::ui::machine::FocusKey<u32> {
        let elem = p
            .shelf(kind)
            .get(col)
            .and_then(|m| self.elem_for(m))
            .unwrap_or(HEADER_ELEM);
        crate::ui::machine::FocusKey {
            entry: self.entry,
            elem,
        }
    }

    fn refresh_runs(&mut self, p: &Person, measure: &dyn Measure) {
        let w = text_w(PORTRAIT_EXP);
        self.name_c = cstr_elide(&p.name, w, theme::size::DISPLAY, 1, measure);
        self.roles_c = cstr_elide(&p.roles, w, theme::size::LABEL, 0, measure);

        let mut life: Vec<String> = Vec::new();
        let born = crate::ui::fmt::pretty_date(&p.born, 0);
        if !born.is_empty() {
            life.push(match p.birthplace.is_empty() {
                true => format!("Born {born}"),
                false => format!("Born {born}, {}", p.birthplace),
            });
        }
        let died = crate::ui::fmt::pretty_date(&p.died, 0);
        if !died.is_empty() {
            life.push(format!("Died {died}"));
        }
        self.life_c = cstr_elide(&life.join(" \u{b7} "), w, theme::size::CAPTION, 0, measure);

        for k in 0..NSHELF {
            self.shelf_count_c[k] = match p.total(k) {
                0 => CString::default(),
                n => CString::new(n.to_string()).unwrap_or_default(),
            };
        }
        self.entry_count_c = match crate::person::filmography_total(p) {
            0 => CString::default(),
            n => CString::new(n.to_string()).unwrap_or_default(),
        };
    }

    fn remeasure_header(&mut self, p: &Person, measure: &dyn Measure) {
        self.refresh_runs(p, measure);
        self.header = header_flow(
            crate::person::facts_pending(p),
            &p.bio,
            &self.roles_c,
            &self.life_c,
            has_entry(p),
            measure,
        );
        self.header_dirty = false;
    }

    /// Which flow child (0 = band, i+1 = the i'th present shelf) the given element sits in — the
    /// engine-driven replacement for `on_header`/`focus_pos`'s job of deriving "which row" from a
    /// struct field.
    fn flow_child(&self, p: &Person, elem: Option<u32>) -> Option<usize> {
        match elem.and_then(|e| self.locate(p, e))? {
            Located::Header | Located::Entry => Some(0),
            Located::Shelf(kind, _) => {
                let (kinds, n) = present(p);
                kinds[..n]
                    .iter()
                    .position(|&k| k == kind)
                    .map(|pos| pos + 1)
            }
        }
    }

    fn focused_movie_in<'a>(&self, p: &'a Person, elem: Option<u32>) -> Option<&'a PmsMovie> {
        match elem.and_then(|e| self.locate(p, e))? {
            Located::Shelf(kind, col) => p.shelf(kind).get(col),
            _ => None,
        }
    }

    pub(crate) fn focused_item(
        &self,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
    ) -> Option<&'static PmsMovie> {
        let p = self.person()?;
        self.focused_movie_in(p, focus.filter(|k| k.entry == self.entry).map(|k| k.elem))
    }

    /// The zero-area geometric anchor. Keeping this separate from [`Self::header_rect`] lets DOWN
    /// project to the first shelf tile without making the drawn biography impossible to click.
    fn header_anchor(&self) -> Rect {
        Rect::new(MARGIN_X, HEADER_TOP, 0.0, self.header.exp_h.max(1.0))
    }

    /// The actual drawn/clickable header band. `place` and the hit-map stop both return this exact
    /// rectangle; the Filmography pill overlaps it but is registered later and therefore wins z.
    fn header_rect(&self) -> Rect {
        Rect::new(
            MARGIN_X,
            HEADER_TOP,
            SCR_W - 2.0 * MARGIN_X,
            self.header.exp_h.max(1.0),
        )
    }

    fn entry_rect(&self, p: &Person, measure: &dyn Measure) -> Rect {
        let Some(ey) = self.header.entry_y else {
            return Rect::new(MARGIN_X, HEADER_TOP, 0.0, 1.0);
        };
        let w = entry_w(p, &self.entry_count_c, measure);
        Rect::new(MARGIN_X, HEADER_TOP + ey, w.max(1.0), ENTRY_H)
    }

    /// A shelf's absolute SCREEN-space row-y, given its flow position among the present shelves —
    /// `ui/person.rs`'s `shelf_row_y`, ported: the one vertical geometry the layout and the focus
    /// engine's `place`/`groups` must share.
    fn shelf_row_y(&self, p: &Person, pos: usize) -> f32 {
        self.scroll.child_top(&self.live_flow(p, None), pos + 1) - self.scroll.scroll.pos
            + SHELF_LABEL_H
    }

    fn live_flow<'a>(&'a self, p: &'a Person, focus_elem: Option<u32>) -> Flow<'a> {
        Flow {
            screen: self,
            person: p,
            focus_child: self.flow_child(p, focus_elem),
            focus_elem,
            settled: false,
        }
    }

    fn settled_flow<'a>(&'a self, p: &'a Person, focus_elem: Option<u32>) -> Flow<'a> {
        Flow {
            screen: self,
            person: p,
            focus_child: self.flow_child(p, focus_elem),
            focus_elem,
            settled: true,
        }
    }

    fn offset_shelf<'a>(&'a self, p: &'a Person, kind: usize) -> Option<OffsetShelf<'a>> {
        let keys: Vec<u32> = p
            .shelf(kind)
            .iter()
            .filter_map(|m| self.elem_for(m))
            .collect();
        if keys.is_empty() {
            return None;
        }
        let (kinds, tot) = present(p);
        let pos = kinds[..tot].iter().position(|&k| k == kind)?;
        Some(OffsetShelf {
            row: &self.shelves[kind],
            keys,
            row_y: self.shelf_row_y(p, pos),
            group: SHELF_GROUP[kind],
            entry: self.entry,
            extent: Rect::new(0.0, self.shelf_row_y(p, pos), SCR_W, CARD_H),
        })
    }

    fn tick<H: ContentLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let dt = t.dt();
        let cur = cx.focus.current.map(|k| k.elem);
        let Some(p) = self.person() else {
            // Nothing is drawn while `person()` is `None` (`Screen::draw` returns before touching
            // the skeleton), so there is nothing on screen for the clock to animate — freeze
            // rather than advance-and-report for no visible reason.
            return;
        };
        if self.header_dirty {
            self.remeasure_header(p, cx.measure);
        }

        let focused_movie = self.focused_movie_in(p, cur);
        let k = amb_target(p, focused_movie);
        self.amb.key_target(k, dt);

        let focus_child = self.flow_child(p, cur);
        for kind in 0..NSHELF {
            let n = p.shelf(kind).len();
            let (kinds, tot) = present(p);
            let pos = kinds[..tot].iter().position(|&kk| kk == kind);
            let shelf_focus = pos
                .filter(|&pos| focus_child == Some(pos + 1))
                .and_then(|_| self.locate(p, cur.unwrap_or(u32::MAX)))
                .and_then(|l| match l {
                    Located::Shelf(kk, col) if kk == kind => Some(col),
                    _ => None,
                });
            self.shelves[kind].update(n, shelf_focus, &SHELF_STYLE, dt);
        }

        let live = self.live_flow(p, cur);
        let settled = self.settled_flow(p, cur);
        let want = scroll_target(&self.scroll, &live, &settled);
        self.scroll.scroll.step(want, K_SCROLL, dt);

        let settling =
            (self.scroll.scroll.pos - want).abs() > 0.25 || self.scroll.scroll.vel.abs() > 0.5;
        if crate::person::facts_pending(p) || crate::person::loading() {
            self.spin_ms = self.spin_phase.advance(t, &mut fx.present());
        }
        if settling {
            fx.note(PresentEvent::Motion);
        }
    }

    /// OK on the header: opens the biography panel when the bio is truncated. Mirrors
    /// `header_ok`'s tail (the overlay guard lives in `step`'s `Input` arm now — see the module
    /// doc for why the raw key is intercepted before the engine ever turns it into `Activate`).
    fn activate_header<H: ContentLike>(&mut self, measure: &dyn Measure, fx: &mut Effects<'_, H>) {
        self.header_marked = true;
        let Some(p) = self.person() else {
            return;
        };
        if bio_is_truncated(p, measure) {
            // The GATE is the page's, and stays the page's: the panel exists exactly when the
            // `MORE` mark is drawn, and both read `bio_is_truncated` — which depends on this
            // header's own column width. A surface asked to re-derive it would be how the mark and
            // the sheet came to disagree about whether there is more to read.
            fx.push(crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Panel(
                crate::screens::registry::ContentPanel::Bio,
            ))));
            fx.invalidate(Provenance::Input);
        }
    }

    /// **Can the biography sheet be offered at all?** The page's answer about the page's own
    /// person, and the same predicate the `MORE` mark is drawn from — so the mark and the sheet
    /// cannot disagree about whether there is more to read.
    ///
    /// `pub(crate)` for one caller, `dev::scenarios`' `/tmp/plxnative-bio`: a headless boot presents
    /// the sheet through the same door the OK press uses, and asking the page first is what stops
    /// the trigger opening a panel an interactive press would have refused. `DetailScreen::
    /// tracks_available` is the precedent and the reason.
    /// The scenario has no frame capability; it reads the header's last measured answer and
    /// waits for the next measure after a store invalidation, just as the painted header does.
    pub(crate) fn bio_available(&self) -> bool {
        self.person().is_some() && !self.header_dirty && self.header.bio_truncated
    }

    fn activate_entry<H: ContentLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(p) = self.person() else {
            return;
        };
        if has_entry(p) {
            fx.push(crate::ui::machine::Fx::App(AppFx::Content(
                ContentReq::Present(ContentArg::Filmography {
                    sid: self.sid,
                    key: self.key.clone(),
                }),
            )));
        }
    }

    fn commit_card<H: ContentLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if let Some(m) = self.focused_item(cx.focus.current) {
            fx.push(crate::ui::machine::Fx::App(AppFx::Content(
                ContentReq::Push(ContentArg::Detail {
                    sid: m.sid,
                    rk: m.rk.clone(),
                }),
            )));
        }
    }

    pub(crate) fn focused_rect<H: ContentLike>(
        &self,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
        cx: &Cx<'_, H>,
        at: At,
    ) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.entry)?;
        self.focused_item(Some(key))?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|p| p.rect)
    }

    /// Repaint the focused poster above an item-menu scrim. The current engine key is an explicit
    /// argument so this migration query cannot revive the old screen-local cursor.
    pub(crate) fn redraw_focused<H: ContentLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        focus: Option<crate::ui::machine::FocusKey<u32>>,
    ) {
        let Some(key) = focus.filter(|k| k.entry == self.entry) else {
            return;
        };
        let Some(person) = self.person() else {
            return;
        };
        let Some(Located::Shelf(kind, col)) = self.locate(person, key.elem) else {
            return;
        };
        let Some(item) = person.shelf(kind).get(col) else {
            return;
        };
        let Some(shelf) = self.offset_shelf(person, kind) else {
            return;
        };
        let Some(placed) = Focusable::<H>::place(&shelf, &key.elem, f.cx, At::Drawn) else {
            return;
        };
        let press_scale = if f.press.scale > 0.0 {
            f.press.scale
        } else {
            1.0
        };
        let scale = self.shelves[kind].scale(col) * press_scale;
        let label = card_row::TileLabel::titled(&item.title, person.role(kind, col));
        card_row::draw_focused(
            f.painter.alpha(f.page_alpha),
            Art::Poster(Some(item)),
            placed.rest_rect.scaled(scale),
            scale,
            &SHELF_STYLE,
            item.resume_frac(),
            &label,
            f.measure,
        );
    }

    // ---- draw ----

    fn draw_header(&self, p: Painter, person: &Person, focus_elem: Option<u32>, measure: &dyn crate::ui::machine::Measure) {
        let flow = self.header;
        let d = flow.exp_d;
        let portrait = Rect::new(MARGIN_X, flow.portrait_y, d, d);
        crate::ui::widgets::card(
            p,
            portrait,
            Art::Person {
                sid: person.sid,
                key: &person.thumb,
                res: PORTRAIT_RES,
            },
            d * 0.5,
            false,
            1.0,
            0.0,
        );

        let col_x_ = col_x(d);
        let truncated = bio_is_truncated(person, measure);
        let marked = focus_elem == Some(HEADER_ELEM) && self.header_marked && truncated;
        let mark = flow
            .bio_y
            .filter(|_| marked && !person.bio.is_empty())
            .map(|by| {
                let bio = bio_view(&person.bio, 1.0, measure);
                let bh = bio.measure_h(BIO_W);
                let ink =
                    bio.last_line_cap_y(by, bh) - by + measure.cap_h(theme::size::BODY);
                Rect::new(
                    col_x_ - HL_PAD_X,
                    by - HL_PAD_Y,
                    BIO_W + 2.0 * HL_PAD_X,
                    ink + 2.0 * HL_PAD_Y,
                )
            });
        if let Some(r) = mark {
            crate::ui::widgets::text_block_highlight(p, r);
        }

        Label::new(
            self.name_c.as_ptr(),
            theme::size::DISPLAY,
            theme::TEXT_PRIMARY,
        )
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(col_x_, flow.name_y, 0.0, 0.0));

        let pending = crate::person::facts_pending(person);
        let phase = crate::ui::widgets::skeleton_phase(self.spin_ms as u32);
        for (y, run, sz, w) in [
            (flow.meta_y, &self.roles_c, theme::size::LABEL, 0.42),
            (flow.life_y, &self.life_c, theme::size::CAPTION, 0.68),
        ] {
            let Some(y) = y else { continue };
            if pending {
                let h = measure.cap_h(sz);
                crate::ui::widgets::skeleton_bar(p, Rect::new(col_x_, y, BIO_W * w, h), phase);
            } else {
                Label::new(run.as_ptr(), sz, theme::TEXT_SECONDARY)
                    .v(VAlign::CapTop)
                    .draw(p, Rect::new(col_x_, y, 0.0, 0.0));
            }
        }
        if pending {
            if let Some(by) = flow.bio_y {
                let h = measure.cap_h(theme::size::BODY);
                for (i, w) in [1.0, 1.0, 0.58].into_iter().enumerate() {
                    let ly = by + i as f32 * BIO_LEAD;
                    crate::ui::widgets::skeleton_bar(p, Rect::new(col_x_, ly, BIO_W * w, h), phase);
                }
            }
        } else if let Some(by) = flow.bio_y {
            let bio = bio_view(&person.bio, 1.0, measure);
            let bh = bio.measure_h(BIO_W);
            bio.draw(p, Rect::new(col_x_, by, BIO_W, 0.0));
            if truncated {
                Label::new(
                    MORE.as_ptr(),
                    theme::size::BODY,
                    match mark.is_some() {
                        true => theme::TEXT_SECONDARY,
                        false => theme::TEXT_TERTIARY,
                    },
                )
                .bold()
                .h(HAlign::Right)
                .v(VAlign::CapTop)
                .draw(
                    p,
                    Rect::new(col_x_, bio.last_line_cap_y(by, bh), BIO_W, 0.0),
                );
            }
        }
        if let Some(y) = flow.entry_y {
            self.draw_entry(p, person, MARGIN_X, y, focus_elem == Some(ENTRY_ELEM), measure);
        }
    }

    fn draw_shelf(&self, p: Painter, _env: &Env, person: &Person, kind: usize, focused: bool, measure: &dyn crate::ui::machine::Measure) {
        let items = person.shelf(kind);
        let row = &self.shelves[kind];
        let cur_col = if focused { row.focus() } else { -1 };
        let hy = -row.lift();
        Label::new(
            SHELF_TITLE[kind].as_ptr(),
            theme::size::HEADLINE,
            theme::TEXT_HEADING,
        )
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(MARGIN_X, hy, SCR_W, 0.0));
        if !self.shelf_count_c[kind].as_bytes().is_empty() {
            let tw = measure.width(SHELF_TITLE[kind], theme::size::HEADLINE, true);
            Label::new(
                self.shelf_count_c[kind].as_ptr(),
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
            )
            .v(VAlign::Baseline)
            .draw(
                p,
                Rect::new(
                    MARGIN_X + tw + SHELF_COUNT_GAP,
                    hy,
                    0.0,
                    measure.cap_h(theme::size::HEADLINE),
                ),
            );
        }
        let pitch = SHELF_STYLE.w + SHELF_STYLE.gap;
        card_row::strip(
            p,
            row,
            items.len(),
            cur_col,
            SHELF_LABEL_H,
            (SHELF_STYLE.w, SHELF_STYLE.h),
            pitch,
            &SHELF_STYLE,
            SCR_W,
            |i| Art::Poster(items.get(i)),
            |i| items.get(i).and_then(|m| m.resume_frac()),
            |i| match items.get(i) {
                Some(m) => card_row::TileLabel::titled(&m.title, person.role(kind, i)),
                None => card_row::TileLabel::default(),
            },
            |_, _, _, _| {},
            measure,
        );
    }

    fn draw_shelf_state(&self, p: Painter, env: &Env, person: &Person) {
        if present(person).1 > 0 {
            return;
        }
        let y = self.scroll.child_top(&self.live_flow(person, None), 1) - self.scroll.scroll.pos;
        let band = Rect::new(0.0, y, SCR_W, CARD_H);
        if crate::person::loading() {
            let phase = crate::ui::widgets::skeleton_phase(self.spin_ms as u32);
            crate::ui::widgets::skeleton_bar(
                p,
                Rect::new(MARGIN_X, y + TITLE_DY - 32.0, 214.0, 32.0),
                phase,
            );
            let cy = y + TITLE_DY + CARD_DY;
            for i in 0..5 {
                let cx_ = MARGIN_X + i as f32 * (CARD_W + GAP);
                crate::ui::widgets::skeleton_sheet(
                    p,
                    Rect::new(cx_, cy, CARD_W, CARD_H),
                    theme::CARD_RING_RAD,
                    phase,
                );
            }
            return;
        }
        StatusOverlay::new(
            band,
            c"Nothing from this person is in your libraries",
            StatusKind::Empty,
        )
        .draw(env, p);
    }

    fn draw_entry(&self, p: Painter, person: &Person, x: f32, y: f32, focused: bool, measure: &dyn Measure) {
        let w = entry_w(person, &self.entry_count_c, measure);
        let e = if focused {
            crate::ui::widgets::CTRL_FOCUS_SCALE
        } else {
            1.0
        };
        let (pw, ph) = (w * e, ENTRY_H * e);
        let pill = Rect::new(x, y - (ph - ENTRY_H) * 0.5, pw, ph);
        crate::ui::widgets::draw_control_face(
            p,
            pill,
            if focused {
                crate::ui::ACCENT
            } else {
                theme::CONTROL_IDLE_FILL
            },
            focused,
            crate::ui::widgets::ControlGround::Keyed,
        );
        let (ink, count_ink) = if focused {
            (crate::ui::ACCENT_INK, theme::ROW_VALUE_INK_ON)
        } else {
            (theme::TEXT_PRIMARY, theme::TEXT_TERTIARY)
        };
        let cy = pill.y + pill.h * 0.5;
        let mut rx = x + entry_run_x(w, e);
        rx += Label::new(c"Filmography".as_ptr(), theme::size::LABEL, ink)
            .bold()
            .v(VAlign::Middle)
            .draw(p, Rect::new(rx, cy, 0.0, 0.0));
        if !self.entry_count_c.as_bytes().is_empty() {
            rx += ENTRY_RUN_GAP;
            rx += Label::new(
                c"\u{b7}".as_ptr(),
                theme::size::LABEL,
                theme::TEXT_SEPARATOR,
            )
            .v(VAlign::Middle)
            .draw(p, Rect::new(rx, cy, 0.0, 0.0));
            rx += ENTRY_RUN_GAP;
            rx += Label::new(self.entry_count_c.as_ptr(), theme::size::LABEL, count_ink)
                .v(VAlign::Middle)
                .draw(p, Rect::new(rx, cy, 0.0, 0.0));
        }
        rx += ENTRY_CHEVRON_GAP - ENTRY_MARK_BEARING_L;
        crate::ui::icons::draw(
            p,
            crate::ui::icons::Icon::Chevron,
            Rect::new(rx, cy - ENTRY_MARK * 0.5, ENTRY_MARK, ENTRY_MARK),
            ink,
        );
        let _ = person;
    }
}

/// Shelves, in flow order. Kind 0 = Movies, 1 = Shows.
const SHELF_TITLE: [&std::ffi::CStr; NSHELF] = [c"Movies", c"Shows"];

/// The Filmography entry pill's width, sized to its own runs.
fn entry_w(_p: &Person, entry_count_c: &std::ffi::CStr, m: &dyn Measure) -> f32 {
    let label = m.width(c"Filmography", theme::size::LABEL, true);
    let count = if entry_count_c.to_bytes().is_empty() {
        0.0
    } else {
        ENTRY_RUN_GAP
            + m.width(c"\u{b7}", theme::size::LABEL, false)
            + ENTRY_RUN_GAP
            + m.width(entry_count_c, theme::size::LABEL, false)
    };
    2.0 * ENTRY_OUTER_PAD + label + count + ENTRY_CHEVRON_GAP + ENTRY_MARK
        - ENTRY_MARK_BEARING_L
        - ENTRY_MARK_BEARING_R
}

fn entry_run_x(w: f32, e: f32) -> f32 {
    (w * e - (w - 2.0 * ENTRY_OUTER_PAD)) * 0.5
}

// -------------------------------------------------------------------------------------------
// Focusable / Machine / Screen
// -------------------------------------------------------------------------------------------

impl<H: ContentLike> Focusable<H> for PersonScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let Some(p) = self.person() else {
            return;
        };
        out.push(GroupSpec {
            id: HEADER_GROUP,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::VERTICAL,
            edge: [
                EdgeRule::Stop,
                EdgeRule::Geometric,
                EdgeRule::Stop,
                EdgeRule::Stop,
            ],
            extent: self.header_anchor(),
            len: 1,
            elem: ElemKind::Bare,
        });
        if entry_reachable(p) {
            let down = if nshelves(p) > 0 {
                EdgeRule::Geometric
            } else {
                EdgeRule::Stop
            };
            out.push(GroupSpec {
                id: ENTRY_GROUP,
                kind: GroupKind::Free,
                seat: Seat::First,
                reachable: AxisMask::VERTICAL,
                edge: [EdgeRule::Geometric, down, EdgeRule::Stop, EdgeRule::Stop],
                extent: self.entry_rect(p, cx.measure),
                len: 1,
                elem: ElemKind::Bare,
            });
        }
        for kind in 0..NSHELF {
            if let Some(shelf) = self.offset_shelf(p, kind) {
                Focusable::<H>::groups(&shelf, cx, out);
            }
        }
    }

    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        let p = self.person()?;
        match self.locate(p, *key)? {
            Located::Header => Some(HEADER_GROUP),
            Located::Entry => entry_reachable(p).then_some(ENTRY_GROUP),
            Located::Shelf(kind, _) => self
                .offset_shelf(p, kind)
                .and_then(|s| Focusable::<H>::group_of(&s, key, cx)),
        }
    }

    fn neighbour(
        &self,
        key: crate::ui::machine::FocusKey<u32>,
        dir: Dir,
        cx: &Cx<'_, H>,
    ) -> Step<u32> {
        let Some(p) = self.person() else {
            return Step::Edge;
        };
        match self.locate(p, key.elem) {
            Some(Located::Shelf(kind, _)) => self
                .offset_shelf(p, kind)
                .map(|s| Focusable::<H>::neighbour(&s, key, dir, cx))
                .unwrap_or(Step::Edge),
            _ => Step::Edge,
        }
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        let p = self.person()?;
        match self.locate(p, *key)? {
            Located::Header => {
                let r = self.header_rect();
                Some(Placed {
                    rect: r,
                    rest_rect: r,
                    clip: Rect::FULL,
                    index: Some(0),
                })
            }
            Located::Entry => {
                if !entry_reachable(p) {
                    return None;
                }
                let r = self.entry_rect(p, cx.measure);
                Some(Placed {
                    rect: r,
                    rest_rect: r,
                    clip: Rect::FULL,
                    index: Some(0),
                })
            }
            Located::Shelf(kind, _) => self
                .offset_shelf(p, kind)
                .and_then(|s| Focusable::<H>::place(&s, key, cx, at)),
        }
    }

    /// Keep the focus inside whatever the store currently holds — `ui/person.rs`'s `clamp_focus`,
    /// run every frame by the engine (§7.3 step 6) rather than only after a landing, and made pure
    /// by making the engine element itself a stable item identity rather than storing a cursor.
    fn reconcile(
        &self,
        want: crate::ui::machine::FocusKey<u32>,
        _cx: &Cx<'_, H>,
    ) -> crate::ui::machine::FocusKey<u32> {
        let header_key = crate::ui::machine::FocusKey {
            entry: self.entry,
            elem: HEADER_ELEM,
        };
        let returning_card = if self.return_pending {
            self.card_for_elem(want.elem)
        } else {
            None
        };
        let Some(p) = self.person() else {
            return if returning_card.is_some() {
                want
            } else {
                header_key
            };
        };
        match self.locate(p, want.elem) {
            Some(Located::Header) => crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: HEADER_ELEM,
            },
            Some(Located::Entry) => {
                if entry_reachable(p) {
                    crate::ui::machine::FocusKey {
                        entry: self.entry,
                        elem: ENTRY_ELEM,
                    }
                } else {
                    self.fallback(p)
                }
            }
            Some(Located::Shelf(_, _)) => crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: want.elem,
            },
            None if returning_card
                .is_some_and(|card| crate::person::media_resolving(p, card.sid)) =>
            {
                want
            }
            None if self.card_for_elem(want.elem).is_some() => self.fallback(p),
            None => header_key,
        }
    }

    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        let Some(p) = self.person() else {
            return crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: HEADER_ELEM,
            };
        };
        if g == HEADER_GROUP {
            return crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: HEADER_ELEM,
            };
        }
        if g == ENTRY_GROUP {
            return crate::ui::machine::FocusKey {
                entry: self.entry,
                elem: ENTRY_ELEM,
            };
        }
        for kind in 0..NSHELF {
            if SHELF_GROUP[kind] == g {
                return self
                    .offset_shelf(p, kind)
                    .map(|s| Focusable::<H>::seat(&s, g, from, cx))
                    .unwrap_or(crate::ui::machine::FocusKey {
                        entry: self.entry,
                        elem: HEADER_ELEM,
                    });
            }
        }
        crate::ui::machine::FocusKey {
            entry: self.entry,
            elem: HEADER_ELEM,
        }
    }
}

impl PersonScreen {
    /// The entry-row-gone-under-focus fallback: the first PRESENT shelf's own head, else the
    /// header. Mirrors `clamp_focus`'s observed behaviour exactly (see the module doc's worked
    /// trace against `a_vanishing_entry_row_hands_its_focus_back`) — legacy's own comment claimed
    /// "the last shelf", which the code it sat above never actually did; this states what the code
    /// does, not what the comment said it did.
    fn fallback(&self, p: &Person) -> crate::ui::machine::FocusKey<u32> {
        for kind in 0..NSHELF {
            if !p.shelf(kind).is_empty() {
                return self.shelf_key(p, kind, 0);
            }
        }
        crate::ui::machine::FocusKey {
            entry: self.entry,
            elem: HEADER_ELEM,
        }
    }
}

impl<H: ContentLike> Machine<H> for PersonScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Person(memory)) => {
                self.restore(memory);
                self.return_pending = true;
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx, fx);
                Handled::Yes
            }
            ScreenEvent::Enter(Enter::Restored) => {
                if self.person().is_none() {
                    self.request_store();
                    self.sync_card_keys();
                    fx.invalidate(Provenance::Nav);
                }
                self.settle_return_pending(cx);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, By::Dir | By::Pointer)
                    || self
                        .person()
                        .is_some_and(|person| self.locate(person, to.elem).is_some())
                {
                    self.return_pending = false;
                }
                if matches!(
                    self.person().and_then(|p| self.locate(p, to.elem)),
                    Some(Located::Header)
                ) && matches!(by, By::Dir | By::Pointer)
                {
                    self.header_marked = true;
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                match self.person().and_then(|p| self.locate(p, *e)) {
                    Some(Located::Header) => self.activate_header(cx.measure, fx),
                    Some(Located::Entry) => self.activate_entry(fx),
                    _ => {}
                }
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                self.commit_card(cx, fx);
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => {
                if self.focused_item(cx.focus.current).is_some() {
                    fx.push(crate::ui::machine::Fx::App(AppFx::Content(
                        ContentReq::ItemMenu,
                    )));
                    return Handled::Yes;
                }
                Handled::No
            }
            ScreenEvent::Input(InputEvent {
                kind:
                    InputKind::Key {
                        key,
                        edge: Edge::Down,
                        ..
                    },
                ..
            }) => {
                if matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) {
                    self.return_pending = false;
                }
                if *key == Key::Back {
                    fx.push(crate::ui::machine::Fx::App(AppFx::Content(
                        ContentReq::Back,
                    )));
                    return Handled::Yes;
                }
                Handled::No
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Click { .. },
                ..
            }) => {
                self.return_pending = false;
                Handled::No
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Pointer { .. },
                ..
            }) => {
                Handled::No
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == crate::stores::StoreId::Person.ord() => {
                self.header_dirty = true;
                self.sync_card_keys();
                self.settle_return_pending(cx);
                Handled::Yes
            }
            ScreenEvent::WillLeave(Leave::ForGood) | ScreenEvent::Unmount => {
                if self.person().is_some() {
                    crate::stores::person::apply(PersonCmd::Close);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike> Screen<H> for PersonScreen {
    fn name(&self) -> &'static str {
        // Filmography deliberately returns the same word: the manifest records that opaque modal
        // as the Person route with a separate `filmography=1` state bit.
        super::registry::word::PERSON
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut crate::ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        let cur = f.focus.current.map(|k| k.elem);
        let env = Env::inert();
        let Some(person) = self.person() else {
            return;
        };
        self.amb.draw(p, Rect::FULL);
        let col = self.scroll;
        let live = self.live_flow(person, cur);
        col.draw(&live, &env, p, f.measure);
        self.draw_shelf_state(p, &env, person);

        self.record_stops(f, person, cur);
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
    fn covered_surfaces_ready(&self) -> bool {
        self.person().is_some_and(|person| person.credited) && !crate::person::loading()
    }
    fn links(&self, out: &mut Vec<Link>) {
        let Some(person) = self.person() else {
            return;
        };
        let (kinds, n) = present(person);
        if entry_reachable(person) {
            out.push(Link {
                from: HEADER_GROUP,
                dir: Dir::Down,
                to: ENTRY_GROUP,
            });
            out.push(Link {
                from: ENTRY_GROUP,
                dir: Dir::Up,
                to: HEADER_GROUP,
            });
            if let Some(&first) = kinds[..n].first() {
                out.push(Link {
                    from: ENTRY_GROUP,
                    dir: Dir::Down,
                    to: SHELF_GROUP[first],
                });
                out.push(Link {
                    from: SHELF_GROUP[first],
                    dir: Dir::Up,
                    to: ENTRY_GROUP,
                });
            }
        } else if let Some(&first) = kinds[..n].first() {
            out.push(Link {
                from: HEADER_GROUP,
                dir: Dir::Down,
                to: SHELF_GROUP[first],
            });
            out.push(Link {
                from: SHELF_GROUP[first],
                dir: Dir::Up,
                to: HEADER_GROUP,
            });
        }
        for pair in kinds[..n].windows(2) {
            out.push(Link {
                from: SHELF_GROUP[pair[0]],
                dir: Dir::Down,
                to: SHELF_GROUP[pair[1]],
            });
            out.push(Link {
                from: SHELF_GROUP[pair[1]],
                dir: Dir::Up,
                to: SHELF_GROUP[pair[0]],
            });
        }
    }
    fn memory_at(&self, _focus: Option<crate::ui::machine::FocusKey<u32>>) -> PageMemory {
        PageMemory::Person(self.memory())
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl PersonScreen {
    /// Register this frame's hit-map stops for the header/entry Bare rows — the shelves' own
    /// stops are the engine's ordinary `Card` hit-testing over [`OffsetShelf::place`], which needs
    /// no separate registration; only the two Bare rows (no drawn tile the strip already stops)
    /// need one, mirroring `screens::login::LoginScreen::draw_readout`'s single `f.stop(...)`.
    fn record_stops<H: ContentLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        person: &Person,
        cur: Option<u32>,
    ) {
        let hp = f.painter;
        f.stop(
            hp,
            Stop {
                key: crate::ui::machine::FocusKey {
                    entry: self.entry,
                    elem: HEADER_ELEM,
                },
                rect: self.header_rect(),
                rest_rect: self.header_rect(),
                clip: Rect::FULL,
                hover: Hover::Focus,
                activate: Activate::Direct,
            },
        );
        if entry_reachable(person) {
            let r = self.entry_rect(person, f.measure);
            f.stop(
                hp,
                Stop {
                    key: crate::ui::machine::FocusKey {
                        entry: self.entry,
                        elem: ENTRY_ELEM,
                    },
                    rect: r,
                    rest_rect: r,
                    clip: Rect::FULL,
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
        for kind in 0..NSHELF {
            let Some(shelf) = self.offset_shelf(person, kind) else {
                continue;
            };
            for elem in shelf.keys.iter().copied() {
                let Some(placed) = Focusable::<H>::place(&shelf, &elem, f.cx, At::Drawn) else {
                    continue;
                };
                f.stop(
                    hp,
                    Stop {
                        key: crate::ui::machine::FocusKey {
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
        let _ = cur;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{FocusRead, Host, InputOwner, PressRead, Tick};

    struct PersonHost;

    impl Host for PersonHost {
        type Arg = super::super::family::SettingsPage;
        type Fx = AppFx;
        type Msg = super::super::registry::AppMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = super::super::family::NoInit;
        type Memory = PageMemory;
    }

    fn item(rk: &str) -> PmsMovie {
        item_on(ServerId::UNSET, rk)
    }

    fn item_on(sid: ServerId, rk: &str) -> PmsMovie {
        PmsMovie {
            sid,
            rk: rk.to_string(),
            ..Default::default()
        }
    }

    fn cx(m: &dyn Measure) -> Cx<'_, PersonHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    fn cx_at(m: &FixtureMeasure, focus: crate::ui::machine::FocusKey<u32>) -> Cx<'_, PersonHost> {
        Cx {
            focus: FocusRead {
                current: Some(focus),
            ..Default::default() },
            ..cx(m)
        }
    }

    /// Mounts through [`PersonScreen::new`] (which issues the real `PersonCmd::Open`) and then
    /// seeds the shelves the way a landing does — `install_for_test` ALSO settles `credited =
    /// true`, which is why every test below that wants a genuinely PENDING entry row reasons about
    /// [`entry_reachable_of`] directly instead (see that function's own doc for why no test seam
    /// can force `credited` back to `false` on a live `Person` from outside `crate::person`).
    fn seed(movies: usize, shows: usize) -> PersonScreen {
        let mut s = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        crate::person::install_for_test(
            (0..movies).map(|i| item(&format!("m{i}"))).collect(),
            (0..shows).map(|i| item(&format!("s{i}"))).collect(),
        );
        s.sync_card_keys();
        s
    }

    fn focus_of(s: &PersonScreen, kind: usize, col: usize) -> crate::ui::machine::FocusKey<u32> {
        s.shelf_key(s.person().unwrap(), kind, col)
    }

    #[test]
    #[cfg(feature = "devtriggers")]
    fn populated_person_geometry_uses_recorded_metrics() {
        let _serial = crate::testlock::serial();
        let path = crate::paths::in_runtime_dir("plxnative-personbio");
        assert!(!path.exists(), "this test needs an isolated runtime root");
        std::fs::write(&path, "A populated biography whose words must pass through the recorded measurement capability. ".repeat(60)).unwrap();
        let mut s = seed(3, 2);
        crate::person::install_credits_for_test(&[("Actor", 9)]);
        std::fs::remove_file(path).unwrap();
        assert!(!s.person().unwrap().bio.is_empty());
        crate::ui::rec::assert_measured_geometry(|measure| {
            s.remeasure_header(s.person().unwrap(), measure);
            let context = cx(measure);
            let mut groups = Vec::new();
            Focusable::<PersonHost>::groups(&s, &context, &mut groups);
            assert_eq!(groups.len(), 4);
            let mut bits = Vec::new();
            for g in groups {
                bits.extend([g.extent.x, g.extent.y, g.extent.w, g.extent.h].map(f32::to_bits));
            }
            for key in [HEADER_ELEM, ENTRY_ELEM, focus_of(&s, 0, 0).elem, focus_of(&s, 1, 0).elem] {
                for at in [At::Drawn, At::SpringTarget] {
                    let p = Focusable::<PersonHost>::place(&s, &key, &context, at).unwrap();
                    bits.extend([p.rect.x, p.rect.y, p.rect.w, p.rect.h].map(f32::to_bits));
                }
            }
            bits
        });
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// **The frozen-animator regression class, closed for the header skeleton's spinner (phase 12
    /// D4).** `spin_ms` used to be a raw `+= dt` accumulator with a separate, easy-to-forget
    /// `fx.note(Motion)` a few lines below it. Now it is `motion::Phase`, which reports from
    /// inside its own `advance`. A screen fresh off `PersonScreen::new` (no `install_for_test`,
    /// unlike `seed`) has `person()` answering `Some` with `profiled`/`profile_tried` both false —
    /// `facts_pending` — which is exactly the header-skeleton state the spinner animates, so this
    /// drives it through the real `Machine::step` `Tick` path.
    #[test]
    fn the_header_skeleton_spinner_reports_motion_while_facts_are_pending() {
        let _serial = crate::testlock::serial();
        let mut s = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        let p = crate::person::current().expect("Open seeds a pending Person synchronously");
        assert!(
            crate::person::facts_pending(p),
            "a fresh mount must start pending, or this test is not exercising the skeleton clock"
        );
        let m = FixtureMeasure;
        let cxv = cx(&m);
        let mut present = crate::ui::present::Present::new();
        let _ = present.take(0);
        let mut buf: Vec<crate::ui::machine::Stamped<PersonHost>> = Vec::new();
        for ms in [16, 32, 48] {
            let mut fx = Effects::new(
                &mut buf,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            let ev = ScreenEvent::Tick(Tick { ms, dt_us: 16_667 });
            Machine::<PersonHost>::step(&mut s, &ev, &cxv, &mut fx);
            assert!(
                present.take(ms),
                "a pending header skeleton must present every frame it is on screen (ms={ms})"
            );
        }
        crate::stores::person::apply(PersonCmd::Close);
    }

    /// **The mount-on-entry-pill rule's PENDING half** (module doc, point 2 of `ui/person.rs`'s
    /// own doc): with no filmography answered yet but a guid to ask plex.tv with,
    /// `entry_reachable_of` holds the entry group open even though `has_entry_of` is false — which
    /// is exactly what lets a fresh mount's default `ContainerGroup(GroupId(0))` target
    /// (`ENTRY_GROUP`) have somewhere to land before credits have answered at all.
    #[test]
    fn a_fresh_mount_holds_the_entry_group_pending_credits() {
        assert!(
            entry_reachable_of(false, false, false),
            "no credits answer yet, but a guid to ask plex.tv with — the hold must not release early"
        );
    }

    /// Once credits settle with nothing found, the entry group releases and `reconcile` hands
    /// focus to the first present shelf — `clamp_focus`'s `p.credited || p.guid.is_empty()` gate.
    #[test]
    fn the_entry_group_releases_once_credits_settle_with_nothing_found() {
        let _serial = crate::testlock::serial();
        let mut s = seed(1, 0);
        let p = crate::person::current().unwrap();
        assert!(!has_entry(p), "no credits were installed");
        assert!(
            !entry_reachable(p),
            "credited=true (install_for_test) with zero total: settled and empty"
        );
        let m = FixtureMeasure;
        let want = crate::ui::machine::FocusKey {
            entry: EntryId(0),
            elem: ENTRY_ELEM,
        };
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m));
        assert_eq!(
            got,
            focus_of(&s, 0, 0),
            "falls to the first present shelf's own head"
        );
        let _ = &mut s;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// **`focused_item`/`focused_target` split** — mining `ui/person.rs`'s own regression: the
    /// header/entry rows hold no card at all, so a shelf tile's identity is the only thing OK
    /// should ever navigate to.
    #[test]
    fn focused_movie_answers_only_for_a_shelf_row() {
        let _serial = crate::testlock::serial();
        let mut s = seed(2, 0);
        assert!(s
            .focused_item(Some(crate::ui::machine::FocusKey {
                entry: s.entry,
                elem: HEADER_ELEM
            }))
            .is_none());
        assert!(s
            .focused_item(Some(crate::ui::machine::FocusKey {
                entry: s.entry,
                elem: ENTRY_ELEM
            }))
            .is_none());
        assert_eq!(
            s.focused_item(Some(focus_of(&s, 0, 0)))
                .map(|m| m.rk.as_str()),
            Some("m0")
        );
        assert_eq!(
            s.focused_item(Some(focus_of(&s, 0, 1)))
                .map(|m| m.rk.as_str()),
            Some("m1")
        );
        let _ = &mut s;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// A shelf that has vanished under the focus re-seats by IDENTITY first (the merge-redivide
    /// case), and only falls back to a plain index clamp when no identity is held — the two-step
    /// order `reconcile` must keep (module doc's worked trace).
    #[test]
    fn reconcile_reseats_by_identity_before_falling_back_to_index_clamp() {
        let _serial = crate::testlock::serial();
        let mut s = seed(4, 0);
        let want = focus_of(&s, 0, 2);
        // the row is rebuilt with two items inserted ahead — "m2" is now at index 4
        let rebuilt: Vec<PmsMovie> = ["x0", "x1", "m0", "m1", "m2", "m3"]
            .iter()
            .map(|rk| item(rk))
            .collect();
        crate::person::install_for_test(rebuilt, Vec::new());
        let m = FixtureMeasure;
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m));
        assert_eq!(
            got, want,
            "the engine key follows the card without a screen-owned cursor"
        );
        assert_eq!(
            s.locate(s.person().unwrap(), got.elem),
            Some(Located::Shelf(0, 4))
        );

        // the identity is gone entirely: falls back to clamping the SAME kind's own bounds
        crate::person::install_for_test(vec![item("only")], Vec::new());
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m));
        assert_eq!(got, focus_of(&s, 0, 0));
        let _ = &mut s;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// A shelf that disappears ENTIRELY (not merely shrinks) hands focus to the other present
    /// shelf, never leaving it pointed at a kind with nothing in it.
    #[test]
    fn a_vanished_shelf_kind_falls_back_to_the_other_present_shelf() {
        let _serial = crate::testlock::serial();
        let mut s = seed(2, 3);
        let want = focus_of(&s, 1, 2);
        crate::person::install_for_test(vec![item("m0")], Vec::new()); // shows vanished
        let m = FixtureMeasure;
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m));
        assert_eq!(
            got,
            focus_of(&s, 0, 0),
            "movies is the only present shelf left"
        );
        let _ = &mut s;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// **Header pending vs answered-with-nothing vs answered-fully** — `header_flow`'s three
    /// states, mined from `ui/person.rs`'s own module doc ("Every header line below the name is
    /// optional… a header line is drawn only when it has content").
    #[test]
    fn header_flow_distinguishes_pending_from_answered_empty_from_answered_full() {
        let m = FixtureMeasure;
        let empty = std::ffi::CString::default();
        // answered, with nothing: no reserved space for the meta/life/bio lines at all.
        let flow = header_flow(false, "", &empty, &empty, false, &m);
        assert!(flow.meta_y.is_none() && flow.life_y.is_none() && flow.bio_y.is_none());

        // pending: not yet asked at all — reserves the full placeholder stack even though every
        // cached run is still empty.
        let pending_flow = header_flow(true, "", &empty, &empty, false, &m);
        assert!(
            pending_flow.meta_y.is_some()
                && pending_flow.life_y.is_some()
                && pending_flow.bio_y.is_some()
        );
        assert!(
            pending_flow.exp_h > flow.exp_h,
            "the placeholder reserves real room, not the bare header's"
        );

        // answered, WITH content: the runs actually drive the reserved space.
        let roles = std::ffi::CString::new("Actor").unwrap();
        let full_flow = header_flow(false, "", &roles, &empty, false, &m);
        assert!(full_flow.meta_y.is_some());
        assert!(full_flow.life_y.is_none(), "no life facts were given");
    }

    /// The Filmography entry pill's group vanishes the instant the header releases it — the two
    /// must never both answer "present" (which would let focus land on nothing drawn) nor both
    /// answer "absent" while data is still pending (which would strand a fresh mount nowhere).
    #[test]
    fn entry_reachable_matches_has_entry_once_credits_settle() {
        assert!(
            entry_reachable_of(false, false, false),
            "pending: held open"
        );
        assert!(
            !entry_reachable_of(true, false, false),
            "settled with nothing: released"
        );
        assert!(
            entry_reachable_of(true, false, true),
            "settled WITH something: reachable via has_entry"
        );
    }

    /// A page with NO shelves at all still walks header ↔ entry, and OK on either does nothing to
    /// the (nonexistent) catalog focus — `focused_movie` must answer `None` throughout.
    #[test]
    fn a_page_with_no_shelves_answers_no_focused_movie_anywhere() {
        let _serial = crate::testlock::serial();
        let s = seed(0, 0);
        assert!(s
            .focused_item(Some(crate::ui::machine::FocusKey {
                entry: s.entry,
                elem: HEADER_ELEM
            }))
            .is_none());
        assert!(s
            .focused_item(Some(crate::ui::machine::FocusKey {
                entry: s.entry,
                elem: ENTRY_ELEM
            }))
            .is_none());
        assert!(s
            .focused_item(Some(crate::ui::machine::FocusKey {
                entry: s.entry,
                elem: FIRST_CARD_ELEM
            }))
            .is_none());
        let _ = &s;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// `PressCommit` leaves immediately through the content contract; there is no pending latch.
    #[test]
    fn press_commit_on_a_card_pushes_detail_as_an_effect() {
        let _serial = crate::testlock::serial();
        let mut s = seed(2, 0);
        let m = FixtureMeasure;
        let cxv = cx(&m);
        let mut present = crate::ui::present::Present::new();
        let mut buf: Vec<crate::ui::machine::Stamped<PersonHost>> = Vec::new();
        {
            let mut fx = Effects::new(
                &mut buf,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            s.commit_card(&cxv, &mut fx);
        }
        assert!(buf.is_empty(), "no focus means no navigation effect");
        // Feed the identical `Cx` shape but with focus parked on the first movie tile.
        let cxv2 = Cx {
            views: (),
            tick: Tick::default(),
            measure: &m,
            press: PressRead::default(),
            focus: FocusRead {
                current: Some(focus_of(&s, 0, 0)),
            ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)),
        };
        {
            let mut fx = Effects::new(
                &mut buf,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            s.commit_card(&cxv2, &mut fx);
        }
        assert!(buf.iter().any(|st| matches!(
            &st.fx,
            crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { sid, rk })))
                if *sid == ServerId::UNSET && rk == "m0"
        )));
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// An explicit D-pad/pointer arrival on the header marks it (the bio truncation mark may show
    /// from then on); a landing that merely leaves the header as the resolved position (`Restore`)
    /// must not.
    #[test]
    fn focus_moved_marks_the_header_only_on_an_explicit_arrival() {
        let _serial = crate::testlock::serial();
        let mut s = seed(1, 0);
        let header_key = crate::ui::machine::FocusKey {
            entry: EntryId(0),
            elem: HEADER_ELEM,
        };
        let m = FixtureMeasure;
        let cxv = cx(&m);
        let mut present = crate::ui::present::Present::new();
        let mut buf: Vec<crate::ui::machine::Stamped<PersonHost>> = Vec::new();
        let mut fx = Effects::new(
            &mut buf,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        assert!(!s.header_marked);
        Machine::<PersonHost>::step(
            &mut s,
            &ScreenEvent::FocusMoved {
                from: None,
                to: header_key,
                by: By::Restore,
            },
            &cxv,
            &mut fx,
        );
        assert!(
            !s.header_marked,
            "a restore/landing is not an explicit arrival"
        );
        Machine::<PersonHost>::step(
            &mut s,
            &ScreenEvent::FocusMoved {
                from: None,
                to: header_key,
                by: By::Dir,
            },
            &cxv,
            &mut fx,
        );
        assert!(
            s.header_marked,
            "an explicit D-pad press onto the header marks it"
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    fn hash(s: &PersonScreen) -> u64 {
        let mut canon = Canon::new();
        LogicalState::write(s, &mut canon);
        canon.finish()
    }

    /// The interner and its future allocation point are machine decisions, even when two pages
    /// currently draw the same person. A summary-only hash would miss both differences.
    #[test]
    fn logical_state_hash_includes_the_full_card_registry_and_counter() {
        let _serial = crate::testlock::serial();
        let mut a = seed(1, 0);
        let mut b = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        b.card_keys = a.card_keys.clone();
        b.next_card_elem = a.next_card_elem;
        assert_eq!(
            hash(&a),
            hash(&b),
            "identical machine state hashes identically"
        );

        b.next_card_elem += 1;
        assert_ne!(hash(&a), hash(&b), "the future allocator is part of state");
        b.next_card_elem = a.next_card_elem;
        b.card_keys[0].elem += 1;
        assert_ne!(hash(&a), hash(&b), "the identity registry is part of state");
        b.card_keys = a.card_keys.clone();
        b.return_pending = true;
        assert_ne!(
            hash(&a),
            hash(&b),
            "return hydration changes future reconciliation even when the page draws identically"
        );
        let _ = &mut a;
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    /// Entry eviction preserves the identity interner in PageMemory. A reordered landing after
    /// remount therefore resolves the old engine element to the same movie, not merely the same
    /// numeric slot.
    #[test]
    fn evict_remount_with_reordered_shelf_preserves_movie_identity() {
        let _serial = crate::testlock::serial();
        let original = seed(2, 0);
        let old_focus = focus_of(&original, 0, 1);
        assert_eq!(
            original
                .focused_item(Some(old_focus))
                .map(|m| m.rk.as_str()),
            Some("m1")
        );
        let PageMemory::Person(memory) =
            Screen::<PersonHost>::memory_at(&original, Some(old_focus))
        else {
            panic!("Person must persist its interner through PageMemory::Person");
        };

        let mut remounted = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        remounted.restore(&memory);
        crate::person::install_for_test(vec![item("m1"), item("m0")], Vec::new());
        remounted.sync_card_keys();
        let measure = FixtureMeasure;
        let restored = Focusable::<PersonHost>::reconcile(&remounted, old_focus, &cx(&measure));
        assert_eq!(restored.elem, old_focus.elem);
        assert_eq!(
            remounted
                .focused_item(Some(restored))
                .map(|m| m.rk.as_str()),
            Some("m1")
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    #[test]
    fn restoring_a_frozen_registry_never_rewinds_keys_minted_after_the_snapshot() {
        let _serial = crate::testlock::serial();
        let mut screen = seed(1, 0);
        let frozen = screen.memory();
        crate::person::install_for_test(vec![item("m0"), item("newer")], Vec::new());
        screen.sync_card_keys();
        let newer_elem = screen
            .elem_for(&screen.person().unwrap().shelf(0)[1])
            .expect("the live body interned the later landing");
        let live_next = screen.next_card_elem;

        screen.restore(&frozen);

        assert_eq!(
            screen.elem_for(&screen.person().unwrap().shelf(0)[1]),
            Some(newer_elem),
            "request-time memory merges into a live interner instead of replacing it"
        );
        assert_eq!(screen.next_card_elem, live_next);
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    fn pending_share_return(
        measure: &FixtureMeasure,
    ) -> (
        PersonScreen,
        crate::ui::machine::FocusKey<u32>,
        ServerId,
        ServerId,
    ) {
        crate::plex::reset_servers_for_test();
        let origin =
            crate::plex::register_for_test("person-pending-origin", "127.0.0.1", 1, "a", "cid");
        let share =
            crate::plex::register_for_test("person-pending-share", "127.0.0.1", 2, "b", "cid");
        let mut original = PersonScreen::new(
            EntryId(0),
            origin,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        crate::person::install_source_for_test(share, vec![item_on(share, "wanted")], Vec::new());
        original.sync_card_keys();
        let old_focus = focus_of(&original, 0, 0);
        let PageMemory::Person(memory) =
            Screen::<PersonHost>::memory_at(&original, Some(old_focus))
        else {
            panic!("Person must persist its interner through PageMemory::Person");
        };

        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        let mut returned = PersonScreen::new(
            EntryId(0),
            origin,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        returned.restore(&memory);
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::RestoreMemory(PageMemory::Person(memory)),
            &cx_at(measure, old_focus),
            &mut fx,
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::Enter(Enter::Restored),
            &cx_at(measure, old_focus),
            &mut fx,
        );
        crate::person::install_source_for_test(
            origin,
            vec![item_on(origin, "available")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::StoreChanged(crate::stores::StoreId::Person.ord(), 1),
            &cx_at(measure, old_focus),
            &mut fx,
        );
        assert!(returned.return_pending);
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&returned, old_focus, &cx_at(measure, old_focus)),
            old_focus
        );
        (returned, old_focus, origin, share)
    }

    #[test]
    fn a_direction_abandons_an_unavailable_return_card_and_allows_fallback() {
        let _serial = crate::testlock::serial();
        let measure = FixtureMeasure;
        let (mut returned, old_focus, _origin, _share) = pending_share_return(&measure);
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        let right = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Replay,
            kind: InputKind::Key {
                key: Key::Right,
                sym: 0,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        assert_eq!(
            Machine::<PersonHost>::step(
                &mut returned,
                &right,
                &cx_at(&measure, old_focus),
                &mut fx,
            ),
            Handled::No,
            "the engine still owns directional movement"
        );
        assert!(!returned.return_pending);
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&returned, old_focus, &cx_at(&measure, old_focus)),
            focus_of(&returned, 0, 0),
            "the same frame's dispatcher reconcile can seat the available card"
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn a_click_abandons_an_unavailable_return_card() {
        let _serial = crate::testlock::serial();
        let measure = FixtureMeasure;
        let (mut returned, old_focus, _origin, _share) = pending_share_return(&measure);
        let available = focus_of(&returned, 0, 0);
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        let click = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Replay,
            kind: InputKind::Click {
                x: 0.0,
                y: 0.0,
                hit: Some(available.elem),
            },
        });
        assert_eq!(
            Machine::<PersonHost>::step(
                &mut returned,
                &click,
                &cx_at(&measure, old_focus),
                &mut fx,
            ),
            Handled::No
        );
        assert!(!returned.return_pending);
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn a_successful_empty_source_answer_releases_the_return_card_to_fallback() {
        let _serial = crate::testlock::serial();
        let measure = FixtureMeasure;
        let (mut returned, old_focus, _origin, share) = pending_share_return(&measure);
        crate::person::install_source_for_test(share, Vec::new(), Vec::new());
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::StoreChanged(crate::stores::StoreId::Person.ord(), 2),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(
            !returned.return_pending,
            "a successful empty media answer is terminal for this saved source"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&returned, old_focus, &cx_at(&measure, old_focus)),
            focus_of(&returned, 0, 0)
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        crate::plex::reset_servers_for_test();
    }

    /// Returning to a retained Person A first reclaims the single-slot store from Person B. That
    /// request is asynchronous: until A's own shelves land, the old engine key is deliberately
    /// absent from the published rows. Reconciliation must hold that known identity instead of
    /// rewriting the engine cursor to Header; the reordered landing can then resolve the same key
    /// back to the same movie without a screen-local focus copy.
    #[test]
    fn retained_back_holds_the_known_card_key_until_the_requested_person_lands() {
        let _serial = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let origin =
            crate::plex::register_for_test("person-return-origin", "127.0.0.1", 1, "a", "cid");
        let share =
            crate::plex::register_for_test("person-return-share", "127.0.0.1", 2, "b", "cid");
        let mut first = PersonScreen::new(
            EntryId(0),
            origin,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        crate::person::install_source_for_test(
            share,
            vec![item_on(share, "m0"), item_on(share, "m1")],
            Vec::new(),
        );
        first.sync_card_keys();
        let old_focus = focus_of(&first, 0, 1);
        assert_eq!(
            first
                .focused_item(Some(old_focus))
                .map(|movie| movie.rk.as_str()),
            Some("m1")
        );
        let PageMemory::Person(memory) = Screen::<PersonHost>::memory_at(&first, Some(old_focus))
        else {
            panic!("Person must persist its interner through PageMemory::Person");
        };

        let _other = PersonScreen::new(
            EntryId(8),
            origin,
            "other".to_string(),
            "other-guid".to_string(),
            "Other Person".to_string(),
            String::new(),
        );
        assert!(first.person().is_none(), "Person B displaced Person A");

        let measure = FixtureMeasure;
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::RestoreMemory(PageMemory::Person(memory)),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::StoreChanged(crate::stores::StoreId::Person.ord(), 1),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(
            first.return_pending,
            "Person B's store notice is not an answer about A"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, old_focus)),
            old_focus,
            "a wrong-person notice cannot consume return hydration"
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::Enter(Enter::Restored),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(first.person().is_some(), "return reclaims Person A's store");
        assert!(
            first.person().unwrap().shelf(0).is_empty(),
            "the addressed shelves have not landed yet"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, old_focus)),
            old_focus,
            "a known return identity must survive the empty resolving interval"
        );

        crate::person::install_source_for_test(
            origin,
            vec![item_on(origin, "other-source-card")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::StoreChanged(crate::stores::StoreId::Person.ord(), 2),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(
            first.person().unwrap().landed,
            "another source has enough content to settle the page-level spinner"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, old_focus)),
            old_focus,
            "page-level landed must not settle the saved card's still-resolving source"
        );

        crate::person::install_source_for_test(
            share,
            vec![item_on(share, "m1"), item_on(share, "m0")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::StoreChanged(crate::stores::StoreId::Person.ord(), 3),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(
            !first.return_pending,
            "the matching card landing retires hydration"
        );
        let restored =
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, old_focus));
        assert_eq!(restored, old_focus);
        assert_eq!(
            first
                .focused_item(Some(restored))
                .map(|movie| movie.rk.as_str()),
            Some("m1"),
            "the reordered landing resolves the preserved identity"
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        crate::plex::reset_servers_for_test();
    }

    /// The same delayed interval exists after the dispatcher's body cap evicts the Person screen:
    /// PageMemory restores the interner before `Enter(Restored)`, while the new body has already
    /// opened an empty A store. The saved engine key must remain intact until A's shelves arrive.
    #[test]
    fn cold_remount_holds_the_memory_interner_key_until_reordered_shelves_land() {
        let _serial = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("person-cold-return", "127.0.0.1", 1, "a", "cid");
        let mut original = PersonScreen::new(
            EntryId(0),
            sid,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        crate::person::install_source_for_test(
            sid,
            vec![item_on(sid, "m0"), item_on(sid, "m1")],
            Vec::new(),
        );
        original.sync_card_keys();
        let old_focus = focus_of(&original, 0, 1);
        let PageMemory::Person(memory) =
            Screen::<PersonHost>::memory_at(&original, Some(old_focus))
        else {
            panic!("Person must persist its interner through PageMemory::Person");
        };

        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        let mut remounted = PersonScreen::new(
            EntryId(0),
            sid,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        remounted.restore(&memory);

        let measure = FixtureMeasure;
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut remounted,
            &ScreenEvent::RestoreMemory(PageMemory::Person(memory)),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        Machine::<PersonHost>::step(
            &mut remounted,
            &ScreenEvent::Enter(Enter::Restored),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(remounted.person().unwrap().shelf(0).is_empty());
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&remounted, old_focus, &cx_at(&measure, old_focus)),
            old_focus,
            "CAP eviction must not turn the saved card identity into Header while A reloads"
        );

        crate::person::install_source_for_test(
            sid,
            vec![item_on(sid, "m1"), item_on(sid, "m0")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut remounted,
            &ScreenEvent::StoreChanged(crate::stores::StoreId::Person.ord(), 2),
            &cx_at(&measure, old_focus),
            &mut fx,
        );
        assert!(!remounted.return_pending);
        let restored =
            Focusable::<PersonHost>::reconcile(&remounted, old_focus, &cx_at(&measure, old_focus));
        assert_eq!(restored, old_focus);
        assert_eq!(
            remounted
                .focused_item(Some(restored))
                .map(|movie| movie.rk.as_str()),
            Some("m1")
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
        crate::plex::reset_servers_for_test();
    }

    #[test]
    fn restore_reclaims_only_a_displaced_store_and_preserves_shell_scroll() {
        let _serial = crate::testlock::serial();
        let mut first = seed(2, 0);
        first.scroll.scroll.jump(173.0);
        let _other = PersonScreen::new(
            EntryId(8),
            ServerId::UNSET,
            "other".to_string(),
            "other-guid".to_string(),
            "Other Person".to_string(),
            String::new(),
        );
        assert!(first.person().is_none());

        let measure = FixtureMeasure;
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::Enter(Enter::Restored),
            &cx(&measure),
            &mut fx,
        );
        assert!(first.person().is_some());
        assert_eq!(first.scroll.scroll.pos, 173.0);

        // A second restore while the matching store is already current is a true no-op.
        let before_gen = crate::stores::gen(crate::stores::StoreId::Person);
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::Enter(Enter::Restored),
            &cx(&measure),
            &mut fx,
        );
        assert_eq!(
            crate::stores::gen(crate::stores::StoreId::Person),
            before_gen
        );
        assert_eq!(first.scroll.scroll.pos, 173.0);
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    #[test]
    fn entry_back_and_hold_emit_their_content_effects_without_latches() {
        let _serial = crate::testlock::serial();
        let mut s = seed(1, 0);
        crate::person::install_credits_for_test(&[("Actor", 7)]);
        let measure = FixtureMeasure;
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();

        let mut run = |screen: &mut PersonScreen,
                       event: ScreenEvent<PersonHost>,
                       focus: Option<crate::ui::machine::FocusKey<u32>>| {
            let context = Cx {
                views: (),
                tick: Tick::default(),
                measure: &measure,
                press: PressRead::default(),
                focus: FocusRead { current: focus , ..Default::default() },
                owner: InputOwner::Entry(screen.entry),
            };
            let mut fx = Effects::new(
                &mut out,
                crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(0)),
                &mut present,
            );
            Machine::<PersonHost>::step(screen, &event, &context, &mut fx)
        };

        assert_eq!(
            run(&mut s, ScreenEvent::Activate(ENTRY_ELEM), None),
            Handled::Yes
        );
        let card = focus_of(&s, 0, 0);
        assert_eq!(
            run(
                &mut s,
                ScreenEvent::PressHold(crate::ui::machine::PressId(2)),
                Some(card)
            ),
            Handled::Yes
        );
        let back = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Replay,
            kind: InputKind::Key {
                key: Key::Back,
                sym: 0,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        assert_eq!(run(&mut s, back, Some(card)), Handled::Yes);
        drop(run);

        assert!(out.iter().any(|st| matches!(
            &st.fx,
            crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Present(
                ContentArg::Filmography { sid, key }
            ))) if *sid == ServerId::UNSET && key == "161"
        )));
        assert!(out.iter().any(|st| matches!(
            &st.fx,
            crate::ui::machine::Fx::App(AppFx::Content(ContentReq::ItemMenu))
        )));
        assert!(out.iter().any(|st| matches!(
            &st.fx,
            crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Back))
        )));
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    #[test]
    fn header_geometric_anchor_is_not_its_pointer_hit_rectangle() {
        let _serial = crate::testlock::serial();
        let s = seed(1, 0);
        let anchor = s.header_anchor();
        let hit = s.header_rect();
        assert_eq!(anchor.x, MARGIN_X);
        assert_eq!(anchor.w, 0.0, "the navigation projection stays left-pinned");
        assert!(
            hit.w > 0.0 && hit.h > 0.0,
            "the drawn biography band is clickable"
        );

        let measure = FixtureMeasure;
        let placed = Focusable::<PersonHost>::place(&s, &HEADER_ELEM, &cx(&measure), At::Drawn)
            .expect("the header is a real engine element");
        assert_eq!(
            (placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h),
            (hit.x, hit.y, hit.w, hit.h),
            "place and the recorded stop share the hit geometry"
        );
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    #[test]
    fn focus_walks_only_the_shelves_that_exist() {
        let _serial = crate::testlock::serial();
        let s = seed(3, 0);
        let measure = FixtureMeasure;
        let context = cx(&measure);
        let mut groups = Vec::new();
        Focusable::<PersonHost>::groups(&s, &context, &mut groups);
        assert!(groups.iter().any(|g| g.id == SHELF_GROUP[0] && g.len == 3));
        assert!(!groups.iter().any(|g| g.id == SHELF_GROUP[1]));
        let last = focus_of(&s, 0, 2);
        assert!(matches!(
            Focusable::<PersonHost>::neighbour(&s, last, Dir::Right, &context),
            Step::Edge
        ));
        crate::person::install_credits_for_test(&[("Actor", 3)]);
        let mut links = Vec::new();
        Screen::<PersonHost>::links(&s, &mut links);
        assert!(links.iter().any(|link| {
            link.from == ENTRY_GROUP && link.dir == Dir::Down && link.to == SHELF_GROUP[0]
        }));
        crate::stores::person::apply(crate::stores::person::PersonCmd::Close);
    }

    #[test]
    fn the_first_shelf_fits_at_rest() {
        let h = shelf_block_h_at(card_row::under_band(1.0));
        for band in [PORTRAIT_BARE, PORTRAIT_EXP] {
            let top = HEADER_TOP + band + BAND_GAP_TO_SHELF;
            assert_eq!(reveal_block(0.0, top, h, top + h + BOTTOM_PAD), 0.0);
        }
    }

    #[test]
    fn reaching_the_second_shelf_scrolls_it_fully_into_view_and_no_further() {
        let h = shelf_block_h_at(card_row::under_band(1.0));
        let top = HEADER_TOP + PORTRAIT_EXP + BAND_GAP_TO_SHELF + h + SHELF_GAP;
        let content = top + h + BOTTOM_PAD;
        let want = reveal_block(0.0, top, h, content);
        assert!(want > 0.0);
        assert!(top + h - want <= SCR_H);
        assert!(top - want >= TOP_MARGIN);
    }

    #[test]
    fn a_shelf_here_pitches_like_a_shelf_on_home() {
        assert_eq!(
            SHELF_GAP + shelf_block_h_at(card_row::under_band(1.0)),
            crate::ui::consts::ROW_PITCH
        );
        assert_eq!(SHELF_LABEL_H, TITLE_DY + CARD_DY);
    }

    #[test]
    fn the_entry_pill_shows_equal_air_at_both_ends_when_focused() {
        for width in [2.0 * ENTRY_OUTER_PAD + 60.0, 307.0, 420.0] {
            let group = width - 2.0 * ENTRY_OUTER_PAD;
            let scale = crate::ui::widgets::CTRL_FOCUS_SCALE;
            let leading = entry_run_x(width, scale);
            let trailing = width * scale - leading - group;
            assert!((leading - trailing).abs() < 0.01);
            assert!(leading > ENTRY_OUTER_PAD);
        }
    }
}
