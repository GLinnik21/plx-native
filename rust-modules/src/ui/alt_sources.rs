//! **Also available** — the same film held by more than one source, as a list you can walk into.
//!
//! Deliverable E of the Shared Sources design (`docs/shared-servers.md` §6): when a second pinned
//! source also holds the item on screen, the detail page's actions row grows an *Also available*
//! pill with a trailing chevron, and it opens this panel — the app's usual [`Popover`] +
//! [`TableView`], the same object the Library's Sort and Filter chips open one page over.
//!
//! | row | detail | badge |
//! |---|---|---|
//! | `Movies`    | `This account · 1 hr 57 min` | `1080p` |
//! | `LDN Films` | `bamx23 · 1 hr 57 min`       | `4K`    |
//!
//! # The four calls this module implements, and why each is not the obvious thing
//!
//! **A CONTROL in the actions row, not a section of the page.** An item on several servers is a
//! rare fact; a permanent band would spend a whole row on nothing on every other page in the
//! library. So the list is behind a button, and the button itself is drawn ONLY when a second
//! pinned source holds this item ([`is_available`]) — the same "with one source, none of this is
//! drawn" rule the shelf heading's annotation implements as absence rather than as a branch that
//! draws nothing visible.
//!
//! **The tick marks the copy you are on now**, and the badge is the RESOLUTION class — the
//! vocabulary the hero's badges row already speaks (`ui::fmt::resolution`, one spelling of "4K" in
//! the product). A row is `[library] [owner · runtime] [class]`: the label answers *which library*,
//! the sub-line *whose*, the badge *what you would get*.
//!
//! **OK NAVIGATES.** It opens that server's own page for the film; it does not swap the copy under
//! you. That is also what settles the per-server progress problem: watch state does not travel
//! between servers, so a design that switched the copy in place would have had to explain a resume
//! position that jumped. Navigating makes it self-evident — you are on a different page, and it
//! says where you are in *that* copy. Like every other menu here this module only REPORTS an
//! [`Action`]; the detail page performs it (through its own `take_open_request` latch, the same one
//! its Related shelf and episode filmstrip raise).
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
//! offer a different film. [`install`] is the seam the multi-server data layer feeds once it can
//! ask each pinned source for its copy of a guid; until then the store is EMPTY at every mount, so
//! [`is_available`] is false, the button is not drawn, and a one-server install pays nothing —
//! not a query, not a control, not a row of layout.
//!
//! For headless verification there is a draw-only stand-in built on the SAME
//! `/tmp/plxnative-shared=<handle>` trigger the hero's own "Shared by …" run is judged through
//! (see [`dev_stand_in`]) — one trigger for one deliverable — which is how this panel is captured
//! on the TV before that layer exists.
#![allow(non_upper_case_globals)]
use crate::plex::ServerId;
use crate::ui::consts::*;
use crate::ui::popover::Popover;
use crate::ui::table::{Badge, Row, Section, TableView};
use crate::ui::{theme, Rect};
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// One copy of the item on ONE source — everything a row needs, and nothing about layout.
///
/// Built by whoever resolved the item's `guid` across the pinned sources; this module only orders,
/// marks and draws them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AltCopy {
    /// The registry slot the copy lives on ([`crate::plex::servers`]). It is the row's IDENTITY —
    /// the gate counts distinct values of it, and OK resolves the destination client through it —
    /// so a copy whose source is not registered carries [`ServerId::UNSET`] and can be listed but
    /// never navigated to.
    pub(crate) sid: ServerId,
    /// The LIBRARY this copy is in, on that server ("Movies", "LDN Films") — the row's label.
    /// Libraries are what a person browses; the machine name (`bx23-ldn`) belongs to the Sources
    /// list and to a failure read-out, and appears nowhere else in the product.
    pub(crate) library: String,
    /// Who owns that server: `None` for the signed-in account's own, `Some(handle)` for a share —
    /// the same `sourceTitle` handle the shelf headings and the Sources list use. `None` is the
    /// ABSENCE of an owner, not an empty one: it is what the row spells "This account", and it is
    /// the ordering's own-before-a-friend's tiebreak.
    pub(crate) owner: Option<String>,
    /// That server's ratingKey for the item — per-server, and the whole reason the identity above
    /// it is a guid. Where OK navigates.
    pub(crate) rk: String,
    /// Runtime in ms, for the row's sub-line. 0 = unknown, and then the sub-line is the owner
    /// alone rather than "0 min".
    pub(crate) dur_ms: i64,
    /// `Media.videoResolution` ("4k" / "1080" / "sd"), plus the stored frame size as its fallback —
    /// exactly the trio `ui::fmt::resolution` takes, so the badge here and the hero's media chip
    /// are one function and cannot spell the same file two ways.
    pub(crate) res: String,
    pub(crate) width: i64,
    pub(crate) height: i64,
}

