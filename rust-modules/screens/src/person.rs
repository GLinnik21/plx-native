//! The owned person page for restructure phase 7. Its measured header keeps the Filmography
//! action below the biography in the text column. Shelves, biography panel and animation share
//! the UI system, while navigation and focus are explicit contracts:
//! the engine is the only cursor, card keys preserve item identity across store reshapes, and page
//! changes leave through `ContentReq` effects. Filmography is a separate modal `Screen`; this page
//! presents it and never imports or stores a sibling screen. The Person owner observes the
//! visible session generation and retires credential-bound metadata on identity changes; its
//! normal store landing rebuilds this page and the filmography without reopening either.
//!
//! **The biography sheet left in phase 10** (`screens::person_bio`, `ContentPanel::Bio`): it is a
//! `Style::Alert` surface on the container tree, so its input, its phase and its teardown are the
//! container's and this page neither intercepts keys for it nor hides it. What stays here is the
//! GATE — the panel is offered exactly when the `MORE` mark is drawn, and both read
//! [`PersonScreen::bio_more`]: the answer `header_flow` measured over this header's own column
//! width, never a fresh wrap per frame.

use std::ffi::CString;

use plx_data::person::{Person, NSHELF};
use plx_plex::plex::ServerId;
use plx_data::pms::PmsMovie;
use plx_data::stores::person::PersonCmd;
use plx_ui::cards::{RowStyle, TileLabel};
use plx_ui::cards::{CardEvent, CardSource, Kind, SectionSpec, Stack, StackEvent, StackPage};
use plx_ui::consts::*;
use plx_ui::label::{Label, VAlign};
use plx_ui::linked_heading::LinkedHeading;
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, GroupId, Handled, InputEvent, InputKind, Key, Leave,
    LogicalState, Machine, Measure, Tick,
};
use plx_machine::present::Provenance;
use plx_ui::screen::{
    At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, FocusedCard, Focusable, GroupKind,
    GroupSpec, HitSource, Link, RenderStrategy, Screen, ScreenEvent, Seat,
};
#[cfg(test)]
use plx_ui::screen::Enter;
use plx_ui::text_view::TextView;
use plx_ui::theme;
use plx_ui::widgets::{Art, PageGround, StatusKind, StatusOverlay};
use plx_ui::{Env, Painter, Rect, View};

use super::registry::{
    tile_facts, AppFx, CardIdentity, CardKeys, CardPageMemory, ContentArg, ContentLike, ContentReq, PageMemory,
    PersonLike,
};

// -------------------------------------------------------------------------------------------
// element-key + group-id namespace (module doc: "Element-key namespace")
// -------------------------------------------------------------------------------------------

const HEADER_ELEM: u32 = 0;
const ENTRY_ELEM: u32 = 1;
const FIRST_CARD_ELEM: u32 = 0x1000;

/// The page's sections, in document order, and the focus group each is known by. The ids are the
/// page's and fixed per section kind (the engine remembers a cursor per `(entry, group)`, and a
/// fresh mount targets `GroupId(0)`, the Filmography pill): a shelf keeps its id whichever other
/// shelves exist. `groups()` lists them in document order: header, pill, then the shelves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    /// The header band: portrait, name, facts, biography (one focus element), and the Filmography
    /// pill's room. Drawn by the page, in the flow from the top of the document.
    Head,
    /// The Filmography pill: a zero-height focus element that sits inside the head's block
    /// ([`StackPage::reveal_with`]); its pill is drawn with the header.
    Entry,
    /// The skeleton / empty read-out in the shelves' place, while no shelf has content.
    State,
    /// The air above the shelf of this kind.
    Gap(usize),
    /// The shelf of this kind (0 = Movies, 1 = Shows).
    Shelf(usize),
}

const HEADER_GROUP: GroupId = GroupId(1);
const ENTRY_GROUP: GroupId = GroupId(0);
/// Group of the shelf of each kind (movies, shows).
const SHELF_GROUP: [GroupId; NSHELF] = [GroupId(2), GroupId(3)];
/// Sections with no focus element still carry a distinct id ([`SectionSpec::group`]); none is listed.
const STATE_GROUP: GroupId = GroupId(8);
const GAP_GROUP: [GroupId; NSHELF] = [GroupId(9), GroupId(10)];

#[cfg(test)]
pub(crate) mod cards_harness; // Tier 2 card conformance (cards_conformance_tests.rs)

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
// geometry constants — the shared person header, biography and compact Filmography action
// -------------------------------------------------------------------------------------------

const PORTRAIT_EXP: f32 = 320.0;
const PORTRAIT_BARE: f32 = 220.0;
const PORTRAIT_RES: (std::os::raw::c_int, std::os::raw::c_int) = (300, 300);
// The owner's reference mock, 2026-09-19: the portrait's top sits on the page's ordinary top
// margin, not 42px below it — the header is the top of the page's content, not a band offset
// from it.
const HEADER_TOP: f32 = plx_ui::consts::MARGIN_Y;
const BAND_GAP: f32 = theme::space::XL;
// The owner's reference mock, 2026-09-19: name→roles measures one rung tighter than the shipped
// `LG` — the header reads as one tight block rather than a loose stack.
const META_GAP: f32 = theme::space::MD;
// The owner's reference mock, 2026-09-19: roles→Born/Died measures the SAME rung as the line
// above it (`MD`), not the tighter `SM` the two lines shipped with — they no longer read as one
// fact split across two lines, so they no longer sit closer than the gap above them.
const LIFE_GAP: f32 = theme::space::MD;
// The highlight BOX, not the prose, keeps `space::MD` (24) from the facts line above it (design:
// the bio highlight sits `--space-md` below the dates line) — and the box itself pads the prose by
// `HL_PAD_Y` on every side, so the cap-to-prose gap this measures to has to carry both.
const BIO_GAP: f32 = theme::space::MD + HL_PAD_Y;
const SHELF_COUNT_GAP: f32 = theme::space::SM;
const fn text_w_const(d: f32) -> f32 {
    SCR_W - MARGIN_X - (MARGIN_X + d + BAND_GAP)
}
const BIO_W: f32 = text_w_const(PORTRAIT_EXP);
const BIO_LINES: usize = 3;
const BIO_LEAD: f32 = 40.0;
const HL_PAD_X: f32 = 26.0;
const HL_PAD_Y: f32 = 24.0;
const BIO_MORE_GAP: f32 = theme::space::LG;
// The owner's reference mock, 2026-09-19: the Filmography pill sits one rung under the shelf
// heading below it, not the full `XL` region gap the band and its first shelf shipped with — the
// pill is part of the header's own block, not a separate section.
const BAND_GAP_TO_SHELF: f32 = theme::space::MD;
const SHELF_GAP: f32 = UNDER_LABEL_AIR;
const SHELF_LABEL_H: f32 = TITLE_DY + CARD_DY;
const SHELF_STYLE: RowStyle = RowStyle::HOME;

// The owner's reference mock, 2026-09-19: the bio-to-pill gap drops two rungs from `LG` to `SM` —
// the pill reads as the header block's own closing line, not a new section starting under it.
const ENTRY_GAP: f32 = theme::space::SM;


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
/// `credits`/`srcs`/`roster_gen` fields are private to `plx_data::person`, so a host test outside
/// that module cannot build one by hand; the two existing `#[cfg(test)]` seams it exposes
/// (`install_for_test`/`install_credits_for_test`) both force `credited = true`, so the PENDING
/// half of this predicate can only be exercised through its pure form today.
fn has_entry_of(credited: bool, filmography_total: usize) -> bool {
    credited && filmography_total > 0
}

fn has_entry(p: &Person) -> bool {
    has_entry_of(p.credited, plx_data::person::filmography_total(p))
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
// Header measurement uses explicit meta/life presence flags, so the pure layout remains
// testable without a PersonScreen or server data. The measured entry frame belongs to this
// same flow and is reused for painting, focus and hit geometry.
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
    has_roles: bool,
    has_life: bool,
    has_entry: bool,
    measure: &dyn Measure,
) -> HeaderFlow {
    let mut f = HeaderFlow::default();
    let mut y = measure.cap_h(theme::size::DISPLAY);
    if pending || has_roles {
        y += META_GAP;
        f.meta_y = Some(y);
        y += measure.cap_h(theme::size::LABEL);
    }
    if pending || has_life {
        y += if f.meta_y.is_some() {
            LIFE_GAP
        } else {
            META_GAP
        };
        f.life_y = Some(y);
        // The life line draws at `CAPTION` (see `draw_header`'s loop), not `LABEL` — advancing by
        // the wrong rung's cap height left a few px of drift between the pending skeleton's
        // reserved band and the loaded line's actual one.
        y += measure.cap_h(theme::size::CAPTION);
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
        y += LinkedHeading::HEIGHT;
    }
    let bare = !pending && f.meta_y.is_none() && f.life_y.is_none() && f.bio_y.is_none();
    f.exp_d = if bare { PORTRAIT_BARE } else { PORTRAIT_EXP };
    f.exp_h = y.max(f.exp_d);
    if bare {
        f.portrait_y = (f.exp_h - f.exp_d) * 0.5;
        let ty = (f.exp_h - y) * 0.5;
        f.name_y = ty;
        for v in [&mut f.meta_y, &mut f.life_y, &mut f.bio_y, &mut f.entry_y]
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
    .fade_for_more(BIO_MORE_GAP)
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
    let elided = plx_gfx::text::elide_by(s, w, false, |t| measure.width_str(t, sz, bold != 0));
    CString::new(elided).unwrap_or_default()
}

// -------------------------------------------------------------------------------------------
// the shelf's content, for `plx_ui::cards::Shelf`
// -------------------------------------------------------------------------------------------

/// One shelf's cards, read over the store's person and the page's element interning. The engine
/// element of a card is stable for the catalog item's identity ([`CardKeys`]), so a landing may
/// move an item between shelves without moving the engine cursor off that item: each shelf's
/// `index_of` simply stops (or starts) answering for it.
struct ShelfCards<'a> {
    person: &'a Person,
    kind: usize,
    cards: &'a CardKeys,
}

impl<H: ContentLike> CardSource<H> for ShelfCards<'_> {
    fn len(&self) -> usize {
        self.person.shelf(self.kind).len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.person
            .shelf(self.kind)
            .get(i)
            .and_then(|m| self.cards.elem_for(m.sid, &m.rk))
            .unwrap_or(HEADER_ELEM)
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        let id = self.cards.get(*e)?;
        self.person
            .shelf(self.kind)
            .iter()
            .position(|m| plx_plex::plex::same_item((m.sid, m.rk.as_str()), (id.sid, id.rk.as_str())))
    }
    fn art(&self, i: usize) -> Art<'_> {
        Art::Poster(self.person.shelf(self.kind).get(i).map(tile_facts::of))
    }
    fn label(&self, i: usize) -> TileLabel {
        match self.person.shelf(self.kind).get(i) {
            Some(m) => TileLabel::titled(&m.title, self.person.role(self.kind, i)),
            None => TileLabel::default(),
        }
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.person.shelf(self.kind).get(i).and_then(|m| m.resume_frac())
    }
}

// -------------------------------------------------------------------------------------------
// the screen
// -------------------------------------------------------------------------------------------

/// Which groups the page links, as of the store's last landing: whether the Filmography entry is
/// offered, and the shelf kinds that have content, in flow order.
#[derive(Clone, Copy, Default)]
struct Reach {
    entry: bool,
    kinds: [usize; NSHELF],
    n: usize,
}

/// The page's content and the state its sections read. The `Stack` is the screen's other field so
/// that `self.stack.on(&self.page, ..)` borrows the two apart; every section reads only this.
struct Page {
    entry: EntryId,
    // ---- identity, fixed at construction; re-issued to the store on `Enter` (module doc: "an
    // Enter, fresh or restored, re-opens" — this Bridge's PersonStore holds one person, so
    // returning to a covered instance must re-claim it through an addressed effect) ----
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
    cards: CardKeys,
    /// One-shot return hydration. While set, a known engine card key may remain unpublished until
    /// that card's own source answers; the engine remains the sole owner of the key itself.
    return_pending: bool,

    // ---- render cache: ambient ground and skeleton clock (never hashed; the page's own scroll and card springs are the stack's, which IS in the canon) ----
    amb: PageGround,
    amb_seeded: bool,
    /// Skeleton spinner clock, in ms — cached each tick from [`spin_phase`](Self::spin_phase)'s
    /// `advance`.
    spin_ms: f32,
    /// The underlying clock for [`spin_ms`](Self::spin_ms) (`motion::Phase`, phase 12 D4): reports
    /// `Motion` from inside its own `advance` rather than the raw `+= dt` this used to be, with
    /// `fx.note(Motion)` a separate line further down `tick`.
    spin_phase: plx_machine::motion::Phase,

    // ---- render cache: baked text runs + measured flow, rebuilt only when the store lands ----
    name_c: CString,
    /// The life line's parts, in flow order (`Born …`, the birthplace, `Died …`, each absent
    /// rather than blank) — drawn through `widgets::dotted_run`, which owns the `·` ink itself, so
    /// this is never joined into one string the way `name_c`/the shelf-count runs are.
    life_parts: Vec<String>,
    shelf_count_c: [CString; NSHELF],
    entry_count_c: CString,
    header: HeaderFlow,
    header_dirty: bool,
    /// What the page's links are made of; the screen turns it into group ids ([`PersonScreen::links`]).
    reach: Reach,
    /// The store revision ([`PersonView::revision`](plx_data::person::PersonView::revision)) the
    /// card interning and `links_c` were last derived at; `None` until a person is there. The
    /// content counter, not the request epoch and not a shelf length: a same-length shelf
    /// replacement moves it.
    synced: Option<u64>,
    covered_ready_c: bool,
    /// The biography's focus lift ([`Self::bio_marked`]). Presentation, not logical state.
    bio_lift: plx_ui::text_lift::TextLift,
    /// Moves whenever the page derives something its sections read that the store's revision does
    /// not carry (a header remeasure). Derived, not logical state.
    layout_gen: u64,
}

/// The person / actor page (restructure spec §13, phase 7).
pub struct PersonScreen {
    page: Page,
    /// The page's layout, one scroll, groups, reconcile and the shelves (`ui::cards::Stack`).
    /// Presentation, not logical state — its motion is in the canon, its content is the store's.
    stack: Stack<Sec>,
    /// This page has already asked the Person store to close the slot it owned. §3.4's
    /// `pop_sequence` delivers `WillLeave(ForGood)` AND `Unmount` to the same body, and both land
    /// in the teardown arm below; an un-latched `self.person(cx).is_some()` guard there reads a
    /// slot this page has already disclaimed by the time the second event arrives, because the
    /// first `Close` crosses `AppFx::Store` and is still queued rather than applied — and so
    /// emits a SECOND non-idempotent `Close`. Not hashed: it is teardown bookkeeping for one
    /// event pair, never a property of the page's logical shape. Mirrors
    /// `screens/detail/mod.rs`'s `teardown_cleared` exactly (#119, #126).
    teardown_closed: bool,
}

impl LogicalState for PersonScreen {
    fn write(&self, w: &mut Canon) {
        let page = &self.page;
        w.u32(page.entry.0)
            .u32(page.sid.raw() as u32)
            .str(&page.key)
            .str(&page.guid)
            .str(&page.name)
            .str(&page.thumb)
            .bool(page.header_marked)
            .bool(page.return_pending)
            .u32(page.cards.next)
            .u32(page.cards.len() as u32);
        for card in &page.cards.keys {
            w.u32(card.sid.raw() as u32).str(&card.rk).u32(card.elem);
        }
        self.stack.write(w);
    }

    fn probe(&self, out: &mut String) {
        let page = &self.page;
        out.push_str(&format!(
            "person sid={} key={} marked={} return_pending={} card_keys={} next_elem={}",
            page.sid.raw(),
            page.key,
            page.header_marked,
            page.return_pending,
            page.cards.len(),
            page.cards.next
        ));
    }
}

impl PersonScreen {
    pub const SHAPE: &'static str = "PersonScreen{entry:EntryId,sid:ServerId,key:String,guid:String,name:String,thumb:String,header_marked:bool,return_pending:bool,next_card_elem:u32,card_keys:[{sid:ServerId,rk:String,elem:u32}],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Shelf{motion}]}}";

    /// Mount a person from the header a cast row (or, later, the Filmography route) handed in —
    /// mirrors `PersonCmd::Open`'s fields exactly. Issues the store command directly; see the
    /// module doc for why an `Enter` (fresh OR restored) does so again rather than assuming the
    /// single-slot store still holds this instance's data.
    pub fn new(
        entry: EntryId,
        sid: ServerId,
        key: String,
        guid: String,
        name: String,
        thumb: String,
    ) -> Self {
        Self {
            page: Page::new(entry, sid, key, guid, name, thumb),
            // A page that leaves focus (a menu over it, a pointer gone) rests at the top.
            stack: Stack::new(entry).home_when_unfocused(true),
            teardown_closed: false,
        }
    }

    /// The page's `Focusable` (see `focusable_via_view!` below): the stack's view over the page.
    fn view<H: ContentLike + PersonLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    pub fn restore(&mut self, memory: &CardPageMemory) {
        self.page.restore(memory);
    }
}

impl Page {
    fn new(entry: EntryId, sid: ServerId, key: String, guid: String, name: String, thumb: String) -> Self {
        Self {
            entry,
            sid,
            key,
            guid,
            name,
            thumb,
            header_marked: false,
            cards: CardKeys::new(FIRST_CARD_ELEM),
            return_pending: false,
            amb: PageGround::new(),
            amb_seeded: false,
            spin_ms: 0.0,
            spin_phase: plx_machine::motion::Phase::default(),
            name_c: CString::default(),
            life_parts: Vec::new(),
            shelf_count_c: [CString::default(), CString::default()],
            entry_count_c: CString::default(),
            header: HeaderFlow::default(),
            header_dirty: true,
            reach: Reach::default(),
            synced: None,
            covered_ready_c: false,
            bio_lift: plx_ui::text_lift::TextLift::new(),
            layout_gen: 0,
        }
    }

    /// Claim this Bridge's Person owner for this identity through the addressed store effect.
    fn request_store<H: ContentLike + PersonLike>(&mut self, fx: &mut Effects<'_, H>) {
        fx.push(plx_machine::machine::Fx::App(AppFx::Store(
            plx_data::stores::StoreId::Person,
            plx_data::stores::StoreCmd::Person(PersonCmd::Open {
            sid: self.sid,
            key: self.key.clone(),
            guid: self.guid.clone(),
            name: self.name.clone(),
            thumb: self.thumb.clone(),
            }),
        )));
        self.header_dirty = true;
        self.amb_seeded = false;
    }