/// What OK on the highlighted row does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    None,
    /// Open `rk` on server `sid` — that copy's own page. Never a swap of the copy in place.
    Open { sid: ServerId, rk: String },
}

/// The panel's width — the design's `altPanelW`. Wide because the rows carry three things (a
/// library, a sub-line naming a person and a runtime, and a class badge) and the sub-line is the
/// one that must not elide: "bamx23 · 1 hr 57 min" is the answer to *whose copy is this*.
const PANEL_W: f32 = 700.0;
/// The pinned ~20px corner radius, the item menu's.
const PANEL_RAD: f32 = 20.0;
/// Air between the button that opened the panel and the panel itself — one `space` rung.
const BTN_GAP: f32 = theme::space::MD;
/// Keep-out from the screen edges, so a panel opened low on the page is still whole.
const EDGE: f32 = theme::space::XL;
/// Scrim peak alpha — the LIBRARY toolbar chip menu's (`ui::library`), deliberately, because this
/// is the same object one page over: a chip-shaped control on a live page opening a list over it.
/// The page recedes; it is not blanked, and the hero behind stays readable.
const SCRIM_A: f32 = 0.45;
/// How far the panel rises into place, matching those same chip menus.
const RISE: f32 = 20.0;

/// The item the store describes, as its ratingKey ON THE CURRENT SERVER — i.e. the page's own
/// mounted rk. It is BOTH halves of a mailbox: [`install`] refuses a landing for any other item
/// (a guid resolve that outlived the page it was asked for must not land on the next one), and it
/// is the `rk` half of "which copy am I on now", the other half being the current server.
static mut FOR_RK: String = String::new();
/// Every copy of that item, in the order the producer found them (this module orders them itself).
static mut COPIES: Vec<AltCopy> = Vec::new();

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The destination per global row index — parallel to the rows the panel was BUILT from, so a
/// selection can never be resolved against a differently-ordered list (`more_menu`'s rule: the row
/// list IS the mapping).
static mut DESTS: Vec<(ServerId, String)> = Vec::new();
/// The button's drawn rect at open time, in screen coords — what the panel anchors to.
static mut ANCHOR: Rect = Rect::new(0.0, 0.0, 0.0, 0.0);

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
fn dests() -> &'static mut Vec<(ServerId, String)> {
    unsafe { &mut *addr_of_mut!(DESTS) }
}
fn copies() -> &'static [AltCopy] {
    unsafe { &*addr_of!(COPIES) }
}

// ---- the store -------------------------------------------------------------------------------

/// Point the store at a freshly mounted item and drop whatever the last one had.
///
/// Called by the detail page from every mount, BEFORE anything can be installed for the new item.
/// It also closes the panel: a popover that survived the page under it would be listing another
/// film's copies over this one's hero.
pub(crate) fn reset(item_rk: &str) {
    close();
    // ASSIGNMENT, not `ptr::write`: these are owning heap values, and `write` overwrites without
    // running the old one's destructor — which here would leak a `String` and a `Vec<AltCopy>` on
    // every detail mount. (The `.write()` idiom this file's neighbours use is sound where they use
    // it, on `Copy` types and `&'static [T]`.)
    unsafe {
        *addr_of_mut!(FOR_RK) = item_rk.to_string();
        *addr_of_mut!(COPIES) = Vec::new();
        *addr_of_mut!(STAND_RK) = String::new();
    }
}

/// Install the copies resolved for `item_rk` — the seam the multi-server data layer feeds.
///
/// **A landing only applies while it is still the item being awaited**, the invariant
/// `metadata::pump_detail` exists for: a guid resolve is a per-server round trip, so one asked for
/// the page you just left can land on the page you just opened, and a wrong list here would offer
/// to navigate to a different film. Repaints, so it invalidates the frame gate — a landing that
/// grows the actions row must be visible without waiting for a keypress.
pub(crate) fn install(item_rk: &str, list: Vec<AltCopy>) {
    if item_rk != unsafe { (*addr_of!(FOR_RK)).as_str() } {
        return;
    }
    unsafe { *addr_of_mut!(COPIES) = list }; // assignment — see [`reset`]
    crate::ui::idle::invalidate();
}

/// How many distinct SOURCES hold this item. The gate is stated in sources rather than in copies
/// because that is the design's own wording — two copies in two libraries of ONE server is not
/// "also available *elsewhere*", and the row that would name the other one has nowhere to send you
/// that you are not already.
fn source_count(list: &[AltCopy]) -> usize {
    let mut n = 0;
    for (i, c) in list.iter().enumerate() {
        if !list[..i].iter().any(|p| p.sid == c.sid) {
            n += 1;
        }
    }
    n
}

/// **The gate**: is a second pinned source holding this item? The actions row asks before it draws
/// the control, so with one source there is no button, no layout for it and no draw call.
pub(crate) fn is_available() -> bool {
    source_count(copies()) >= 2
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
    /// `"This account · 1 hr 57 min"` / `"bamx23 · 1 hr 57 min"`
    pub(crate) detail: String,
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
    let here = list.iter().position(|c| c.sid == here_sid && c.rk == here_rk);
    let mut idx: Vec<usize> = (0..list.len()).collect();
    idx.sort_by(|&a, &b| {
        let (ca, cb) = (&list[a], &list[b]);
        // `false` sorts before `true`, so each key is written as "the one that loses"
        (here != Some(a), std::cmp::Reverse(scan_lines(ca)), ca.owner.is_some(), &ca.library)
            .cmp(&(here != Some(b), std::cmp::Reverse(scan_lines(cb)), cb.owner.is_some(), &cb.library))
    });
    idx.into_iter()
        .map(|i| {
            let c = &list[i];
            let who = c.owner.as_deref().unwrap_or(OWN_ACCOUNT);
            AltRow {
                label: c.library.clone(),
                // a copy the server sent no duration for says whose it is and stops, rather than
                // claiming "0 min" — the dangling-separator rule `dotted_run` follows one file over
                detail: if c.dur_ms > 0 {
                    format!("{who} \u{b7} {}", crate::ui::fmt::dur_long(c.dur_ms))
                } else {
                    who.to_string()
                },
                badge: crate::ui::fmt::resolution(&c.res, c.width, c.height),
                checked: here == Some(i),
                sid: c.sid,
                rk: c.rk.clone(),
            }
        })
        .collect()
}

/// The rows as the panel would build them right now: the store, marked against the CURRENT server
/// and the item the store was filled for.
fn live_rows() -> Vec<AltRow> {
    rows(copies(), crate::plex::current_server(), unsafe { (*addr_of!(FOR_RK)).as_str() })
}

// ---- the panel -------------------------------------------------------------------------------

pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// Open the list, anchored to the control that opened it (the *Also available* pill's drawn rect).
/// Selection starts on the copy you are on — the row the tick is against — because that is where
/// the eye already is, and one DOWN from it is the alternative.
pub(crate) fn open(anchor: Rect) {
    let rs = live_rows();
    let mut sec = Section::new("");
    let mut sel = 0;
    dests().clear();
    for (i, r) in rs.iter().enumerate() {
        let mut row = Row::new(r.label.clone()).detail(r.detail.clone()).checked(r.checked);
        if let Some(b) = &r.badge {
            row = row.badge(Badge::Text(b.clone()));
        }
        sec = sec.row(row);
        dests().push((r.sid, r.rk.clone()));
        if r.checked {
            sel = i as i32;
        }
    }
    unsafe { addr_of_mut!(ANCHOR).write(anchor) };
    table().compact = false; // rows carry a sub-line — the track menu's size class, not the account popover's
    table().set_sections(vec![sec], sel, false);
    debug_assert_eq!(dests().len() as i32, table().n_rows());
    pop().open();
}

pub(crate) fn close() {
    pop().close();
}

pub(crate) fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    }
}

/// Commit the highlighted row and close. The row you are ALREADY on reports nothing — there is
/// nowhere to navigate to, and the tick has already answered the question the press was asking.
pub(crate) fn on_ok() -> Action {
    let sel = table().sel;
    close();
    action_at(dests(), sel, crate::plex::current_server(), unsafe { (*addr_of!(FOR_RK)).as_str() })
}