    fn person<'a, H: PersonLike>(&self, cx: &Cx<'a, H>) -> Option<&'a Person> {
        H::person(cx).current().filter(|p| {
            plx_plex::plex::same_item((p.sid, p.key.as_str()), (self.sid, self.key.as_str()))
        })
    }

    fn sync_card_keys<H: PersonLike>(&mut self, cx: &Cx<'_, H>) {
        let Some(p) = self.person(cx) else {
            return;
        };
        let shelved = (0..NSHELF).flat_map(|kind| p.shelf(kind).iter());
        self.cards.intern_all(shelved.map(|item| (item.sid, item.rk.as_str())), "person");
    }

    fn refresh_store_cache<H: PersonLike>(&mut self, cx: &Cx<'_, H>) {
        self.covered_ready_c = false;
        let Some(person) = self.person(cx) else {
            self.synced = None;
            self.reach = Reach::default();
            return;
        };
        self.covered_ready_c = person.credited && !H::person(cx).loading();
        let revision = H::person(cx).revision();
        if self.synced == Some(revision) {
            return;
        }
        self.synced = Some(revision);
        self.layout_gen += 1;
        self.sync_card_keys(cx);
        let (kinds, n) = present(person);
        self.reach = Reach { entry: entry_reachable(person), kinds, n };
    }

    pub fn restore(&mut self, memory: &CardPageMemory) {
        self.synced = None;
        self.cards.merge(&memory.cards, FIRST_CARD_ELEM, "person");
        self.header_marked |= memory.header_marked;
    }

    fn memory(&self) -> CardPageMemory {
        CardPageMemory { cards: self.cards.clone(), header_marked: self.header_marked }
    }

    #[cfg(test)]
    fn elem_for(&self, item: &PmsMovie) -> Option<u32> {
        self.cards.elem_for(item.sid, &item.rk)
    }

    fn card_for_elem(&self, elem: u32) -> Option<&CardIdentity> {
        self.cards.get(elem)
    }

    /// Retire return hydration only after the engine's saved card has an answer from its own
    /// source. A page-level `landed` is intentionally not consulted: another server may already
    /// have populated a shelf while this card's server is still resolving or retrying.
    fn settle_return_pending<H: ContentLike + PersonLike>(&mut self, cx: &Cx<'_, H>) {
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
        let Some(person) = self.person(cx) else {
            return;
        };
        if self.locate(person, elem).is_some() || !plx_data::person::media_resolving(person, sid) {
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
        let id = self.cards.get(elem)?;
        for kind in 0..NSHELF {
            if let Some(col) = p.shelf(kind).iter().position(|m| {
                plx_plex::plex::same_item((m.sid, m.rk.as_str()), (id.sid, id.rk.as_str()))
            }) {
                return Some(Located::Shelf(kind, col));
            }
        }
        None
    }

    #[cfg(test)]
    fn shelf_key(&self, p: &Person, kind: usize, col: usize) -> plx_machine::machine::FocusKey<u32> {
        let elem = p
            .shelf(kind)
            .get(col)
            .and_then(|m| self.elem_for(m))
            .unwrap_or(HEADER_ELEM);
        plx_machine::machine::FocusKey {
            entry: self.entry,
            elem,
        }
    }

    fn refresh_runs(&mut self, p: &Person, measure: &dyn Measure) {
        let w = text_w(PORTRAIT_EXP);
        self.name_c = cstr_elide(&p.name, w, theme::size::DISPLAY, 1, measure);

        // Roles and life are drawn through `widgets::dotted_run` now (see `draw_header`), which
        // owns the `·` ink itself and elides nothing — `MAX_ROLES`/the fixed life-fact count are
        // this line's width safety, the way the cap on any other fixed-vocabulary run is, so the
        // per-character `cstr_elide` this used to run is no longer needed. Birthplace is its own
        // part now (design: "Born …" · birthplace · "Died …"), not glued onto Born with a comma —
        // it no longer depends on a birth date being known.
        self.life_parts = Vec::new();
        let born = plx_ui::fmt::pretty_date(&p.born, 0);
        if !born.is_empty() {
            self.life_parts.push(plx_platform::i18n::msg::browse_person_born(&born));
        }
        if !p.birthplace.is_empty() {
            self.life_parts.push(p.birthplace.clone());
        }
        let died = plx_ui::fmt::pretty_date(&p.died, 0);
        if !died.is_empty() {
            self.life_parts.push(plx_platform::i18n::msg::browse_person_died(&died));
        }

        for k in 0..NSHELF {
            self.shelf_count_c[k] = match p.total(k) {
                0 => CString::default(),
                n => CString::new(plx_platform::i18n::current().number(n as i64)).unwrap_or_default(),
            };
        }
        self.entry_count_c = match plx_data::person::filmography_total(p) {
            0 => CString::default(),
            n => CString::new(plx_platform::i18n::current().number(n as i64)).unwrap_or_default(),
        };
    }

    fn remeasure_header(&mut self, p: &Person, measure: &dyn Measure) {
        self.refresh_runs(p, measure);
        self.header = header_flow(
            plx_data::person::facts_pending(p),
            &p.bio,
            !p.roles.is_empty(),
            !self.life_parts.is_empty(),
            has_entry(p),
            measure,
        );
        self.header_dirty = false;
        self.layout_gen += 1;
    }

    fn focused_movie_in<'a>(&self, p: &'a Person, elem: Option<u32>) -> Option<&'a PmsMovie> {
        match elem.and_then(|e| self.locate(p, e))? {
            Located::Shelf(kind, col) => p.shelf(kind).get(col),
            _ => None,
        }
    }

    pub fn focused_item<'a, H: PersonLike>(
        &self,
        focus: Option<plx_machine::machine::FocusKey<u32>>,
        cx: &Cx<'a, H>,
    ) -> Option<&'a PmsMovie> {
        let p = self.person(cx)?;
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

    /// **Regression (phase-7 port): the pill belongs in the TEXT column.** Before the port
    /// (`894f20f8^:rust-modules/src/ui/person.rs`'s `draw_entry`) it drew at `col_x`, the same x
    /// the name and roles start at; the port dropped that and both this rect and the draw call
    /// below fell back to bare `MARGIN_X` — under the circular portrait. `col_x(self.header.
    /// exp_d)` is the one expression both read, so the drawn pill and its focus/hit rect cannot
    /// disagree about where it sits.
    /// The Filmography entry: the shared linked-heading control, titled from the catalog.
    fn entry_heading(&self) -> LinkedHeading<'_> {
        LinkedHeading::entry(plx_platform::i18n::msg::browse_person_filmography(), self.entry_count_c.to_str().unwrap_or(""))
    }

    /// The Filmography pill's focus/hit rect; `section_y` is the screen y of the [`Sec::Entry`]
    /// section, which sits at the bottom of the head's block (so the page's scroll is
    /// `HEADER_TOP + exp_h - section_y`).
    fn entry_rect(&self, measure: &dyn Measure, section_y: f32) -> Rect {
        let x = col_x(self.header.exp_d);
        let Some(ey) = self.header.entry_y else {
            return Rect::new(x, HEADER_TOP, 0.0, 1.0);
        };
        let entry = self.entry_heading();
        let measured = entry.measure(measure);
        entry.face_rect(x, section_y - self.header.exp_h + ey, 0.0, &measured)
    }

    /// The Tick work that precedes the stack's layout: the store cache, the header measure and
    /// the ambient ground. False while no person is on show (nothing is drawn, nothing animates).
    fn pre_tick<H: ContentLike + PersonLike>(&mut self, dt: f32, cx: &Cx<'_, H>) -> bool {
        let cur = cx.focus.current.map(|k| k.elem);
        self.refresh_store_cache(cx);
        let Some(p) = self.person(cx) else {
            // Nothing is drawn while `person()` is `None` (`Screen::draw` returns before touching
            // the skeleton), so there is nothing on screen for the clock to animate — freeze
            // rather than advance-and-report for no visible reason.
            return false;
        };
        if self.header_dirty {
            self.remeasure_header(p, cx.measure);
        }

        let focused_movie = self.focused_movie_in(p, cur);
        let k = PageGround::page_target(focused_movie.map(tile_facts::of));
        if self.amb_seeded {
            self.amb.key_target(k, dt);
        } else {
            self.amb.jump_target(k);
            self.amb_seeded = true;
        }
        true
    }

    /// The Tick work that follows the stack's: the skeleton clock and the biography's lift.
    fn post_tick<H: ContentLike + PersonLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(p) = self.person(cx) else { return };
        if plx_data::person::facts_pending(p) || H::person(cx).loading() {
            self.spin_ms = self.spin_phase.advance(t, &mut fx.present());
        }
        let cur = cx.focus.current.map(|k| k.elem);
        self.bio_lift.step(self.bio_marked(cur), t.dt());
    }

    /// OK on the header: opens the biography panel when the bio is truncated. Mirrors
    /// `header_ok`'s tail (the overlay guard lives in `step`'s `Input` arm now — see the module
    /// doc for why the raw key is intercepted before the engine ever turns it into `Activate`).
    fn activate_header<H: ContentLike + PersonLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.header_marked = true;
        if self.person(cx).is_some() && self.bio_more() {
            // The GATE is the page's, and stays the page's: the panel exists exactly when the
            // `MORE` mark is drawn, and both read `bio_more` — which depends on this
            // header's own column width. A surface asked to re-derive it would be how the mark and
            // the sheet came to disagree about whether there is more to read.
            fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Panel(
                crate::registry::ContentPanel::Bio,
            ))));
            fx.invalidate(Provenance::Input);
        }
    }

    /// **Can the biography sheet be offered at all?** The page's answer about the page's own
    /// person, and the same predicate the `MORE` mark is drawn from — so the mark and the sheet
    /// cannot disagree about whether there is more to read.
    ///
    /// `pub` for one caller, `dev::scenarios`' `/tmp/plxnative-bio`: a headless boot presents
    /// the sheet through the same door the OK press uses, and asking the page first is what stops
    /// the trigger opening a panel an interactive press would have refused. `DetailScreen::
    /// tracks_available` is the precedent and the reason.
    /// The scenario has no frame capability; it reads the header's last measured answer and
    /// waits for the next measure after a store invalidation, just as the painted header does.
    pub fn bio_available(&self, view: plx_data::person::PersonView<'_>) -> bool {
        view.current().is_some_and(|person| plx_plex::plex::same_item(
            (person.sid, person.key.as_str()), (self.sid, self.key.as_str())))
            && self.bio_more()
    }

    /// **Is there more biography than the header shows?** The one predicate the `MORE` mark, the
    /// OK gate and `bio_available` read. It is the answer `header_flow` measured when the header
    /// was last laid out — not a fresh wrap: the set's person-page stack profile (2026-09-19)
    /// caught the draw path re-wrapping the whole bio (`TTF_SizeUTF8` per word) every frame to
    /// ask this. A header awaiting its remeasure answers `false` until it has one.
    fn bio_more(&self) -> bool {
        !self.header_dirty && self.header.bio_truncated
    }

    /// The bio earns its marked/lifted treatment when the header holds focus, is marked open, and
    /// the bio truncates.
    fn bio_marked(&self, focus: Option<u32>) -> bool {
        focus == Some(HEADER_ELEM) && self.header_marked && self.bio_more()
    }

    fn activate_entry<H: ContentLike + PersonLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(p) = self.person(cx) else {
            return;
        };
        if has_entry(p) {
            fx.push(plx_machine::machine::Fx::App(AppFx::Content(
                ContentReq::Present(ContentArg::Filmography {
                    sid: self.sid,
                    key: self.key.clone(),
                }),
            )));
        }
    }

    /// A press committed on card `elem`: leave for its detail page.
    fn open_card<H: ContentLike + PersonLike>(&self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let item = self.person(cx).and_then(|p| self.focused_movie_in(p, Some(elem)));
        if let Some(m) = item {
            fx.push(plx_machine::machine::Fx::App(AppFx::Content(
                ContentReq::Push(ContentArg::Detail {
                    sid: m.sid,
                    rk: m.rk.clone(),
                }),
            )));
        }
    }

    // ---- draw ----

    fn draw_header(&self, p: Painter, person: &Person, focus_elem: Option<u32>, measure: &dyn plx_machine::machine::Measure) {
        let flow = self.header;
        let d = flow.exp_d;
        let portrait = Rect::new(MARGIN_X, flow.portrait_y, d, d);
        plx_ui::widgets::card(
            p,
            portrait,
            Art::Person {
                sid: person.sid.raw(),
                key: &person.thumb,
                res: PORTRAIT_RES,
            },
            d * 0.5,
            false,
            1.0,
            0.0,
        );

        let col_x_ = col_x(d);
        let truncated = self.bio_more();
        let marked = self.bio_marked(focus_elem);
        let pending = plx_data::person::facts_pending(person);
        // The bio block (plate, shadow and text) is drawn FIRST so its plate sits under the name,
        // roles and life lines above it, as the old fixed highlight did. A resting bio never
        // measures for a plate: the lift block is skipped until the lift has a factor.
        if let Some(by) = flow.bio_y.filter(|_| !pending) {
            let bio = bio_view(&person.bio, 1.0, measure);
            let bh = bio.measure_h(BIO_W);
            let draw_bio = |p: Painter| {
                bio.draw(p, Rect::new(col_x_, by, BIO_W, 0.0));
                if truncated {
                    bio.draw_more(p, col_x_, by, BIO_W, bh, marked);
                }
            };
            if self.bio_lift.factor() > 0.0 {
                let ink = bio.last_line_cap_y(by, bh) - by + measure.cap_h(theme::size::BODY);
                let plate = Rect::new(
                    col_x_ - HL_PAD_X,
                    by - HL_PAD_Y,
                    BIO_W + 2.0 * HL_PAD_X,
                    ink + 2.0 * HL_PAD_Y,
                );
                plx_ui::text_lift::draw_focused(
                    p,
                    plate,
                    plx_ui::widgets::TEXT_BLOCK_HL_RAD,
                    &self.bio_lift,
                    plx_ui::text_lift::CENTRE,
                    draw_bio,
                );
            } else {
                draw_bio(p);
            }
        }

        Label::new(
            self.name_c.as_ptr(),
            theme::size::DISPLAY,
            theme::TEXT_PRIMARY,
        )
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(col_x_, flow.name_y, 0.0, 0.0));

        let phase = plx_ui::widgets::skeleton_phase(self.spin_ms as u32);
        // Roles and life-facts both draw through `widgets::dotted_run` — words in
        // `TEXT_SECONDARY`, the `·` in `TEXT_SEPARATOR` — rather than one pre-joined `Label`, so
        // the dot can carry its own ink (design: "Actor · Writer · Producer",
        // "Born … · <birthplace> · Died …"). `theme::space::XS` is the same pad idiom
        // `person_bio.rs`'s identity line and `detail.rs`'s facts row already pass it.
        for (y, parts, sz, w) in [
            (flow.meta_y, &person.roles, theme::size::LABEL, 0.42),
            (flow.life_y, &self.life_parts, theme::size::CAPTION, 0.68),
        ] {
            let Some(y) = y else { continue };
            if pending {
                let h = measure.cap_h(sz);
                plx_ui::widgets::skeleton_bar(p, Rect::new(col_x_, y, BIO_W * w, h), phase);
            } else {
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                let (cap_top, _) = plx_gfx::text::text_cap_band(sz, 0);
                plx_ui::widgets::dotted_run(
                    p,
                    &refs,
                    col_x_,
                    y - cap_top,
                    sz,
                    theme::TEXT_SECONDARY,
                    theme::space::XS,
                );
            }
        }
        if pending {
            if let Some(by) = flow.bio_y {
                let h = measure.cap_h(theme::size::BODY);
                for (i, w) in [1.0, 1.0, 0.58].into_iter().enumerate() {
                    let ly = by + i as f32 * BIO_LEAD;
                    plx_ui::widgets::skeleton_bar(p, Rect::new(col_x_, ly, BIO_W * w, h), phase);
                }
            }
        }
        if let Some(y) = flow.entry_y {
            // Same x the name/roles/life column starts at (`col_x_`, above) — never bare
            // `MARGIN_X`, which is the pre-phase-7 regression `entry_rect`'s doc explains.
            let entry = self.entry_heading();
            let measured = entry.measure(measure);
            entry.draw(p, col_x_, y, f32::from(focus_elem == Some(ENTRY_ELEM)), &measured, measure);
        }
    }

    /// Shelf `kind`'s heading and count, drawn in the shelf block's own space (`lift` is how far
    /// the focused tile makes it rise); the tiles are the stack's.
    fn draw_shelf_heading(&self, p: Painter, kind: usize, lift: f32, measure: &dyn plx_machine::machine::Measure) {
        let hy = -lift;
        Label::new(
            shelf_title()[kind].as_ptr(),
            theme::size::HEADLINE,
            theme::TEXT_HEADING,
        )
        .bold()
        .v(VAlign::CapTop)
        .draw(p, Rect::new(MARGIN_X, hy, SCR_W, 0.0));
        if !self.shelf_count_c[kind].as_bytes().is_empty() {
            let tw = measure.width(shelf_title()[kind], theme::size::HEADLINE, true);
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
    }

    /// The skeleton / empty read-out where the first shelf would be, `y` being that shelf's top.
    fn draw_shelf_state(&self, p: Painter, env: &Env, person: &Person, y: f32) {
        if present(person).1 > 0 {
            return;
        }
        let band = Rect::new(0.0, y, SCR_W, CARD_H);
        if !person.landed {
            let phase = plx_ui::widgets::skeleton_phase(self.spin_ms as u32);
            plx_ui::widgets::skeleton_bar(
                p,
                Rect::new(MARGIN_X, y + TITLE_DY - 32.0, 214.0, 32.0),
                phase,
            );
            let cy = y + TITLE_DY + CARD_DY;
            for i in 0..5 {
                let cx_ = MARGIN_X + i as f32 * (CARD_W + GAP);
                plx_ui::widgets::skeleton_sheet(
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
            plx_platform::i18n::msg::browse_person_empty_c(),
            StatusKind::Empty,
        )
        .draw(env, p);
    }

}

/// Shelves, in flow order. Kind 0 = Movies, 1 = Shows.
fn shelf_title() -> [&'static std::ffi::CStr; NSHELF] { [plx_platform::i18n::msg::browse_kind_movies_c(), plx_platform::i18n::msg::browse_kind_shows_c()] }

// -------------------------------------------------------------------------------------------
// the sections / Machine / Screen
// -------------------------------------------------------------------------------------------

impl<H: ContentLike + PersonLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = ShelfCards<'a>;

    /// Everything `sections` reads: the store's content, the header's measure (a remeasure moves
    /// `layout_gen`), whether a person is on show and which of its facts offer the entry.
    fn revision(&self, cx: &Cx<'_, H>) -> u64 {
        let flags = self.person(cx).map_or(0, |p| {
            let (_, n) = present(p);
            1 | (n as u64) << 1 | u64::from(entry_reachable(p)) << 4 | u64::from(p.credited) << 5 | u64::from(p.landed) << 6
        });
        H::person(cx).revision().wrapping_mul(1_000_003).wrapping_add(self.layout_gen).wrapping_mul(128).wrapping_add(flags)
    }

    /// The remembered card of a Back has not landed yet (or the whole person has not).
    fn pending(&self, cx: &Cx<'_, H>, want: &u32) -> bool {
        if !self.return_pending {
            return false;
        }
        let Some(card) = self.card_for_elem(*want) else { return false };
        match self.person(cx) {
            None => true,
            Some(p) => plx_data::person::media_resolving(p, card.sid),
        }
    }

    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        let person = self.person(cx);
        let (kinds, n) = person.map_or(([0; NSHELF], 0), present);
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: HEADER_TOP + self.header.exp_h, focusable: person.is_some() }, HEADER_GROUP));
        out.push(SectionSpec::new(Sec::Entry, Kind::Custom { height: 0.0, focusable: person.is_some_and(entry_reachable) }, ENTRY_GROUP));
        out.push(SectionSpec::new(Sec::State, Kind::Custom { height: 0.0, focusable: false }, STATE_GROUP));
        for (pos, &kind) in kinds[..n].iter().enumerate() {
            let gap = if pos == 0 { BAND_GAP_TO_SHELF } else { SHELF_GAP };
            out.push(SectionSpec::new(Sec::Gap(kind), Kind::Custom { height: gap, focusable: false }, GAP_GROUP[kind]));
            out.push(SectionSpec::new(Sec::Shelf(kind), Kind::Shelf { style: &SHELF_STYLE, heading: SHELF_LABEL_H }, SHELF_GROUP[kind]));
        }
    }

    /// The entry-row-gone-under-focus fallback: the first PRESENT shelf's own head, else the header.
    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        out.extend((0..NSHELF).map(Sec::Shelf));
        out.push(Sec::Head);
    }

    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<ShelfCards<'a>> {
        match k {
            Sec::Shelf(kind) => self.person(cx).map(|person| ShelfCards { person, kind, cards: &self.cards }),
            _ => None,
        }
    }

    fn elem_of(&self, k: Sec) -> Option<u32> {
        match k {
            Sec::Head => Some(HEADER_ELEM),
            Sec::Entry => Some(ENTRY_ELEM),
            _ => None,
        }
    }

    /// The pill is part of the head's block: focusing it reveals the whole band.
    fn reveal_with(&self, k: Sec) -> Option<Sec> {
        (k == Sec::Entry).then_some(Sec::Head)
    }

    fn focus_rect(&self, cx: &Cx<'_, H>, k: Sec, section: Rect) -> Rect {
        match k {
            Sec::Head => self.header_rect(),
            Sec::Entry => self.entry_rect(cx.measure, section.y),
            _ => section,
        }
    }

    fn plain_group(&self, cx: &Cx<'_, H>, k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        let (extent, edge) = match k {
            // the zero-width anchor: DOWN projects to the first shelf tile without making the
            // drawn biography impossible to click
            Sec::Head => (self.header_anchor(), [EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop]),
            _ => {
                let down = if self.person(cx).is_some_and(|p| nshelves(p) > 0) { EdgeRule::Geometric } else { EdgeRule::Stop };
                (extent, [EdgeRule::Geometric, down, EdgeRule::Stop, EdgeRule::Stop])
            }
        };
        GroupSpec { id, kind: GroupKind::Free, seat: Seat::First, reachable: AxisMask::VERTICAL, edge, extent, len: 1, elem: ElemKind::Bare }
    }

    fn draw_heading(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, lift: f32) {
        if let Sec::Shelf(kind) = k {
            let p = f.painter.alpha(f.page_alpha).translate(0.0, r.y);
            self.draw_shelf_heading(p, kind, lift, f.measure);
        }
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, _scroll: f32) {
        let Some(person) = self.person(f.cx) else { return };
        let p = f.painter.alpha(f.page_alpha);
        let cur = f.focus.current.map(|k| k.elem);
        match k {
            Sec::Head => {
                // the focused band is never culled, as the column's own cull has it
                let top = r.y + HEADER_TOP;
                let held = matches!(cur.and_then(|e| self.locate(person, e)), Some(Located::Header | Located::Entry));
                if held || plx_ui::on_axis(top, self.header.exp_h, SCR_H, 0.0) {
                    self.draw_header(p.translate(0.0, top), person, cur, f.measure);
                }
            }
            Sec::State => self.draw_shelf_state(p, &Env::inert(), person, r.y + BAND_GAP_TO_SHELF),
            Sec::Entry | Sec::Gap(_) | Sec::Shelf(_) => {}
        }
    }
}