/// The index→destination mapping, pure so the "the row you are on is not a destination" rule is
/// host-testable. A selection outside the list is [`Action::None`] rather than whatever happens to
/// sit at that index in some other row set (`sel` survives a rebuild).
fn action_at(list: &[(ServerId, String)], sel: i32, here_sid: ServerId, here_rk: &str) -> Action {
    let Some((sid, rk)) = usize::try_from(sel).ok().and_then(|i| list.get(i)) else {
        return Action::None;
    };
    if (*sid == here_sid && rk == here_rk) || rk.is_empty() {
        return Action::None;
    }
    Action::Open { sid: *sid, rk: rk.clone() }
}

/// Pointer hover: focus follows the cursor over the rows.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if !is_open() {
        return;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
    }
}

/// Pointer click: commit the row under the cursor (same as OK); a click anywhere else dismisses,
/// which is what a click outside a popover means everywhere in this app.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if !is_open() {
        return Action::None;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
        return on_ok();
    }
    close();
    Action::None
}

/// Under the button when there is room, above it when there is not — never over it, so the control
/// that opened the panel stays readable beside its own list. Horizontally it hangs off the
/// button's left edge (the list and the label it came from share a margin), pulled inside the
/// screen's keep-out. Pure (anchor + measured height in, rect out), so the placement rules are
/// host-testable without the module's statics.
fn panel_at(a: Rect, content_h: f32) -> Rect {
    let h = content_h.clamp(120.0, SCR_H - 2.0 * EDGE);
    let below = a.y + a.h + BTN_GAP;
    let y = if below + h <= SCR_H - EDGE { below } else { (a.y - BTN_GAP - h).max(EDGE) };
    let x = a.x.clamp(EDGE, (SCR_W - EDGE - PANEL_W).max(EDGE));
    Rect::new(x, y, PANEL_W, h)
}

fn panel_rect() -> Rect {
    panel_at(unsafe { addr_of!(ANCHOR).read() }, table().measured_height())
}

pub(crate) fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let ph = panel_rect().h;
    table().update(dt, ph - crate::ui::table::PAD_V);
}

pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let p = pop().painter(SCRIM_A, RISE);
    let r = panel_rect();
    p.rect(r, PANEL_RAD, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
    table().draw(p, r);
}

// ---- the headless stand-in ---------------------------------------------------------------------

/// A DRAW-ONLY copy list for `/tmp/plxnative-altsources`, so this panel and the control that opens
/// it can be captured on the television before the multi-server data layer exists.
///
/// The trigger's content is a JSON array of copies, e.g.
///
/// ```json
/// [{"library":"Movies","res":"1080"},
///  {"library":"LDN Films","owner":"bamx23","res":"4k","slot":1,"rk":"318"}]
/// ```
///
/// with three conveniences that keep one file usable on any item: an entry with **no `rk`** is the
/// copy you are on (it takes the page's own ratingKey and the current server, so the tick lands on
/// a real identity rather than a fabricated one), a missing `dur_ms` takes the loaded item's
/// runtime, and a missing `slot` gets an id that names NO registered server — so OK on such a row
/// reports nothing and logs, instead of navigating to a ratingKey on the wrong machine. Reached
/// through `dev::read`, so it is absent from a `RELEASE=1` build at compile time along with the
/// whole `/tmp` surface (the trigger literal is `/tmp/plxnative-altsources`, spelled here for the
/// catalog grep in the root `CLAUDE.md`).
/// Which item the stand-in has been built for, so it is built ONCE per page rather than every
/// frame. Cleared by [`reset`] with the rest of the store.
static mut STAND_RK: String = String::new();

/// Build the headless stand-in once the item has LANDED — called every frame by `detail::update`,
/// and doing nothing at all in the ordinary case (two string compares, no allocation).
///
/// It cannot be built in [`reset`], where the rest of the store is: a mount deliberately clears
/// `metadata::current()` for the whole 2-5 round-trip fetch window (`detail::open_rk`), so at that
/// moment there is no runtime, no resolution class and no title to build a copy of the item FROM.
pub(crate) fn pump() {
    let for_rk = unsafe { (*addr_of!(FOR_RK)).as_str() };
    if for_rk.is_empty() || for_rk == unsafe { (*addr_of!(STAND_RK)).as_str() } {
        return; // no page, or this one has already had its chance — two string compares
    }
    // Wait for the item, and do NOT mark the attempt until it is here: a mount keeps
    // `metadata::current()` empty for the whole fetch, and spending the one chance during that
    // window would mean no stand-in at all on the page it was armed for.
    if crate::metadata::current().filter(|d| d.rk == for_rk).is_none() {
        return;
    }
    // Marked whatever the outcome, so the ordinary build — where the trigger is not armed at all —
    // opens the `/tmp` file ONCE per page rather than on every frame of it.
    unsafe { *addr_of_mut!(STAND_RK) = for_rk.to_string() };
    if let Some(list) = dev_stand_in(for_rk) {
        unsafe { *addr_of_mut!(COPIES) = list };
        crate::ui::idle::invalidate();
    }
}