plx_ui::focusable_via_view!(PersonScreen, H: [ContentLike + PersonLike], view);

impl PersonScreen {
    pub fn focused_item<'a, H: PersonLike>(
        &self,
        focus: Option<plx_machine::machine::FocusKey<u32>>,
        cx: &Cx<'a, H>,
    ) -> Option<&'a PmsMovie> {
        self.page.focused_item(focus, cx)
    }

    pub fn bio_available(&self, view: plx_data::person::PersonView<'_>) -> bool {
        self.page.bio_available(view)
    }
}

impl<H: ContentLike + PersonLike> Machine<H> for PersonScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // The content the stack reads is current before it reads it for this event.
        let mut live = false;
        match ev {
            ScreenEvent::Tick(t) => live = self.page.pre_tick(t.dt(), cx),
            ScreenEvent::StoreChanged(ord, _) if *ord == plx_data::stores::StoreId::Person.ord() => {
                self.page.header_dirty = true;
                self.page.refresh_store_cache(cx);
            }
            _ => {}
        }
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(_, card)) => Some(card),
            _ => None,
        };
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Person(memory)) => {
                self.page.restore(memory);
                self.page.return_pending = true;
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if live {
                    self.page.post_tick(*t, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Enter(_) | ScreenEvent::Uncover => {
                // Unconditional, not gated on `self.person(cx).is_none()` (review round 1, P1):
                // `Close` and `Open` are both deferred `AppFx::Store` effects applied AFTER this
                // event is delivered, so a same-identity `Close` queued by an evicted/left body
                // earlier in this SAME drain (e.g. a stack eviction's `Unmount`, or a `PopTo`
                // that leaves a same-identity page) can land after this `Enter`/`Uncover` — and a
                // guard reading the store NOW would see the about-to-be-closed identity and skip
                // `Open`, leaving the store empty under a page with no pending re-request.
                // `person::open` is idempotent on the identity it already holds (an early return
                // before any generation bump / fetch-claim clear / rebuild — see its doc), so
                // requesting it every time is a genuine no-op whenever the store is already
                // correctly settled on `(self.sid, self.key)`, and issues the real request
                // whenever it is not.
                self.page.request_store(fx);
                fx.invalidate(Provenance::Nav);
                self.page.settle_return_pending(cx);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                let page = &mut self.page;
                if matches!(by, By::Dir | By::Pointer)
                    || page.person(cx).is_some_and(|person| page.locate(person, to.elem).is_some())
                {
                    page.return_pending = false;
                }
                if matches!(page.person(cx).and_then(|p| page.locate(p, to.elem)), Some(Located::Header))
                    && matches!(by, By::Dir | By::Pointer)
                {
                    page.header_marked = true;
                }
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                match self.page.person(cx).and_then(|p| self.page.locate(p, *e)) {
                    Some(Located::Header) => self.page.activate_header(cx, fx),
                    Some(Located::Entry) => self.page.activate_entry(cx, fx),
                    _ => {}
                }
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    self.page.open_card(elem, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => {
                if let Some(CardEvent::Hold(_)) = card {
                    fx.push(plx_machine::machine::Fx::App(AppFx::Content(
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
                    self.page.return_pending = false;
                }
                if *key == Key::Back {
                    fx.push(plx_machine::machine::Fx::App(AppFx::Content(
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
                self.page.return_pending = false;
                Handled::No
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Pointer { .. },
                ..
            }) => {
                Handled::No
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == plx_data::stores::StoreId::Person.ord() => {
                self.page.settle_return_pending(cx);
                Handled::Yes
            }
            ScreenEvent::WillLeave(Leave::ForGood) | ScreenEvent::Unmount => {
                // `&& !self.teardown_closed`: see the field. One teardown, one `Close`, even
                // though §3.4 delivers this arm twice and the queued command has not run yet.
                if self.page.person(cx).is_some() && !self.teardown_closed {
                    self.teardown_closed = true;
                    fx.push(plx_machine::machine::Fx::App(AppFx::Store(
                        plx_data::stores::StoreId::Person,
                        plx_data::stores::StoreCmd::Person(PersonCmd::Close),
                    )));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike + PersonLike> Screen<H> for PersonScreen {
    fn focused_card<'a>(&self, cx: &Cx<'a, H>, focus: Option<plx_machine::machine::FocusKey<u32>>, at: Option<At>) -> Option<FocusedCard<'a>> {
        let item = self.focused_item(focus, cx)?;
        let placed = focus.zip(at).and_then(|(key, at)| Focusable::<H>::place(self, &key.elem, cx, at));
        Some(FocusedCard::new(item, placed))
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<plx_machine::machine::FocusKey<u32>>) {
        self.stack.view(&self.page).redraw_focused(f, focus);
    }
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
    fn prepare(&mut self, _b: &mut plx_ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        if self.page.person(f.cx).is_none() {
            return;
        }
        self.page.amb.draw(p, Rect::FULL);
        // Stops register in z order, section by section in document order: the header band, the
        // Filmography pill, then each shelf's tiles (a shelf wholly off the screen registers none).
        self.stack.view(&self.page).paint(f);
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
        self.page.covered_ready_c
    }
    fn links(&self, out: &mut Vec<Link>) {
        let reach = self.page.reach;
        let kinds = &reach.kinds[..reach.n];
        let mut link = |from: Sec, dir: Dir, to: Sec| {
            if let (Some(from), Some(to)) = (self.stack.group(from), self.stack.group(to)) {
                out.push(Link { from, dir, to });
            }
        };
        let first = kinds.first().map(|&k| Sec::Shelf(k));
        if reach.entry {
            link(Sec::Head, Dir::Down, Sec::Entry);
            link(Sec::Entry, Dir::Up, Sec::Head);
            if let Some(first) = first {
                link(Sec::Entry, Dir::Down, first);
                link(first, Dir::Up, Sec::Entry);
            }
        } else if let Some(first) = first {
            link(Sec::Head, Dir::Down, first);
            link(first, Dir::Up, Sec::Head);
        }
        for pair in kinds.windows(2) {
            link(Sec::Shelf(pair[0]), Dir::Down, Sec::Shelf(pair[1]));
            link(Sec::Shelf(pair[1]), Dir::Up, Sec::Shelf(pair[0]));
        }
    }
    fn memory_at(&self, _focus: Option<plx_machine::machine::FocusKey<u32>>) -> PageMemory {
        PageMemory::Person(self.page.memory())
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::fixture::FixtureMeasure;
    use plx_ui::cards as ui_cards;
    use plx_ui::cards::StackMemory;
    use plx_ui::screen::Step;
    use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead, Tick};

    struct PersonHost;

    impl Host for PersonHost {
        type Arg = super::super::family::SettingsPage;
        type Fx = AppFx;
        type Msg = super::super::registry::AppMsg;
        type Elem = u32;
        type Views<'a> = plx_data::person::PersonView<'a>;
        type Init = super::super::family::NoInit;
        type Memory = PageMemory;
    }

    impl PersonLike for PersonHost {
        fn person<'a>(cx: &Cx<'a, Self>) -> plx_data::person::PersonView<'a> { cx.views }
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

    fn cx<'a>(m: &'a dyn Measure, person: plx_data::person::PersonView<'a>) -> Cx<'a, PersonHost> {
        Cx {
            views: person,
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    fn cx_at<'a>(m: &'a FixtureMeasure, person: plx_data::person::PersonView<'a>, focus: plx_machine::machine::FocusKey<u32>) -> Cx<'a, PersonHost> {
        Cx {
            focus: FocusRead {
                current: Some(focus),
            ..Default::default() },
            ..cx(m, person)
        }
    }

    /// Mounts through [`PersonScreen::new`] (which issues the real `PersonCmd::Open`) and then
    /// seeds the shelves the way a landing does — `install_for_test` ALSO settles `credited =
    /// true`, which is why every test below that wants a genuinely PENDING entry row reasons about
    /// [`entry_reachable_of`] directly instead (see that function's own doc for why no test seam
    /// can force `credited` back to `false` on a live `Person` from outside `plx_data::person`).
    fn seed(movies: usize, shows: usize) -> (plx_data::stores::person::PersonStore, PersonScreen) {
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open {
            sid: ServerId::UNSET,
            key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(),
            name: "Idina Menzel".into(),
            thumb: String::new(),
        });
        let mut s = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        store.install_for_test(
            (0..movies).map(|i| item(&format!("m{i}"))).collect(),
            (0..shows).map(|i| item(&format!("s{i}"))).collect(),
        );
        s.page.refresh_store_cache(&cx(&FixtureMeasure, store.view()));
        lay(&mut s, &store, &FixtureMeasure);
        (store, s)
    }

    fn focus_of(s: &PersonScreen, store: &plx_data::stores::person::PersonStore, kind: usize, col: usize) -> plx_machine::machine::FocusKey<u32> {
        s.page.shelf_key(store.view().current().unwrap(), kind, col)
    }

    /// Feed `ev` to the screen with the engine focus on `focus`.
    fn feed(
        s: &mut PersonScreen,
        store: &plx_data::stores::person::PersonStore,
        m: &dyn Measure,
        focus: Option<plx_machine::machine::FocusKey<u32>>,
        ev: ScreenEvent<PersonHost>,
    ) {
        let mut present = plx_machine::present::Present::new();
        let mut buf: Vec<plx_machine::machine::Stamped<PersonHost>> = Vec::new();
        let cxv = Cx { focus: FocusRead { current: focus, ..Default::default() }, ..cx(m, store.view()) };
        let mut fx = Effects::new(
            &mut buf,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(s, &ev, &cxv, &mut fx);
    }

    /// Let the stack lay its sections out over what the page now holds (the stack lays out on the
    /// first event it sees, and again on the next one after the page's content moved).
    fn lay(s: &mut PersonScreen, store: &plx_data::stores::person::PersonStore, m: &dyn Measure) {
        feed(s, store, m, None, ScreenEvent::Cover);
    }

    /// Tick the page `frames` times with the engine focus on `focus`, so a focused tile has grown
    /// to its settled pop and the page has scrolled to its target.
    fn settle_shelves(
        s: &mut PersonScreen,
        store: &plx_data::stores::person::PersonStore,
        focus: plx_machine::machine::FocusKey<u32>,
        frames: u32,
    ) {
        for _ in 0..frames {
            feed(s, store, &FixtureMeasure, Some(focus), ScreenEvent::Tick(Tick { ms: 0, dt_us: 16_667 }));
        }
    }

    /// Every stop the page registers, in z order — what `Screen::draw` registers without painting.
    fn page_stops(s: &PersonScreen, f: &mut DrawFrame<'_, '_, PersonHost>) {
        s.stack.view(&s.page).record_stops(f);
    }

    /// Put the page's scroll at `scroll`, without gliding there.
    fn jump_to(s: &mut PersonScreen, scroll: f32) {
        s.stack.restore(&StackMemory { scroll, shelves: Vec::new() });
    }

    /// The Filmography pill's focus/hit rect where the page now has it.
    fn entry_rect_of(s: &PersonScreen, store: &plx_data::stores::person::PersonStore, m: &dyn Measure) -> Rect {
        Focusable::<PersonHost>::place(s, &ENTRY_ELEM, &cx(m, store.view()), At::Drawn).unwrap().rect
    }

    #[test]
    #[cfg(feature = "devtriggers")]
    fn populated_person_geometry_uses_recorded_metrics() {
        let _serial = plx_base::testlock::serial();
        // Armed in a private trigger root: the shared runtime root is the bare `/tmp` on a host test
        // run, so the old `assert!(!path.exists(), "needs an isolated runtime root")` was only as
        // true as the other processes on the machine were quiet.
        let (mut store, mut s) = plx_base::devtrig::with_private_triggers(|| {
            let path = plx_base::devtrig::path("personbio");
            std::fs::write(&path, "A populated biography whose words must pass through the recorded measurement capability. ".repeat(60)).unwrap();
            let (mut store, s) = seed(3, 2);
            store.install_credits_for_test(&[("Actor", 9)]);
            (store, s)
        });
        assert!(!store.view().current().unwrap().bio.is_empty());
        plx_ui::rec::assert_measured_geometry(|measure| {
            s.page.remeasure_header(store.view().current().unwrap(), measure);
            lay(&mut s, &store, measure);
            let context = cx(measure, store.view());
            let mut groups = Vec::new();
            Focusable::<PersonHost>::groups(&s, &context, &mut groups);
            assert_eq!(groups.len(), 4);
            let mut bits = Vec::new();
            for g in groups {
                bits.extend([g.extent.x, g.extent.y, g.extent.w, g.extent.h].map(f32::to_bits));
            }
            for key in [HEADER_ELEM, ENTRY_ELEM, focus_of(&s, &store, 0, 0).elem, focus_of(&s, &store, 1, 0).elem] {
                for at in [At::Drawn, At::SpringTarget] {
                    let p = Focusable::<PersonHost>::place(&s, &key, &context, at).unwrap();
                    bits.extend([p.rect.x, p.rect.y, p.rect.w, p.rect.h].map(f32::to_bits));
                }
            }
            bits
        });
        store.run(PersonCmd::Close);
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
        let _serial = plx_base::testlock::serial();
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open {
            sid: ServerId::UNSET, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new(),
        });
        let mut s = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        let p = store.view().current().expect("Open seeds a pending Person synchronously");
        assert!(
            plx_data::person::facts_pending(p),
            "a fresh mount must start pending, or this test is not exercising the skeleton clock"
        );
        let m = FixtureMeasure;
        let cxv = cx(&m, store.view());
        let mut present = plx_machine::present::Present::new();
        let _ = present.take(0);
        let mut buf: Vec<plx_machine::machine::Stamped<PersonHost>> = Vec::new();
        for ms in [16, 32, 48] {
            let mut fx = Effects::new(
                &mut buf,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
                &mut present,
            );
            let ev = ScreenEvent::Tick(Tick { ms, dt_us: 16_667 });
            Machine::<PersonHost>::step(&mut s, &ev, &cxv, &mut fx);
            assert!(
                present.take(ms),
                "a pending header skeleton must present every frame it is on screen (ms={ms})"
            );
        }
        store.run(PersonCmd::Close);
    }

    #[test]
    fn enter_and_leave_emit_addressed_person_store_commands() {
        let mut screen = PersonScreen::new(
            EntryId(0), ServerId::from_raw(2), "161".into(), "person-guid".into(),
            "Person Name".into(), "thumb".into());
        let measure = FixtureMeasure;
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        {
            let context = cx(&measure, plx_data::person::PersonView::default());
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(
                plx_machine::machine::InstanceId(0)), &mut present);
            Machine::<PersonHost>::step(
                &mut screen, &ScreenEvent::Enter(Enter::Restored), &context, &mut fx);
        }
        assert!(matches!(&out[0].fx,
            plx_machine::machine::Fx::App(AppFx::Store(plx_data::stores::StoreId::Person,
                plx_data::stores::StoreCmd::Person(PersonCmd::Open { sid, key, guid, name, thumb })))
                if *sid == ServerId::from_raw(2) && key == "161" && guid == "person-guid"
                    && name == "Person Name" && thumb == "thumb"));

        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid: ServerId::from_raw(2), key: "161".into(),
            guid: "person-guid".into(), name: "Person Name".into(), thumb: "thumb".into() });
        out.clear();
        {
            let context = cx(&measure, store.view());
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(
                plx_machine::machine::InstanceId(0)), &mut present);
            Machine::<PersonHost>::step(
                &mut screen, &ScreenEvent::WillLeave(Leave::ForGood), &context, &mut fx);
        }
        assert!(matches!(&out[0].fx,
            plx_machine::machine::Fx::App(AppFx::Store(plx_data::stores::StoreId::Person,
                plx_data::stores::StoreCmd::Person(PersonCmd::Close)))));
        assert!(store.view().current().is_some(),
            "the screen emits; only the addressed Bridge is allowed to apply the command");
    }

    /// §3.4's `pop_sequence` delivers `WillLeave(ForGood)` AND `Unmount` to the same body before
    /// either event's queued effects have run (`AppFx::Store` is deferred to the addressed
    /// Bridge). Mirrors the `#119`/`teardown_cleared` regression in `screens/detail/mod.rs`: an
    /// un-latched `self.person(cx).is_some()` guard reads the store as still populated on the
    /// second event and emits a SECOND non-idempotent `Close` (#126).
    #[test]
    fn teardown_closes_the_person_store_exactly_once() {
        let mut screen = PersonScreen::new(
            EntryId(0), ServerId::from_raw(2), "161".into(), "person-guid".into(),
            "Person Name".into(), "thumb".into());
        let measure = FixtureMeasure;
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid: ServerId::from_raw(2), key: "161".into(),
            guid: "person-guid".into(), name: "Person Name".into(), thumb: "thumb".into() });
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let context = cx(&measure, store.view());
        {
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(
                plx_machine::machine::InstanceId(0)), &mut present);
            Machine::<PersonHost>::step(
                &mut screen, &ScreenEvent::WillLeave(Leave::ForGood), &context, &mut fx);
            // The queued Close has not been applied to `store` yet — it is still populated when
            // Unmount arrives, exactly as `pop_sequence` delivers it.
            Machine::<PersonHost>::step(
                &mut screen, &ScreenEvent::Unmount, &context, &mut fx);
        }
        let closes = out.iter().filter(|s| matches!(&s.fx,
            plx_machine::machine::Fx::App(AppFx::Store(plx_data::stores::StoreId::Person,
                plx_data::stores::StoreCmd::Person(PersonCmd::Close))))).count();
        assert_eq!(closes, 1,
            "WillLeave(ForGood) then Unmount must close the Person store exactly once, not twice");
    }

    /// A bare `Unmount` (no preceding `WillLeave`) must also close the store exactly once — the
    /// latch must not suppress the only teardown event when there is no pair.
    #[test]
    fn unmount_alone_closes_the_person_store_once() {
        let mut screen = PersonScreen::new(
            EntryId(0), ServerId::from_raw(2), "161".into(), "person-guid".into(),
            "Person Name".into(), "thumb".into());
        let measure = FixtureMeasure;
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid: ServerId::from_raw(2), key: "161".into(),
            guid: "person-guid".into(), name: "Person Name".into(), thumb: "thumb".into() });
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let context = cx(&measure, store.view());
        {
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(
                plx_machine::machine::InstanceId(0)), &mut present);
            Machine::<PersonHost>::step(
                &mut screen, &ScreenEvent::Unmount, &context, &mut fx);
        }
        let closes = out.iter().filter(|s| matches!(&s.fx,
            plx_machine::machine::Fx::App(AppFx::Store(plx_data::stores::StoreId::Person,
                plx_data::stores::StoreCmd::Person(PersonCmd::Close))))).count();
        assert_eq!(closes, 1, "a bare Unmount must close the Person store exactly once");
    }

    /// **The mount-on-entry-pill rule's PENDING half** (module doc, point 2 of `ui/person.rs`'s
    /// own doc): with no filmography answered yet but a guid to ask plex.tv with,
    /// `entry_reachable_of` holds the entry group open even though `has_entry_of` is false — which
    /// is exactly what lets a fresh mount's default `FirstInGroup(GroupId(0))` target
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
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(1, 0);
        let p = store.view().current().unwrap();
        assert!(!has_entry(p), "no credits were installed");
        assert!(
            !entry_reachable(p),
            "credited=true (install_for_test) with zero total: settled and empty"
        );
        let m = FixtureMeasure;
        let want = plx_machine::machine::FocusKey {
            entry: EntryId(0),
            elem: ENTRY_ELEM,
        };
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m, store.view()));
        assert_eq!(
            got,
            focus_of(&s, &store, 0, 0),
            "falls to the first present shelf's own head"
        );
        let _ = &mut s;
        store.run(PersonCmd::Close);
    }

    /// **`focused_item`/`focused_target` split** — mining `ui/person.rs`'s own regression: the
    /// header/entry rows hold no card at all, so a shelf tile's identity is the only thing OK
    /// should ever navigate to.
    #[test]
    fn focused_movie_answers_only_for_a_shelf_row() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(2, 0);
        assert!(s
            .focused_item(Some(plx_machine::machine::FocusKey {
                entry: s.page.entry,
                elem: HEADER_ELEM
            }), &cx(&FixtureMeasure, store.view()))
            .is_none());
        assert!(s
            .focused_item(Some(plx_machine::machine::FocusKey {
                entry: s.page.entry,
                elem: ENTRY_ELEM
            }), &cx(&FixtureMeasure, store.view()))
            .is_none());
        assert_eq!(
            s.focused_item(Some(focus_of(&s, &store, 0, 0)), &cx(&FixtureMeasure, store.view()))
                .map(|m| m.rk.as_str()),
            Some("m0")
        );
        assert_eq!(
            s.focused_item(Some(focus_of(&s, &store, 0, 1)), &cx(&FixtureMeasure, store.view()))
                .map(|m| m.rk.as_str()),
            Some("m1")
        );
        let _ = &mut s;
        store.run(PersonCmd::Close);
    }

    /// A shelf that has vanished under the focus re-seats by IDENTITY first (the merge-redivide
    /// case), and only falls back to a plain index clamp when no identity is held — the two-step
    /// order `reconcile` must keep (module doc's worked trace).
    #[test]
    fn reconcile_reseats_by_identity_before_falling_back_to_index_clamp() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(4, 0);
        let want = focus_of(&s, &store, 0, 2);
        // the row is rebuilt with two items inserted ahead — "m2" is now at index 4
        let rebuilt: Vec<PmsMovie> = ["x0", "x1", "m0", "m1", "m2", "m3"]
            .iter()
            .map(|rk| item(rk))
            .collect();
        store.install_for_test(rebuilt, Vec::new());
        let m = FixtureMeasure;
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m, store.view()));
        assert_eq!(
            got, want,
            "the engine key follows the card without a screen-owned cursor"
        );
        assert_eq!(
            s.page.locate(store.view().current().unwrap(), got.elem),
            Some(Located::Shelf(0, 4))
        );

        // the identity is gone entirely: falls back to clamping the SAME kind's own bounds
        store.install_for_test(vec![item("only")], Vec::new());
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m, store.view()));
        assert_eq!(got, focus_of(&s, &store, 0, 0));
        let _ = &mut s;
        store.run(PersonCmd::Close);
    }

    /// A shelf that disappears ENTIRELY (not merely shrinks) hands focus to the other present
    /// shelf, never leaving it pointed at a kind with nothing in it.
    #[test]
    fn a_vanished_shelf_kind_falls_back_to_the_other_present_shelf() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(2, 3);
        let want = focus_of(&s, &store, 1, 2);
        store.install_for_test(vec![item("m0")], Vec::new()); // shows vanished
        let m = FixtureMeasure;
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&m, store.view()));
        assert_eq!(
            got,
            focus_of(&s, &store, 0, 0),
            "movies is the only present shelf left"
        );
        let _ = &mut s;
        store.run(PersonCmd::Close);
    }

    /// **Regression: the Filmography entry pill must draw in the TEXT column, not under the
    /// portrait.** Before phase-7 (`894f20f8^:rust-modules/src/ui/person.rs`) `draw_entry` was
    /// called at `col_x`, the same x the name/roles start at; the phase-7 port dropped that and
    /// drew — and hit-tested — the pill at bare `MARGIN_X` instead, under the circular portrait.
    /// `entry_rect` is the one seam both the draw call and the focus/hit rect read, so fixing it
    /// here fixes both at once.
    #[test]
    fn the_entry_pill_sits_in_the_text_column_not_under_the_portrait() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(1, 0);
        store.install_credits_for_test(&[("Actor", 3)]);
        let m = FixtureMeasure;
        let p = store.view().current().unwrap();
        s.page.remeasure_header(p, &m);
        assert_eq!(
            s.page.header.exp_d, PORTRAIT_EXP,
            "the header must be in its expanded band for this assertion to mean anything"
        );
        lay(&mut s, &store, &m);
        let rect = entry_rect_of(&s, &store, &m);
        assert_eq!(
            rect.x,
            col_x(PORTRAIT_EXP),
            "the pill must sit in the text column, exactly where the name/roles start"
        );
        store.run(PersonCmd::Close);
    }

    /// The Filmography entry follows the biography's text column, and in the compact header with
    /// no biography it stays in the centred identity stack.
    #[test]
    fn person_filmography_entry_follows_the_biography_text_column() {
        let measure = FixtureMeasure;
        let long = "A long biography whose visible lines must all remain above Filmography. ".repeat(60);
        for (pending, bio) in [(false, "Short biography."), (false, long.as_str()), (true, "")] {
            let flow = header_flow(pending, bio, true, true, true, &measure);
            let entry_y = flow.entry_y.expect("credits offer the entry");
            let biography_top = flow.bio_y.expect("biography or its placeholder is present");
            let biography_h = if pending { BIO_LEAD * BIO_LINES as f32 }
                else { bio_view(bio, 1.0, &measure).measure_h(BIO_W) };
            assert!(col_x(flow.exp_d) >= MARGIN_X + flow.exp_d + BAND_GAP,
                "the action must sit under the text, not the portrait");
            assert!((entry_y - biography_top - biography_h - ENTRY_GAP).abs() < 0.01);
            assert!(flow.exp_h >= entry_y + LinkedHeading::HEIGHT, "the next shelf clears the action");
        }
        let bare = header_flow(false, "", false, false, true, &measure);
        let entry_y = bare.entry_y.unwrap();
        assert!((entry_y - bare.name_y - measure.cap_h(theme::size::DISPLAY) - ENTRY_GAP).abs() < 0.01,
            "without biography the centered identity and action remain one text stack");
        assert!(bare.exp_h >= entry_y + LinkedHeading::HEIGHT);
    }

    /// Every shipped translation of the entry, and the expanded pseudo-locale, fits at rest and
    /// focused inside the safe frame at its existing size.
    #[test]
    fn person_filmography_entry_keeps_translated_labels_and_focus_inside_the_safe_frame() {
        let measure = FixtureMeasure;
        let flow = header_flow(false, "A short biography.", true, false, true, &measure);
        let mut labels = Vec::new();
        for preference in [plx_platform::i18n::Preference::En, plx_platform::i18n::Preference::Es, plx_platform::i18n::Preference::Be] {
            let locale = plx_platform::i18n::LocaleContext::resolve(preference, None, None, None, None);
            labels.push(plx_platform::i18n::msg::browse_person_filmography_in(&locale).to_owned());
        }
        labels.push("[!! Fïlmöögrááphy !!]".to_owned());
        for label in &labels {
            let entry = LinkedHeading::entry(label, "9 223 372 036 854 775 807");
            let measured = entry.measure(&measure);
            let y = HEADER_TOP + flow.entry_y.unwrap();
            for focus in [0.0, 1.0] {
                assert!(inside_safe(entry.face_rect(col_x(flow.exp_d), y, focus, &measured)),
                    "{label:?} at focus {focus} must keep the safe inset");
            }
        }
    }

    /// The entry stop is appended after the shelf stops, so a stale unscrolled pill rectangle
    /// wins their overlap in `HitMap`'s last-stop z-order. Put a first-shelf tile exactly under
    /// that stale rectangle: the live, scrolled entry geometry must leave the click with the tile.
    #[test]
    fn a_scrolled_filmography_stop_does_not_steal_the_first_shelf_tile() {
        use plx_ui::hit::{HitMap, PointerKind};
        use plx_ui::screen::DrawFrame;

        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(2, 0);
        store.install_credits_for_test(&[("Actor", 3)]);
        let measure = FixtureMeasure;
        s.page.refresh_store_cache(&cx(&measure, store.view()));
        s.page.remeasure_header(store.view().current().unwrap(), &measure);
        s.page.header.exp_d = PORTRAIT_BARE;
        lay(&mut s, &store, &measure);
        let key = focus_of(&s, &store, 0, 1);
        settle_shelves(&mut s, &store, key, 120);

        let stale_entry = entry_rect_of(&s, &store, &measure);
        let initial_tile = Focusable::<PersonHost>::place(
            &s, &key.elem, &cx(&measure, store.view()), At::Drawn)
            .expect("the seeded first-shelf tile is drawn");
        let scroll = initial_tile.rect.cy() - stale_entry.cy();
        assert!(scroll > 0.0, "the shelf starts below Filmography");
        jump_to(&mut s, scroll);

        let context = cx(&measure, store.view());
        let tile = Focusable::<PersonHost>::place(&s, &key.elem, &context, At::Drawn)
            .expect("the scrolled first-shelf tile remains drawn");
        let stale_overlap = stale_entry.intersect(tile.rect);
        assert!(stale_overlap.w > 0.0 && stale_overlap.h > 0.0,
            "the fixture must reproduce the stale pill/tile overlap");
        let (x, y) = (stale_overlap.cx(), stale_overlap.cy());
        let mut frame = DrawFrame::new(&context, plx_ui::Painter::root());
        page_stops(&s, &mut frame);
        let mut hit = HitMap::new();
        hit.fill(frame.into_stops());
        hit.swap();
        let resolved = hit.resolve(Some(s.page.entry), PointerKind::Click, x, y, None);
        assert_eq!(resolved.hit, Some(key), "the tile must remain above no stale pill stop");

        let entry = entry_rect_of(&s, &store, &measure);
        let past = s.stack.scroll() + entry.y + entry.h + 1.0;
        jump_to(&mut s, past);
        let context = cx(&measure, store.view());
        let mut frame = DrawFrame::new(&context, plx_ui::Painter::root());
        page_stops(&s, &mut frame);
        assert!(frame.stops().iter().all(|stop| stop.key.elem != ENTRY_ELEM),
            "an entry wholly above the viewport registers no pointer stop");
        store.run(PersonCmd::Close);
    }

    /// **Header pending vs answered-with-nothing vs answered-fully** — `header_flow`'s three
    /// states, mined from `ui/person.rs`'s own module doc ("Every header line below the name is
    /// optional… a header line is drawn only when it has content").
    #[test]
    fn header_flow_distinguishes_pending_from_answered_empty_from_answered_full() {
        let m = FixtureMeasure;
        // answered, with nothing: no reserved space for the meta/life/bio lines at all.
        let flow = header_flow(false, "", false, false, false, &m);
        assert!(flow.meta_y.is_none() && flow.life_y.is_none() && flow.bio_y.is_none());

        // pending: not yet asked at all — reserves the full placeholder stack even though every
        // cached run is still empty.
        let pending_flow = header_flow(true, "", false, false, false, &m);
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
        let full_flow = header_flow(false, "", true, false, false, &m);
        assert!(full_flow.meta_y.is_some());
        assert!(full_flow.life_y.is_none(), "no life facts were given");
    }

    /// **The `MORE` gate is the header's measured answer, not a per-frame wrap.** The set's
    /// person-page stack profile (2026-09-19) caught `draw_header` asking `bio_is_truncated` —
    /// a fresh `TextView` wrap of the whole bio, `TTF_SizeUTF8` per word — on every frame. The
    /// mark, OK's gate and `bio_available` now all read `bio_more`, which is `header_flow`'s
    /// `bio.truncates(BIO_W)` from the last remeasure, and it takes no measure capability at all.
    #[test]
    fn the_more_gate_reads_the_header_measure_not_a_fresh_wrap() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(1, 0);
        let m = FixtureMeasure;
        let long = "A biography far longer than the header's three lines can hold. ".repeat(40);
        for (bio, want) in [(long.as_str(), true), ("One short line.", false)] {
            let flow = header_flow(false, bio, false, false, false, &m);
            assert_eq!(flow.bio_truncated, bio_view(bio, 1.0, &m).truncates(BIO_W));
            assert_eq!(flow.bio_truncated, want, "fixture sanity for {:.20}", bio);
            s.page.header = flow;
            s.page.header_dirty = false;
            assert_eq!(s.page.bio_more(), want);
        }
        s.page.header = header_flow(false, &long, false, false, false, &m);
        s.page.header_dirty = true;
        assert!(!s.page.bio_more(), "a header awaiting its remeasure offers no panel yet");
        store.run(PersonCmd::Close);
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
        let _serial = plx_base::testlock::serial();
        let (mut store, s) = seed(0, 0);
        assert!(s
            .focused_item(Some(plx_machine::machine::FocusKey {
                entry: s.page.entry,
                elem: HEADER_ELEM
            }), &cx(&FixtureMeasure, store.view()))
            .is_none());
        assert!(s
            .focused_item(Some(plx_machine::machine::FocusKey {
                entry: s.page.entry,
                elem: ENTRY_ELEM
            }), &cx(&FixtureMeasure, store.view()))
            .is_none());
        assert!(s
            .focused_item(Some(plx_machine::machine::FocusKey {
                entry: s.page.entry,
                elem: FIRST_CARD_ELEM
            }), &cx(&FixtureMeasure, store.view()))
            .is_none());
        let _ = &s;
        store.run(PersonCmd::Close);
    }

    /// `PressCommit` leaves immediately through the content contract; there is no pending latch.
    #[test]
    fn press_commit_on_a_card_pushes_detail_as_an_effect() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(2, 0);
        let m = FixtureMeasure;
        let cxv = cx(&m, store.view());
        let mut present = plx_machine::present::Present::new();
        let mut buf: Vec<plx_machine::machine::Stamped<PersonHost>> = Vec::new();
        {
            let mut fx = Effects::new(
                &mut buf,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
                &mut present,
            );
            Machine::<PersonHost>::step(&mut s, &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)), &cxv, &mut fx);
        }
        assert!(buf.is_empty(), "no focus means no navigation effect");
        // Feed the identical `Cx` shape but with focus parked on the first movie tile.
        let cxv2 = Cx {
            views: store.view(),
            tick: Tick::default(),
            measure: &m,
            press: PressRead::default(),
            focus: FocusRead {
                current: Some(focus_of(&s, &store, 0, 0)),
            ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)),
        };
        {
            let mut fx = Effects::new(
                &mut buf,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
                &mut present,
            );
            Machine::<PersonHost>::step(&mut s, &ScreenEvent::PressCommit(plx_machine::machine::PressId(1)), &cxv2, &mut fx);
        }
        assert!(buf.iter().any(|st| matches!(
            &st.fx,
            plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Push(ContentArg::Detail { sid, rk })))
                if *sid == ServerId::UNSET && rk == "m0"
        )));
        store.run(PersonCmd::Close);
    }

    /// An explicit D-pad/pointer arrival on the header marks it (the bio truncation mark may show
    /// from then on); a landing that merely leaves the header as the resolved position (`Restore`)
    /// must not.
    #[test]
    fn focus_moved_marks_the_header_only_on_an_explicit_arrival() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(1, 0);
        let header_key = plx_machine::machine::FocusKey {
            entry: EntryId(0),
            elem: HEADER_ELEM,
        };
        let m = FixtureMeasure;
        let cxv = cx(&m, store.view());
        let mut present = plx_machine::present::Present::new();
        let mut buf: Vec<plx_machine::machine::Stamped<PersonHost>> = Vec::new();
        let mut fx = Effects::new(
            &mut buf,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        assert!(!s.page.header_marked);
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
            !s.page.header_marked,
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
            s.page.header_marked,
            "an explicit D-pad press onto the header marks it"
        );
        store.run(PersonCmd::Close);
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
        let _serial = plx_base::testlock::serial();
        let (mut store, mut a) = seed(1, 0);
        let mut b = PersonScreen::new(
            EntryId(0),
            ServerId::UNSET,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        b.page.cards = a.page.cards.clone();
        lay(&mut b, &store, &FixtureMeasure);
        assert_eq!(
            hash(&a),
            hash(&b),
            "identical machine state hashes identically"
        );

        b.page.cards.next += 1;
        assert_ne!(hash(&a), hash(&b), "the future allocator is part of state");
        b.page.cards.next = a.page.cards.next;
        b.page.cards.keys[0].elem += 1;
        assert_ne!(hash(&a), hash(&b), "the identity registry is part of state");
        b.page.cards = a.page.cards.clone();
        b.page.return_pending = true;
        assert_ne!(
            hash(&a),
            hash(&b),
            "return hydration changes future reconciliation even when the page draws identically"
        );
        let _ = &mut a;
        store.run(PersonCmd::Close);
    }

    /// Entry eviction preserves the identity interner in PageMemory. A reordered landing after
    /// remount therefore resolves the old engine element to the same movie, not merely the same
    /// numeric slot.
    #[test]
    fn evict_remount_with_reordered_shelf_preserves_movie_identity() {
        let _serial = plx_base::testlock::serial();
        let (mut store, original) = seed(2, 0);
        let old_focus = focus_of(&original, &store, 0, 1);
        assert_eq!(
            original
                .focused_item(Some(old_focus), &cx(&FixtureMeasure, store.view()))
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
        store.install_for_test(vec![item("m1"), item("m0")], Vec::new());
        remounted.page.sync_card_keys(&cx(&FixtureMeasure, store.view()));
        let measure = FixtureMeasure;
        let restored = Focusable::<PersonHost>::reconcile(
            &remounted, old_focus, &cx(&measure, store.view()));
        assert_eq!(restored.elem, old_focus.elem);
        assert_eq!(
            remounted
                .focused_item(Some(restored), &cx(&FixtureMeasure, store.view()))
                .map(|m| m.rk.as_str()),
            Some("m1")
        );
        store.run(PersonCmd::Close);
    }

    #[test]
    fn restoring_a_frozen_registry_never_rewinds_keys_minted_after_the_snapshot() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut screen) = seed(1, 0);
        let frozen = screen.page.memory();
        store.install_for_test(vec![item("m0"), item("newer")], Vec::new());
        screen.page.sync_card_keys(&cx(&FixtureMeasure, store.view()));
        let newer_elem = screen
            .page.elem_for(&store.view().current().unwrap().shelf(0)[1])
            .expect("the live body interned the later landing");
        let live_next = screen.page.cards.next;

        screen.restore(&frozen);

        assert_eq!(
            screen.page.elem_for(&store.view().current().unwrap().shelf(0)[1]),
            Some(newer_elem),
            "request-time memory merges into a live interner instead of replacing it"
        );
        assert_eq!(screen.page.cards.next, live_next);
        store.run(PersonCmd::Close);
    }

    fn pending_share_return(
        measure: &FixtureMeasure,
    ) -> (
        plx_data::stores::person::PersonStore,
        PersonScreen,
        plx_machine::machine::FocusKey<u32>,
        ServerId,
        ServerId,
    ) {
        plx_plex::plex::reset_servers_for_test();
        let origin =
            plx_plex::plex::register_for_test("person-pending-origin", "127.0.0.1", 1, "a", "cid");
        let share =
            plx_plex::plex::register_for_test("person-pending-share", "127.0.0.1", 2, "b", "cid");
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid: origin, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        let mut original = PersonScreen::new(
            EntryId(0),
            origin,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        store.install_source_for_test(share, vec![item_on(share, "wanted")], Vec::new());
        original.page.sync_card_keys(&cx(measure, store.view()));
        let old_focus = focus_of(&original, &store, 0, 0);
        let PageMemory::Person(memory) =
            Screen::<PersonHost>::memory_at(&original, Some(old_focus))
        else {
            panic!("Person must persist its interner through PageMemory::Person");
        };

        store.run(PersonCmd::Close);
        let mut returned = PersonScreen::new(
            EntryId(0),
            origin,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        returned.restore(&memory);
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::RestoreMemory(PageMemory::Person(memory)),
            &cx_at(measure, store.view(), old_focus),
            &mut fx,
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::Enter(Enter::Restored),
            &cx_at(measure, store.view(), old_focus),
            &mut fx,
        );
        store.run(PersonCmd::Open { sid: origin, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        store.install_source_for_test(
            origin,
            vec![item_on(origin, "available")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 1),
            &cx_at(measure, store.view(), old_focus),
            &mut fx,
        );
        assert!(returned.page.return_pending);
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&returned, old_focus, &cx_at(measure, store.view(), old_focus)),
            old_focus
        );
        (store, returned, old_focus, origin, share)
    }

    #[test]
    fn a_direction_abandons_an_unavailable_return_card_and_allows_fallback() {
        let _serial = plx_base::testlock::serial();
        let measure = FixtureMeasure;
        let (mut store, mut returned, old_focus, _origin, _share) = pending_share_return(&measure);
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        let right = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: plx_machine::machine::Source::Replay,
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
                &cx_at(&measure, store.view(), old_focus),
                &mut fx,
            ),
            Handled::No,
            "the engine still owns directional movement"
        );
        assert!(!returned.page.return_pending);
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&returned, old_focus, &cx_at(&measure, store.view(), old_focus)),
            focus_of(&returned, &store, 0, 0),
            "the same frame's dispatcher reconcile can seat the available card"
        );
        store.run(PersonCmd::Close);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn a_click_abandons_an_unavailable_return_card() {
        let _serial = plx_base::testlock::serial();
        let measure = FixtureMeasure;
        let (mut store, mut returned, old_focus, _origin, _share) = pending_share_return(&measure);
        let available = focus_of(&returned, &store, 0, 0);
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        let click = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: plx_machine::machine::Source::Replay,
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
                &cx_at(&measure, store.view(), old_focus),
                &mut fx,
            ),
            Handled::No
        );
        assert!(!returned.page.return_pending);
        store.run(PersonCmd::Close);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn a_successful_empty_source_answer_releases_the_return_card_to_fallback() {
        let _serial = plx_base::testlock::serial();
        let measure = FixtureMeasure;
        let (mut store, mut returned, old_focus, _origin, share) = pending_share_return(&measure);
        store.install_source_for_test(share, Vec::new(), Vec::new());
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut returned,
            &ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 2),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        assert!(
            !returned.page.return_pending,
            "a successful empty media answer is terminal for this saved source"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&returned, old_focus, &cx_at(&measure, store.view(), old_focus)),
            focus_of(&returned, &store, 0, 0)
        );
        store.run(PersonCmd::Close);
        plx_plex::plex::reset_servers_for_test();
    }

    /// Returning to a retained Person A first reclaims the single-slot store from Person B. That
    /// request is asynchronous: until A's own shelves land, the old engine key is deliberately
    /// absent from the published rows. Reconciliation must hold that known identity instead of
    /// rewriting the engine cursor to Header; the reordered landing can then resolve the same key
    /// back to the same movie without a screen-local focus copy.
    #[test]
    fn retained_back_holds_the_known_card_key_until_the_requested_person_lands() {
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let origin =
            plx_plex::plex::register_for_test("person-return-origin", "127.0.0.1", 1, "a", "cid");
        let share =
            plx_plex::plex::register_for_test("person-return-share", "127.0.0.1", 2, "b", "cid");
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid: origin, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        let mut first = PersonScreen::new(
            EntryId(0),
            origin,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        store.install_source_for_test(
            share,
            vec![item_on(share, "m0"), item_on(share, "m1")],
            Vec::new(),
        );
        first.page.sync_card_keys(&cx(&FixtureMeasure, store.view()));
        let old_focus = focus_of(&first, &store, 0, 1);
        assert_eq!(
            first
                .focused_item(Some(old_focus), &cx(&FixtureMeasure, store.view()))
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
        store.run(PersonCmd::Open { sid: origin, key: "other".into(), guid: "other-guid".into(),
            name: "Other Person".into(), thumb: String::new() });
        assert!(first.page.person(&cx(&FixtureMeasure, store.view())).is_none(), "Person B displaced Person A");

        let measure = FixtureMeasure;
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::RestoreMemory(PageMemory::Person(memory)),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 1),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        assert!(
            first.page.return_pending,
            "Person B's store notice is not an answer about A"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, store.view(), old_focus)),
            old_focus,
            "a wrong-person notice cannot consume return hydration"
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::Enter(Enter::Restored),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        store.run(PersonCmd::Open { sid: origin, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        assert!(first.page.person(&cx(&FixtureMeasure, store.view())).is_some(), "return reclaims Person A's store");
        assert!(
            first.page.person(&cx(&FixtureMeasure, store.view())).unwrap().shelf(0).is_empty(),
            "the addressed shelves have not landed yet"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, store.view(), old_focus)),
            old_focus,
            "a known return identity must survive the empty resolving interval"
        );

        store.install_source_for_test(
            origin,
            vec![item_on(origin, "other-source-card")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 2),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        assert!(
            first.page.person(&cx(&FixtureMeasure, store.view())).unwrap().landed,
            "another source has enough content to settle the page-level spinner"
        );
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, store.view(), old_focus)),
            old_focus,
            "page-level landed must not settle the saved card's still-resolving source"
        );

        store.install_source_for_test(
            share,
            vec![item_on(share, "m1"), item_on(share, "m0")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 3),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        assert!(
            !first.page.return_pending,
            "the matching card landing retires hydration"
        );
        let restored =
            Focusable::<PersonHost>::reconcile(&first, old_focus, &cx_at(&measure, store.view(), old_focus));
        assert_eq!(restored, old_focus);
        assert_eq!(
            first
                .focused_item(Some(restored), &cx(&FixtureMeasure, store.view()))
                .map(|movie| movie.rk.as_str()),
            Some("m1"),
            "the reordered landing resolves the preserved identity"
        );
        store.run(PersonCmd::Close);
        plx_plex::plex::reset_servers_for_test();
    }

    /// The same delayed interval exists after the dispatcher's body cap evicts the Person screen:
    /// PageMemory restores the interner before `Enter(Restored)`, while the new body has already
    /// opened an empty A store. The saved engine key must remain intact until A's shelves arrive.
    #[test]
    fn cold_remount_holds_the_memory_interner_key_until_reordered_shelves_land() {
        let _serial = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("person-cold-return", "127.0.0.1", 1, "a", "cid");
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        let mut original = PersonScreen::new(
            EntryId(0),
            sid,
            "161".to_string(),
            "5d77682aeb5d26001f1de4b0".to_string(),
            "Idina Menzel".to_string(),
            String::new(),
        );
        store.install_source_for_test(
            sid,
            vec![item_on(sid, "m0"), item_on(sid, "m1")],
            Vec::new(),
        );
        original.page.sync_card_keys(&cx(&FixtureMeasure, store.view()));
        let old_focus = focus_of(&original, &store, 0, 1);
        let PageMemory::Person(memory) =
            Screen::<PersonHost>::memory_at(&original, Some(old_focus))
        else {
            panic!("Person must persist its interner through PageMemory::Person");
        };

        store.run(PersonCmd::Close);
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
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut remounted,
            &ScreenEvent::RestoreMemory(PageMemory::Person(memory)),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        Machine::<PersonHost>::step(
            &mut remounted,
            &ScreenEvent::Enter(Enter::Restored),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        store.run(PersonCmd::Open { sid, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        assert!(remounted.page.person(&cx(&FixtureMeasure, store.view())).unwrap().shelf(0).is_empty());
        assert_eq!(
            Focusable::<PersonHost>::reconcile(&remounted, old_focus, &cx_at(&measure, store.view(), old_focus)),
            old_focus,
            "CAP eviction must not turn the saved card identity into Header while A reloads"
        );

        store.install_source_for_test(
            sid,
            vec![item_on(sid, "m1"), item_on(sid, "m0")],
            Vec::new(),
        );
        Machine::<PersonHost>::step(
            &mut remounted,
            &ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 2),
            &cx_at(&measure, store.view(), old_focus),
            &mut fx,
        );
        assert!(!remounted.page.return_pending);
        let restored =
            Focusable::<PersonHost>::reconcile(&remounted, old_focus, &cx_at(&measure, store.view(), old_focus));
        assert_eq!(restored, old_focus);
        assert_eq!(
            remounted
                .focused_item(Some(restored), &cx(&FixtureMeasure, store.view()))
                .map(|movie| movie.rk.as_str()),
            Some("m1")
        );
        store.run(PersonCmd::Close);
        plx_plex::plex::reset_servers_for_test();
    }

    #[test]
    fn restore_reclaims_only_a_displaced_store_and_preserves_shell_scroll() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut first) = seed(2, 0);
        jump_to(&mut first, 173.0);
        let _other = PersonScreen::new(
            EntryId(8),
            ServerId::UNSET,
            "other".to_string(),
            "other-guid".to_string(),
            "Other Person".to_string(),
            String::new(),
        );
        store.run(PersonCmd::Open { sid: ServerId::UNSET, key: "other".into(),
            guid: "other-guid".into(), name: "Other Person".into(), thumb: String::new() });
        assert!(first.page.person(&cx(&FixtureMeasure, store.view())).is_none());

        let measure = FixtureMeasure;
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(
            &mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
            &mut present,
        );
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::Enter(Enter::Restored),
            &cx(&measure, store.view()),
            &mut fx,
        );
        store.run(PersonCmd::Open { sid: ServerId::UNSET, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(),
            thumb: String::new() });
        assert!(first.page.person(&cx(&FixtureMeasure, store.view())).is_some());
        assert_eq!(first.stack.scroll(), 173.0);

        // A second restore while the matching store is already current is a true no-op.
        let before_gen = store.gen();
        Machine::<PersonHost>::step(
            &mut first,
            &ScreenEvent::Enter(Enter::Restored),
            &cx(&measure, store.view()),
            &mut fx,
        );
        assert_eq!(
            store.gen(),
            before_gen
        );
        assert_eq!(first.stack.scroll(), 173.0);
        store.run(PersonCmd::Close);
    }

    #[test]
    fn entry_back_and_hold_emit_their_content_effects_without_latches() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(1, 0);
        store.install_credits_for_test(&[("Actor", 7)]);
        let measure = FixtureMeasure;
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();

        let mut run = |screen: &mut PersonScreen,
                       event: ScreenEvent<PersonHost>,
                       focus: Option<plx_machine::machine::FocusKey<u32>>| {
            let context = Cx {
                views: store.view(),
                tick: Tick::default(),
                measure: &measure,
                press: PressRead::default(),
                focus: FocusRead { current: focus , ..Default::default() },
                owner: InputOwner::Entry(screen.page.entry),
            };
            let mut fx = Effects::new(
                &mut out,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)),
                &mut present,
            );
            Machine::<PersonHost>::step(screen, &event, &context, &mut fx)
        };

        assert_eq!(
            run(&mut s, ScreenEvent::Activate(ENTRY_ELEM), None),
            Handled::Yes
        );
        let card = focus_of(&s, &store, 0, 0);
        assert_eq!(
            run(
                &mut s,
                ScreenEvent::PressHold(plx_machine::machine::PressId(2)),
                Some(card)
            ),
            Handled::Yes
        );
        let back = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: plx_machine::machine::Source::Replay,
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
            plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Present(
                ContentArg::Filmography { sid, key }
            ))) if *sid == ServerId::UNSET && key == "161"
        )));
        assert!(out.iter().any(|st| matches!(
            &st.fx,
            plx_machine::machine::Fx::App(AppFx::Content(ContentReq::ItemMenu))
        )));
        assert!(out.iter().any(|st| matches!(
            &st.fx,
            plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Back))
        )));
        store.run(PersonCmd::Close);
    }

    #[test]
    fn header_geometric_anchor_is_not_its_pointer_hit_rectangle() {
        let _serial = plx_base::testlock::serial();
        let (mut store, s) = seed(1, 0);
        let anchor = s.page.header_anchor();
        let hit = s.page.header_rect();
        assert_eq!(anchor.x, MARGIN_X);
        assert_eq!(anchor.w, 0.0, "the navigation projection stays left-pinned");
        assert!(
            hit.w > 0.0 && hit.h > 0.0,
            "the drawn biography band is clickable"
        );

        let measure = FixtureMeasure;
        let placed = Focusable::<PersonHost>::place(
            &s, &HEADER_ELEM, &cx(&measure, store.view()), At::Drawn)
            .expect("the header is a real engine element");
        assert_eq!(
            (placed.rect.x, placed.rect.y, placed.rect.w, placed.rect.h),
            (hit.x, hit.y, hit.w, hit.h),
            "place and the recorded stop share the hit geometry"
        );
        store.run(PersonCmd::Close);
    }

    /// A landed page entered the way the container enters a fresh mount (`FirstInGroup(GroupId(0))`)
    /// opens on the Filmography pill: group 0 is the pill's, whatever else the page holds.
    #[test]
    fn a_fresh_mount_of_a_landed_page_seats_the_filmography_pill() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(2, 1);
        store.install_credits_for_test(&[("Actor", 3)]);
        s.page.refresh_store_cache(&cx(&FixtureMeasure, store.view()));
        lay(&mut s, &store, &FixtureMeasure);
        let context = cx(&FixtureMeasure, store.view());
        let mut engine = plx_ui::focus::FocusEngine::new();
        let plx_ui::focus::Outcome::Moved { to, .. } = engine.enter(
            context.owner,
            &s,
            plx_machine::machine::FocusTarget::FirstInGroup(GroupId(0)),
            None,
            &context,
        ) else {
            panic!("a landed page must seat");
        };
        assert_eq!(to, plx_machine::machine::FocusKey { entry: EntryId(0), elem: ENTRY_ELEM });
        store.run(PersonCmd::Close);
    }

    /// A shelf's group id belongs to its kind, not to which other shelves the page holds: the
    /// engine remembers a cursor per `(entry, group)`.
    #[test]
    fn a_shelf_keeps_its_group_id_whichever_other_shelves_exist() {
        let _serial = plx_base::testlock::serial();
        let group_of_kind = |movies: usize, shows: usize, kind: usize| {
            let (store, s) = seed(movies, shows);
            let context = cx(&FixtureMeasure, store.view());
            let elem = focus_of(&s, &store, kind, 0).elem;
            let g = Focusable::<PersonHost>::group_of(&s, &elem, &context);
            let mut store = store;
            store.run(PersonCmd::Close);
            g
        };
        assert!(group_of_kind(1, 1, 1).is_some());
        assert_eq!(group_of_kind(1, 1, 1), group_of_kind(0, 1, 1), "Shows with and without Movies");
        assert_eq!(group_of_kind(1, 1, 0), group_of_kind(1, 0, 0), "Movies with and without Shows");
        assert_ne!(group_of_kind(1, 1, 0), group_of_kind(1, 1, 1));
    }

    /// With no person to show, reconcile answers the header (never the bare `want`).
    #[test]
    fn reconcile_with_no_person_answers_the_header() {
        let _serial = plx_base::testlock::serial();
        let store = plx_data::stores::person::PersonStore::default();
        let mut s = PersonScreen::new(EntryId(0), ServerId::UNSET, "161".to_string(), String::new(), String::new(), String::new());
        lay(&mut s, &store, &FixtureMeasure);
        let want = plx_machine::machine::FocusKey { entry: EntryId(0), elem: 0x7777 };
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx(&FixtureMeasure, store.view()));
        assert_eq!(got, plx_machine::machine::FocusKey { entry: EntryId(0), elem: HEADER_ELEM });
    }

    /// Owner decision 3 (one recovery rule on every card screen): a focused card that disappears
    /// hands focus to the card now at the same position, clamped within its own section.
    #[test]
    fn a_removed_focused_card_hands_focus_to_the_same_position_clamped() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(3, 1);
        let m = FixtureMeasure;
        let want = focus_of(&s, &store, 0, 1);
        settle_shelves(&mut s, &store, want, 3);
        store.install_for_test(vec![item("m0"), item("m2")], vec![item("s0")]);
        s.page.refresh_store_cache(&cx(&m, store.view()));
        settle_shelves(&mut s, &store, want, 1);
        let got = Focusable::<PersonHost>::reconcile(&s, want, &cx_at(&m, store.view(), want));
        assert_eq!(got, focus_of(&s, &store, 0, 1), "the card that slid into position 1 takes focus");
        store.install_for_test(vec![item("m0")], vec![item("s0")]);
        s.page.refresh_store_cache(&cx(&m, store.view()));
        settle_shelves(&mut s, &store, got, 1);
        let got = Focusable::<PersonHost>::reconcile(&s, got, &cx_at(&m, store.view(), got));
        assert_eq!(got, focus_of(&s, &store, 0, 0), "past the end, the last card left in the SAME section");
        store.run(PersonCmd::Close);
    }

    #[test]
    fn focus_walks_only_the_shelves_that_exist() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(3, 0);
        let measure = FixtureMeasure;
        let context = cx(&measure, store.view());
        let mut groups = Vec::new();
        Focusable::<PersonHost>::groups(&s, &context, &mut groups);
        let group = |s: &PersonScreen, k: Sec| s.stack.group(k).unwrap();
        assert!(groups.iter().any(|g| g.id == group(&s, Sec::Shelf(0)) && g.len == 3));
        assert!(s.stack.group(Sec::Shelf(1)).is_none(), "no shelf of that kind, no section");
        let last = focus_of(&s, &store, 0, 2);
        assert!(matches!(
            Focusable::<PersonHost>::neighbour(&s, last, Dir::Right, &context),
            Step::Edge
        ));
        drop(context);
        store.install_credits_for_test(&[("Actor", 3)]);
        s.page.refresh_store_cache(&cx(&measure, store.view()));
        lay(&mut s, &store, &measure);
        let mut links = Vec::new();
        Screen::<PersonHost>::links(&s, &mut links);
        assert!(links.iter().any(|link| {
            link.from == group(&s, Sec::Entry) && link.dir == Dir::Down && link.to == group(&s, Sec::Shelf(0))
        }));
        store.run(PersonCmd::Close);
    }

    /// A landing may move the focused item from one shelf to the other. The engine element is the
    /// item's identity, so focus stays on it; the shelf it left lets go of the lift and the shelf it
    /// joined adopts it at the full pop (no frame shows two lifted tiles, none shows it collapsed).
    #[test]
    fn an_item_that_changes_shelf_in_a_landing_keeps_focus_and_is_lifted_on_its_new_shelf() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = seed(2, 1);
        let m = FixtureMeasure;
        let moved = focus_of(&s, &store, 0, 0);
        settle_shelves(&mut s, &store, moved, 120);
        let lift = |s: &PersonScreen, store: &plx_data::stores::person::PersonStore, kind: usize, elem: u32| {
            let p = store.view().current().unwrap();
            let _ = (p, kind);
            s.stack.view(&s.page).scale_of(&cx_at(&m, store.view(), moved), &elem)
        };
        assert_eq!(lift(&s, &store, 0, moved.elem), Some(SHELF_STYLE.focus_scale));

        // "m0" leaves the movies and joins the shows.
        store.install_for_test(vec![item("m1")], vec![item("s0"), item("m0")]);
        s.page.refresh_store_cache(&cx(&m, store.view()));
        assert_eq!(s.page.locate(store.view().current().unwrap(), moved.elem), Some(Located::Shelf(1, 1)));
        let now = Focusable::<PersonHost>::reconcile(&s, moved, &cx_at(&m, store.view(), moved));
        assert_eq!(now, moved, "the cursor stays on the item across shelves");
        settle_shelves(&mut s, &store, moved, 1);
        assert_eq!(lift(&s, &store, 1, moved.elem), Some(SHELF_STYLE.focus_scale), "adopted whole on its new shelf");
        let other = focus_of(&s, &store, 0, 0);
        assert_eq!(lift(&s, &store, 0, other.elem), Some(1.0), "the movies shelf shows nothing lifted");
        settle_shelves(&mut s, &store, moved, 120);
        assert!(Focusable::<PersonHost>::place(&s, &moved.elem, &cx_at(&m, store.view(), moved), At::Drawn).is_some());
        store.run(PersonCmd::Close);
    }

    /// A page whose header has its measured band (or `exp_h`, to pin the other portrait), with two
    /// shelves, the stack laid out over it.
    fn shelved_page(exp_h: f32) -> (plx_data::stores::person::PersonStore, PersonScreen) {
        let (store, mut s) = seed(3, 3);
        let m = FixtureMeasure;
        s.page.remeasure_header(store.view().current().unwrap(), &m);
        s.page.header.exp_h = exp_h;
        s.page.layout_gen += 1;
        lay(&mut s, &store, &m);
        (store, s)
    }

    #[test]
    fn the_first_shelf_fits_at_rest() {
        let _serial = plx_base::testlock::serial();
        for band in [PORTRAIT_BARE, PORTRAIT_EXP] {
            let (mut store, mut s) = shelved_page(band);
            let key = focus_of(&s, &store, 0, 0);
            settle_shelves(&mut s, &store, key, 240);
            assert_eq!(s.stack.scroll(), 0.0, "a {band}px header: the first shelf fits without scrolling");
            store.run(PersonCmd::Close);
        }
    }

    #[test]
    fn reaching_the_second_shelf_scrolls_it_fully_into_view_and_no_further() {
        let _serial = plx_base::testlock::serial();
        let (mut store, mut s) = shelved_page(PORTRAIT_EXP);
        let key = focus_of(&s, &store, 1, 0);
        settle_shelves(&mut s, &store, key, 480);
        assert!(s.stack.scroll() > 0.0);
        let placed = Focusable::<PersonHost>::place(&s, &key.elem, &cx_at(&FixtureMeasure, store.view(), key), At::SpringTarget)
            .unwrap();
        let tile_top = placed.rest_rect.cy() - CARD_H / 2.0;
        let block_top = tile_top - SHELF_LABEL_H;
        let block_bottom = tile_top + CARD_H + ui_cards::under_band(1.0);
        assert!(block_bottom <= SCR_H, "the block's bottom edge is on screen: {block_bottom}");
        assert!(block_top >= HEADER_TOP - 0.5, "and its top keeps the page margin: {block_top}");
        // and no further: the last shelf rests exactly one page margin above the bottom edge
        assert!((block_bottom - (SCR_H - MARGIN_Y)).abs() < 0.5, "{block_bottom}");
        store.run(PersonCmd::Close);
    }

    #[test]
    fn a_shelf_here_pitches_like_a_shelf_on_home() {
        assert_eq!(
            SHELF_GAP + SHELF_LABEL_H + CARD_H + ui_cards::under_band(1.0),
            plx_ui::consts::ROW_PITCH
        );
        assert_eq!(SHELF_LABEL_H, TITLE_DY + CARD_DY);
    }

}