/// A DRAW-ONLY copy list for `/tmp/plxnative-shared=<handle>`, so this panel and the control that
/// opens it can be judged on a television before the multi-server data layer exists.
///
/// **It reuses the trigger the hero's own "Shared by …" run is judged through** rather than adding
/// a second one for the same deliverable: that trigger already means "pretend this item came from
/// `<handle>`", and a second copy of the film on `<handle>`'s source is the other half of the same
/// pretence.
///
/// What it stands in for, exactly:
///
/// * **your copy** is the real one — the page's own ratingKey, on the current server, with the
///   loaded item's own runtime and resolution class, in the library the browse store says you are
///   in. Nothing about it is invented, which is what makes the TICK meaningful.
/// * **their copy** is that same film on a SECOND REGISTRY SLOT ([`stand_in_slot`]). It is a real
///   slot with a real client, so OK on it performs the real source switch and opens a real page —
///   the one behaviour of this panel that cannot be judged from a still.
/// * its resolution is **one class better than yours**, and that is the stand-in's ONE invention.
///   It exists because the ordering rule is otherwise unobservable: the design's own example is a
///   1080p copy you are standing on sorting above a friend's 4K one, which is the case that
///   separates "the copy that plays" from "the best copy". With two equal badges the panel cannot
///   show that it got the rule right.
///
/// `None` (and no stand-in) when the trigger is not armed or the item has not landed yet.
#[cfg(not(test))]
fn dev_stand_in(item_rk: &str) -> Option<Vec<AltCopy>> {
    let handle = crate::dev::read("shared").filter(|h| !h.is_empty())?;
    let d = crate::metadata::current().filter(|d| d.rk == item_rk)?;
    let library = crate::browse::section_title(crate::browse::cur()).to_string();
    let here = crate::plex::current_server();
    let theirs = stand_in_slot()?;
    let v = stand_in(&handle, &library, item_rk, &d.video_resolution, d.dur_ms, here, theirs);
    crate::log(&format!("altsources: stand-in for rk={item_rk} on slot {} (dev)", theirs.raw()));
    Some(v)
}
#[cfg(test)]
fn dev_stand_in(_item_rk: &str) -> Option<Vec<AltCopy>> {
    None // the host suite must not depend on what this dev Mac happens to have under /tmp
}

/// The registry slot the stand-in's borrowed copy lives on — a REAL one, because the point of the
/// stand-in is to exercise the switch.
///
/// A genuinely different server if one is registered. Otherwise the current server registered a
/// SECOND time under a synthetic machine id: from everything this feature does — counting sources,
/// resolving a client, switching, refetching — that is a second source, and because it is the same
/// machine the ratingKey it carries is honestly the same film. What it cannot stand in for is a
/// server going offline, or a library and a resolution that differ for real.
///
/// The token comes from the harness's own `/tmp/plxnative-token`, which is the only place a session
/// token is available to a dev path; with no token there is nothing to build a working client from,
/// so there is no stand-in at all rather than one that 401s.
#[cfg(not(test))]
fn stand_in_slot() -> Option<ServerId> {
    let here = crate::plex::current_server();
    let other = (0..crate::plex::server_count())
        .map(|i| ServerId::from_raw(i as u16))
        .find(|&id| id != here && crate::plex::client_for(id).is_some());
    if let Some(id) = other {
        return Some(id); // a real second server is already registered — use it
    }
    let c = crate::plex::client_opt()?;
    let token = crate::dev::read("token").filter(|t| !t.is_empty())?;
    Some(crate::plex::register(&format!("standin-{}", c.machine_id()), c.host(), c.port(), &token))
}

/// [`dev_stand_in`]'s pure half — the two copies it describes, so the shape of what a device
/// capture is looking at is itself host-graded.
fn stand_in(
    handle: &str,
    library: &str,
    rk: &str,
    res: &str,
    dur_ms: i64,
    here: ServerId,
    theirs: ServerId,
) -> Vec<AltCopy> {
    let mine = AltCopy {
        sid: here,
        library: library.to_string(),
        owner: None,
        rk: rk.to_string(),
        dur_ms,
        res: res.to_string(),
        width: 0,
        height: 0,
    };
    let borrowed = AltCopy {
        sid: theirs,
        owner: Some(handle.to_string()),
        res: one_class_better(res),
        ..mine.clone()
    };
    vec![mine, borrowed]
}

/// The resolution class one rung above `res`, in [`scan_lines`]' own vocabulary — the stand-in's
/// single invention (see [`dev_stand_in`]). Anything already at the top, or unrecognised, is
/// returned unchanged rather than promoted into a class that does not exist.
fn one_class_better(res: &str) -> String {
    match res.trim().to_ascii_lowercase().as_str() {
        "sd" | "480" | "576" => "720".into(),
        "720" => "1080".into(),
        "1080" | "1440" => "4k".into(),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------------------------------------
// The pure half — the ordering, the gate and the row model. `open`/`draw` own `static mut`
// TABLE/POP/COPIES and are deliberately not `Sync`; everything a test can reach takes its inputs
// as arguments.
#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u16) -> ServerId {
        ServerId::from_raw(n)
    }
    /// A copy on server `s`, in `library`, owned by `owner` (`""` = this account), at class `res`.
    fn copy(s: u16, library: &str, owner: &str, rk: &str, res: &str) -> AltCopy {
        AltCopy {
            sid: sid(s),
            library: library.into(),
            owner: (!owner.is_empty()).then(|| owner.to_string()),
            rk: rk.into(),
            dur_ms: 7_020_000, // 1 hr 57 min, the design's own runtime
            res: res.into(),
            width: 0,
            height: 0,
        }
    }
    fn labels(rs: &[AltRow]) -> Vec<&str> {
        rs.iter().map(|r| r.label.as_str()).collect()
    }

    /// **The gate.** The control is drawn only when a SECOND pinned source holds the item — so one
    /// server, or two copies inside one server, draw nothing at all. The middle case is the one
    /// worth pinning: two rows would look like a working feature while the second row led back to
    /// the machine you are already on.
    #[test]
    fn the_button_appears_only_when_a_second_source_holds_the_item() {
        assert_eq!(source_count(&[]), 0, "nothing resolved yet");
        assert_eq!(source_count(&[copy(0, "Movies", "", "4", "1080")]), 1, "one source is the 90% install");
        assert_eq!(
            source_count(&[copy(0, "Movies", "", "4", "1080"), copy(0, "4K Movies", "", "9", "4k")]),
            1,
            "two copies on ONE server are still one source"
        );
        assert_eq!(
            source_count(&[copy(0, "Movies", "", "4", "1080"), copy(1, "LDN Films", "bamx23", "318", "4k")]),
            2
        );
        // and a third source counts once however many copies it contributes
        assert_eq!(
            source_count(&[
                copy(0, "Movies", "", "4", "1080"),
                copy(1, "LDN Films", "bamx23", "318", "4k"),
                copy(1, "Films", "bamx23", "319", "1080"),
            ]),
            2
        );
    }

    /// **The ordering rule, exactly as the design states it**: the copy that plays first, then what
    /// a viewer would prefer. The design's own example is the case that separates the two clauses —
    /// a 1080p copy you are standing on sorts above a friend's 4K one — and it is the row order
    /// the canvas draws.
    #[test]
    fn the_copy_that_plays_comes_first_and_the_rest_rank_by_preference() {
        let list = [copy(1, "LDN Films", "bamx23", "318", "4k"), copy(0, "Movies", "", "4", "1080")];
        let rs = rows(&list, sid(0), "4");
        assert_eq!(labels(&rs), ["Movies", "LDN Films"], "the copy that plays leads, whatever its class");
        assert_eq!(rs[0].detail, "This account \u{b7} 1 hr 57 min");
        assert_eq!(rs[1].detail, "bamx23 \u{b7} 1 hr 57 min");
        assert_eq!(rs[0].badge.as_deref(), Some("1080p"));
        assert_eq!(rs[1].badge.as_deref(), Some("4K"), "the badge is the hero's own resolution vocabulary");

        // …and standing on the OTHER copy inverts only the first key, not the rest
        assert_eq!(labels(&rows(&list, sid(1), "318")), ["LDN Films", "Movies"]);
    }

    /// Below the copy that plays, quality decides — and only at EQUAL quality does "yours cannot go
    /// offline mid-film" break the tie. Both halves are asserted against the same fixture, because
    /// getting the keys in the other order would pass a test for either one alone.
    #[test]
    fn quality_outranks_ownership_and_ownership_breaks_the_tie() {
        let here = copy(9, "On now", "", "1", "720");
        let mine = copy(0, "Movies", "", "4", "1080");
        let theirs_4k = copy(1, "LDN Films", "bamx23", "318", "4k");
        let theirs_hd = copy(2, "Kino", "carol", "77", "1080");

        let rs = rows(&[here.clone(), mine.clone(), theirs_4k.clone(), theirs_hd.clone()], sid(9), "1");
        assert_eq!(
            labels(&rs),
            ["On now", "LDN Films", "Movies", "Kino"],
            "playing copy, then 4K, then the two 1080p with mine in front"
        );

        // the input order must not decide anything — the same set shuffled sorts identically
        let shuffled = rows(&[theirs_hd, theirs_4k, mine, here], sid(9), "1");
        assert_eq!(labels(&shuffled), labels(&rs));
    }

    /// The badge and the sort key are read off the SAME fields in the same precedence, so a list
    /// can never be ordered against a ladder the badges contradict. (The server's class wins over
    /// the stored frame size: a 2.35:1 1080p film is 1918x802, which a height rule would sort as
    /// 720p while its badge said 1080p.)
    #[test]
    fn the_sort_key_agrees_with_the_badge_it_is_drawn_beside() {
        let with = |res: &str, w: i64, h: i64| AltCopy { res: res.into(), width: w, height: h, ..Default::default() };
        let ladder = ["8k", "4k", "1080", "720", "576", "sd"];
        for pair in ladder.windows(2) {
            let (a, b) = (with(pair[0], 0, 0), with(pair[1], 0, 0));
            assert!(scan_lines(&a) > scan_lines(&b), "{} must outrank {}", pair[0], pair[1]);
        }
        // the class beats the frame size, exactly as `fmt::resolution` badges it
        let scope = with("1080", 1918, 802);
        assert_eq!(scan_lines(&scope), 1080);
        assert_eq!(crate::ui::fmt::resolution(&scope.res, scope.width, scope.height).as_deref(), Some("1080p"));
        // …and with no class at all both fall back to the frame, and still agree
        let noclass = with("", 3840, 2160);
        assert!(scan_lines(&noclass) > scan_lines(&with("", 1920, 1080)));
        assert_eq!(crate::ui::fmt::resolution(&noclass.res, noclass.width, noclass.height).as_deref(), Some("4K"));
        // a garbage height must not overflow into a top-of-list key
        assert!(scan_lines(&with("", i64::MAX, i64::MAX)) > 0);
    }

    /// **Exactly one copy is marked current** — the row model's whole claim about the tick. Marked
    /// once even when a producer lists the same copy twice, and marked NOWHERE (never twice, never
    /// on a guess) when the page's own copy is not in the list at all.
    #[test]
    fn exactly_one_row_is_ever_ticked() {
        let ticked = |rs: &[AltRow]| rs.iter().filter(|r| r.checked).count();

        let list = [copy(0, "Movies", "", "4", "1080"), copy(1, "LDN Films", "bamx23", "318", "4k")];
        let rs = rows(&list, sid(0), "4");
        assert_eq!(ticked(&rs), 1);
        assert!(rs[0].checked, "the tick is on the copy the page is standing on");

        // a duplicated copy: one identity, one tick
        let dup = [copy(0, "Movies", "", "4", "1080"), copy(0, "Movies", "", "4", "1080"), copy(1, "LDN", "b", "318", "4k")];
        assert_eq!(ticked(&rows(&dup, sid(0), "4")), 1, "one identity cannot be two 'you are here's");

        // the same ratingKey on the OTHER server is a different copy — rk alone must not tick it
        assert_eq!(ticked(&rows(&[copy(0, "Movies", "", "4", "1080"), copy(1, "LDN", "b", "4", "4k")], sid(0), "4")), 1);
        // …and nothing is ticked when the page's copy is not in the list yet
        assert_eq!(ticked(&rows(&list, sid(7), "4")), 0, "no guess, no second tick");
    }

    /// A copy the server sent no runtime for says whose it is and stops — never "· 0 min", the
    /// dangling-clause rule the hero's facts row follows.
    #[test]
    fn a_copy_with_no_runtime_states_only_its_owner() {
        let mut c = copy(1, "LDN Films", "bamx23", "318", "4k");
        c.dur_ms = 0;
        let rs = rows(&[c], sid(0), "4");
        assert_eq!(rs[0].detail, "bamx23");
        // and one with no video at all carries no badge rather than an empty chip
        let mut n = copy(0, "Movies", "", "4", "");
        n.dur_ms = 0;
        assert_eq!(rows(&[n], sid(0), "4")[0].badge, None);
    }

    /// OK NAVIGATES — and the row you are already on is not a destination: it reports nothing, so
    /// the panel simply dismisses rather than re-mounting the page under itself. An out-of-range
    /// selection is `None` too, never a neighbouring row's server.
    #[test]
    fn ok_navigates_to_another_copy_and_never_to_the_one_you_are_on() {
        let list = [(sid(0), "4".to_string()), (sid(1), "318".to_string()), (sid(2), String::new())];
        assert_eq!(action_at(&list, 0, sid(0), "4"), Action::None, "the copy you are on goes nowhere");
        assert_eq!(action_at(&list, 1, sid(0), "4"), Action::Open { sid: sid(1), rk: "318".into() });
        assert_eq!(action_at(&list, 2, sid(0), "4"), Action::None, "a copy with no ratingKey is not a destination");
        assert_eq!(action_at(&list, 3, sid(0), "4"), Action::None);
        assert_eq!(action_at(&list, -1, sid(0), "4"), Action::None);
        assert_eq!(action_at(&[], 0, sid(0), "4"), Action::None);
    }

    /// The headless stand-in describes what a device capture is looking at, so its SHAPE is graded
    /// here: your real copy plus the same film on a second slot, one class better — which is the
    /// design's own example, and the only arrangement in which a still can show that the ordering
    /// rule put the copy that PLAYS above the better one.
    #[test]
    fn the_headless_stand_in_shows_the_case_the_ordering_rule_turns_on() {
        let v = stand_in("bamx23", "Movies", "4", "1080", 7_020_000, sid(0), sid(1));
        assert_eq!(source_count(&v), 2, "…or the gate would refuse the very panel it exists to show");
        assert_eq!(v[0].owner, None, "your copy is the real one, on the current server");
        assert_eq!((v[0].rk.as_str(), v[0].dur_ms), ("4", 7_020_000));
        assert_eq!(v[1].owner.as_deref(), Some("bamx23"));
        assert_eq!(v[1].rk, v[0].rk, "the same film — the slot is what differs");

        let rs = rows(&v, sid(0), "4");
        assert!(rs[0].checked, "the tick is on yours…");
        assert_eq!(rs[0].badge.as_deref(), Some("1080p"));
        assert_eq!(rs[1].badge.as_deref(), Some("4K"), "…and the better copy is the one BELOW it");

        // the one invention is bounded: the top of the ladder is not promoted past itself, and an
        // unrecognised class is left exactly as the server spelled it
        assert_eq!(one_class_better("4k"), "4k");
        assert_eq!(one_class_better("sd"), "720");
        assert_eq!(one_class_better(""), "");
        assert_eq!(one_class_better("weird"), "weird");
    }

    /// The panel hangs off the control that opened it, and is never over it or off the screen.
    #[test]
    fn the_panel_hangs_off_its_button_and_stays_on_screen() {
        let btn = Rect::new(90.0, 300.0, 300.0, 60.0);
        let r = panel_at(btn, 224.0);
        assert_eq!(r.w, PANEL_W);
        assert_eq!(r.y, btn.y + btn.h + BTN_GAP, "under the button when there is room");
        assert_eq!(r.x, btn.x, "and aligned to its left edge");

        // a button low on the page flips the panel ABOVE it rather than off the bottom
        let low = Rect::new(90.0, 900.0, 300.0, 60.0);
        let r = panel_at(low, 224.0);
        assert!(r.y + r.h <= low.y - BTN_GAP + 0.01, "flipped above the button");
        assert!(r.y >= EDGE);

        // a button near the right edge pulls the panel back inside the keep-out
        let right = Rect::new(SCR_W - 200.0, 300.0, 180.0, 60.0);
        let r = panel_at(right, 224.0);
        assert!(r.x + r.w <= SCR_W - EDGE + 0.01, "a panel must not run off the panel");
        // …and a list taller than the screen is clamped rather than drawn past both edges
        let tall = panel_at(btn, 4000.0);
        assert!(tall.y >= EDGE && tall.y + tall.h <= SCR_H - EDGE + 0.01);
    }
}
