//! **"What goes on your Home?"** — the first-run question, asked once, when someone has shared a
//! library with you.
//!
//! A ROUTE, not a sheet over content: the same standing as sign-in and the who's-watching picker,
//! which is what the app's no-full-screen-sheets rule protects. It sits between the profile picker
//! and Home, has a beginning and an end, and never comes back — a first-run screen that returns is
//! not a first-run screen. It appears **only when the roster holds more than one source**; a
//! single-server install goes straight to Home exactly as it does today.
//!
//! **Nothing here grants or blocks access.** Every library it lists stays browsable from the
//! Library chip either way. It decides one thing: what reaches HOME. Three states are orthogonal
//! and only the middle one is a setting — *granted* (plex.tv's answer), *pinned* (this), *reachable*
//! (a fact about right now).
//!
//! ## What is drawn, and why it is drawn that way
//!
//! Two columns on the app's own ground, from the design system's route frame (`consts::ROUTE_*`):
//! the copy and the single action on the LEFT, the rows on the RIGHT. **No panel under the rows** —
//! a panel's depth claims there is a screen behind it, and a route has none.
//!
//! * **Your own libraries arrive `On`, a friend's arrive `Off`** ([`default_pin`]). The point of
//!   asking is not to put a stranger's shelves on your front door unannounced; the opposite default
//!   (ask nothing, pin everything) was considered and rejected, as was starting the list empty,
//!   which makes the user answer a question they answered by owning a server.
//! * **Focus starts on *Start watching***, so the fast path is one press: the defaults are already
//!   a sensible answer, and the rows are one press RIGHT.
//! * **BACK skips, and the skip is honest** — it commits what is on screen, which for an untouched
//!   screen is exactly those defaults. No modal insisting, no second prompt later.
//! * The copy names the **people**; machine names stay in the group headers. That split ("people in
//!   content, machines in settings") is the whole design's rule, not this screen's.
//! * An **unreachable** source keeps its rows, dimmed at one alpha (`table::Section::dim`). A share
//!   you cannot reach on the evening you set the app up is still a share.
//!
//! ## The answer, and who reads it
//!
//! [`is_pinned`] is the answer for one library, and it is the ONE place the arrival default lives
//! beside the recorded decision: an entry when somebody decided, the ownership default when nobody
//! has. That is what keeps a library that appears LATER honest without any extra state — a new
//! share arrives Off Home (the design's rule) and a new library of your own arrives On (today's
//! behaviour, and the same sentence in the other direction). [`commit`] materialises every library
//! that was on screen, so an explicit "not this one" survives a change of default.
//!
//! It is persisted in the session file (`plex::session::PinRef`), keyed by **machineIdentifier +
//! section key**: a section key is only unique within its own server — both servers in the live
//! capture call their first library `1`.
#![allow(non_upper_case_globals)]
use crate::plex::session::PinRef;
use crate::plex::ServerId;
use crate::ui::consts::*;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Button, Spinner};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::ffi::CString;
use std::os::raw::c_uint;
use std::ptr::addr_of_mut;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

/// The heading. A question, because that is what the screen is.
const TITLE: &str = "What goes on your Home?";
/// The one action. A verb that says what happens next, rather than "Continue", which says nothing.
const ACTION: &str = "Start watching";
/// How the OWN server's group names its person. It is an account, not a stranger — and writing the
/// signed-in user's own handle here would be the one place the app tells you who you are.
const OWN_PERSON: &str = "This account";
/// A share whose owner we have not been told (the credentials came from a dev trigger, or plex.tv
/// sent no `sourceTitle`). Still a person-shaped statement rather than a machine one.
const SOME_PERSON: &str = "Shared with you";
/// Appended to a source's accessory when its libraries could not be fetched — the group keeps its
/// place and its rows, and says why it is quiet.
const UNREACHABLE_NOTE: &str = "Can't be reached";

// ---- the roster this screen asks about ------------------------------------------------------

/// One library a source grants.
#[derive(Clone)]
pub(crate) struct Lib {
    /// The section key — unique within its server only.
    pub key: i64,
    pub title: String,
    /// Item count, **negative while unknown** (see `fmt::library_count`).
    pub count: i64,
    pub is_show: bool,
}

/// What we know about one source's library list. `Loading` is the boot state; `Unreachable` is a
/// FETCH that failed, which is a fact about tonight and not about the grant.
#[derive(Clone)]
enum Libs {
    Loading,
    Ready(Vec<Lib>),
    Unreachable,
}

/// One SOURCE the question is about: a server, the person whose it is, and what it grants.
pub(crate) struct Source {
    /// Registry slot — how the fetch worker reaches this server.
    sid: ServerId,
    /// `machineIdentifier`: the identity the answer is PERSISTED against. May be empty at boot on
    /// the legacy address-keyed install path, in which case the fetch resolves it from `/identity`.
    machine_id: String,
    /// The MACHINE name — the group header.
    server: String,
    /// The PERSON — the group accessory. "" when nobody told us (see [`SOME_PERSON`]).
    person: String,
    own: bool,
    libs: Libs,
    /// A fetch is out for this source (single-flight; the screen can be entered more than once).
    inflight: bool,
}

impl Source {
    /// The group's right-hand accessory: whose this is, plus — only when we tried and failed —
    /// why the group is quiet.
    fn accessory(&self) -> String {
        let who = if self.own {
            OWN_PERSON
        } else if self.person.is_empty() {
            SOME_PERSON
        } else {
            self.person.as_str()
        };
        match self.libs {
            Libs::Unreachable => format!("{who} \u{b7} {UNREACHABLE_NOTE}"),
            _ => who.to_string(),
        }
    }
    /// The libraries to draw — none until the fetch lands, and none for a source we could not reach
    /// on this boot (there is no cached grant to fall back to on a FIRST run).
    fn rows(&self) -> &[Lib] {
        match &self.libs {
            Libs::Ready(v) => v,
            _ => &[],
        }
    }
}

/// The roster. A `Mutex` and not a `static mut` because the fetch workers land into it; it is read
/// on the main thread when the sections are rebuilt, which is on a CHANGE, not per frame.
static SOURCES: Mutex<Vec<Source>> = Mutex::new(Vec::new());
/// The recorded answer, in memory — see [`is_pinned`] for how an absent entry is read.
static PINS: Mutex<Vec<PinRef>> = Mutex::new(Vec::new());
/// Has the question been answered? Only [`commit`] and [`restore`] write it.
static ASKED: AtomicBool = AtomicBool::new(false);
/// The drawn sections are stale — a landing arrived or a toggle flipped. Rebuilt in [`update`]
/// rather than per frame: a rebuild allocates a `CString` per row.
static DIRTY: AtomicBool = AtomicBool::new(false);

/// Where the ring is. The screen has two focus containers and the list is the one that scrolls, so
/// this is a two-value cursor rather than a row index — the row cursor is the `TableView`'s own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Action,
    Rows,
}

static mut TABLE: TableView = TableView::new();
static mut FOCUS: Focus = Focus::Action;
/// Spinner phase while no source has answered yet (ms).
static mut SPIN_MS: f32 = 0.0;
/// Seconds until [`update`] re-offers the library fetches — see the call, and [`KICK_RETRY_S`].
static mut KICK_CD: f32 = 0.0;
/// How often a source whose worker was REFUSED is offered another one.
const KICK_RETRY_S: f32 = 1.0;

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
fn focus() -> Focus {
    unsafe { *addr_of_mut!(FOCUS) }
}
fn set_focus(f: Focus) {
    unsafe { addr_of_mut!(FOCUS).write(f) };
    table().focused = f == Focus::Rows;
}

// ---- the answer -----------------------------------------------------------------------------

/// The state a library ARRIVES in, before anybody has answered: your own On, a friend's Off.
fn default_pin(own: bool) -> bool {
    own
}

/// Is this library on Home?
///
/// The recorded decision when there is one, and the ownership default when there is not — which is
/// both the pre-answer state AND the rule for a library that appears later, because [`commit`]
/// materialises everything that was on screen. So "no entry" can only mean "we have never shown you
/// this library", and for that the honest answer is the same one it would have arrived with.
#[allow(dead_code)] // the reader is Home's shelf merge, which is a separate unit of this feature
pub(crate) fn is_pinned(machine_id: &str, key: i64, own: bool) -> bool {
    let pins = PINS.lock().unwrap_or_else(|e| e.into_inner());
    pin_of(&pins, machine_id, key).unwrap_or(default_pin(own))
}

/// The recorded state for one library, if any — the pure half of [`is_pinned`].
fn pin_of(pins: &[PinRef], machine_id: &str, key: i64) -> Option<bool> {
    pins.iter().find(|p| p.machine_id == machine_id && p.key == key).map(|p| p.on)
}

/// Record a decision (in memory; [`commit`] is what persists).
///
/// **An unidentified server records nothing.** A pin keyed `""` is not a pin on one library, it is
/// a pin on that section number of every server we could not name — and section keys start at 1 on
/// each of them, so two such servers would share every row. A source only ever reaches the screen
/// with an id resolved (see [`fetch_libs`]); this is the guard that keeps that an invariant rather
/// than a convention.
fn set_pin(pins: &mut Vec<PinRef>, machine_id: &str, key: i64, on: bool) {
    if machine_id.is_empty() {
        return;
    }
    match pins.iter_mut().find(|p| p.machine_id == machine_id && p.key == key) {
        Some(p) => p.on = on,
        None => pins.push(PinRef { machine_id: machine_id.to_string(), key, on }),
    }
}

/// Every library currently on screen, with the state it is showing — the whole answer, explicit.
///
/// Materialising is what makes the answer a record of what the user was SHOWN rather than of what
/// they changed: an untouched row carries a default today, and a default is allowed to change.
fn materialize(sources: &[Source], pins: &mut Vec<PinRef>) {
    for s in sources {
        for l in s.rows() {
            let on = pin_of(pins, &s.machine_id, l.key).unwrap_or(default_pin(s.own));
            set_pin(pins, &s.machine_id, l.key, on);
        }
    }
}

// ---- the gate -------------------------------------------------------------------------------

/// Should the question be asked on this boot?
///
/// `forced` is the dev trigger, which is why it is an argument and not a read inside: it must win
/// over both other terms so a capture can reach the screen on a set whose session has long since
/// answered. `automated` is the same rule the who's-watching picker follows — a headless run has to
/// land on a deterministic Home, or every scene grades the wrong screen.
///
/// **`asked` is only ever set by an answer**, never by a boot that had one source. A user whose
/// first share arrives in a year meets this screen then, once, which is the first moment the
/// question means anything at all.
fn gate(sources: usize, asked: bool, automated: bool, forced: bool) -> bool {
    forced || (!automated && sources > 1 && !asked)
}

/// [`gate`], against the live roster and the dev trigger.
///
/// dev: **`/tmp/plxnative-firstrun`** forces this screen up whatever the session and the roster say
/// — the only way to photograph a screen that, by design, a given television shows exactly once.
/// (The literal is spelled out here because the trigger catalog is a grep for it; the read itself
/// goes through `dev::flag`, like every other one.)
pub(crate) fn should_show(automated: bool) -> bool {
    gate(source_count(), ASKED.load(Relaxed), automated, crate::dev::flag("firstrun"))
}

pub(crate) fn source_count() -> usize {
    SOURCES.lock().map(|v| v.len()).unwrap_or(0)
}

// ---- boot: who is on the roster -------------------------------------------------------------

/// Seed the recorded answer from the persisted session. Boot only, before any source is added.
pub(crate) fn restore(sess: &crate::plex::session::Session) {
    ASKED.store(sess.home_pins_asked, Relaxed);
    if let Ok(mut p) = PINS.lock() {
        *p = sess.home_pins.clone();
    }
}

/// Register a source the question is about. Idempotent per registry slot, so a re-install (a
/// profile switch re-registers the same server) updates the names rather than adding a twin.
pub(crate) fn add_source(sid: ServerId, machine_id: &str, server: &str, person: &str, own: bool) {
    let mut v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = v.iter_mut().find(|s| s.sid == sid) {
        s.machine_id = machine_id.to_string();
        s.server = server.to_string();
        s.person = person.to_string();
        s.own = own;
        return;
    }
    v.push(Source {
        sid,
        machine_id: machine_id.to_string(),
        server: server.to_string(),
        person: person.to_string(),
        own,
        libs: Libs::Loading,
        inflight: false,
    });
    DIRTY.store(true, Relaxed);
}

/// Forget every source (an identity change: a different account's roster is a different question).
pub(crate) fn reset_sources() {
    if let Ok(mut v) = SOURCES.lock() {
        v.clear();
    }
    DIRTY.store(true, Relaxed);
}

// ---- the library fetch ----------------------------------------------------------------------

/// One worker's answer: the server's id (learned if we did not have it) and its libraries, or
/// `None` for a server that would not answer.
struct Landing {
    sid: ServerId,
    machine_id: String,
    libs: Option<Vec<Lib>>,
}
static LANDINGS: Mutex<Vec<Landing>> = Mutex::new(Vec::new());

/// Ask every source that has not been asked yet what it grants. One worker per source: a shared
/// server is over the WAN and a dead one costs a connect timeout, so serialising them would make
/// the last group's rows wait on the slowest server in the house.
fn kick() {
    let mut v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    for s in v.iter_mut() {
        if s.inflight || !matches!(s.libs, Libs::Loading) {
            continue;
        }
        let (sid, mid) = (s.sid, s.machine_id.clone());
        // Capture the slot at the SPAWN site and never read "the current server" inside the
        // worker — the rule `browse.rs` states, and the reason a shared server's libraries cannot
        // end up under your own server's header.
        s.inflight = crate::task::spawn_small("firstrun-libs", move || {
            let got = fetch_libs(sid, &mid);
            let learned = got.as_ref().map(|(m, _)| m.clone()).unwrap_or(mid);
            let libs = got.map(|(_, l)| l);
            if let Ok(mut land) = LANDINGS.lock() {
                land.push(Landing { sid, machine_id: learned, libs });
            }
            // A landing repaints, and no spring is involved.
            crate::ui::idle::invalidate();
        });
    }
}

/// What one server grants, plus its id. `None` for a server that would not answer at all — the
/// group then draws dimmed rather than empty-and-silent.
fn fetch_libs(sid: ServerId, known_id: &str) -> Option<(String, Vec<Lib>)> {
    let c = crate::plex::client_for(sid)?;
    let mc = c.sections()?;
    // Only now, with the server proven to answer: the id we persist the answer against. The legacy
    // address-keyed `install` registers with an EMPTY one, so this is the common path for your own
    // server, not a corner.
    let mid = match (known_id.is_empty(), c.machine_id().is_empty()) {
        (false, _) => known_id.to_string(),
        (true, false) => c.machine_id().to_string(),
        (true, true) => c.machine_identity().unwrap_or_default(),
    };
    // A server that will not say WHO IT IS cannot have an answer recorded against it — a pin keyed
    // `""` would be a pin on that section number of every unnamed server at once, and both servers
    // in the live capture call their first library `1`. So it is treated as unreachable: the group
    // keeps its place and its name, dimmed, rather than offering switches that could not be saved.
    // (`/identity` is unauthenticated and this server has just answered `/library/sections`, so in
    // practice this is a server that died between two requests.)
    if mid.is_empty() {
        crate::log("firstrun: a source would not identify itself — no answer can be keyed to it");
        return None;
    }
    let mut libs: Vec<Lib> = Vec::new();
    for d in &mc.directory {
        let is_show = match d.kind.as_str() {
            "movie" => false,
            "show" => true,
            _ => continue, // music/photo: this app cannot browse them, so it must not offer them
        };
        let Ok(key) = d.key.parse::<i64>() else { continue };
        // One item-less listing per section for its COUNT — `/library/sections` does not carry one,
        // and a row that says only "Movies" makes the user open a library to find out how big it is.
        // Container-Size 0, so the server counts and sends no items.
        let count = c
            .section_items_paged(key, 0, 0)
            .map(|m| m.total_size.max(m.size))
            .unwrap_or(-1);
        libs.push(Lib { key, title: d.title.clone(), count, is_show });
    }
    Some((mid, libs))
}

/// Apply whatever the workers have answered. Main thread, every frame this screen updates.
fn pump() {
    let landed: Vec<Landing> = match LANDINGS.lock() {
        Ok(mut l) if !l.is_empty() => std::mem::take(&mut *l),
        _ => return,
    };
    let mut v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    for land in landed {
        let Some(s) = v.iter_mut().find(|s| s.sid == land.sid) else { continue };
        s.inflight = false;
        if !land.machine_id.is_empty() {
            s.machine_id = land.machine_id;
        }
        s.libs = match land.libs {
            Some(l) => Libs::Ready(l),
            None => Libs::Unreachable,
        };
        crate::log(&format!(
            "firstrun: {} libraries={}",
            if s.own { "own server" } else { "shared server" },
            match &s.libs {
                Libs::Ready(l) => l.len() as i64,
                _ => -1,
            }
        ));
    }
    DIRTY.store(true, Relaxed);
}

// ---- rows -----------------------------------------------------------------------------------

/// The drawn list: one group per source, one row per library, the value word carrying its state.
///
/// Row ORDER is the roster's order, which is what makes [`row_at`] a mapping rather than a search —
/// the flip and the draw read one list.
fn build_sections(sources: &[Source], pins: &[PinRef]) -> Vec<Section> {
    sources
        .iter()
        .map(|s| {
            let mut sec = Section::new(s.server.clone())
                .accessory(s.accessory())
                .dim(matches!(s.libs, Libs::Unreachable));
            for l in s.rows() {
                let on = pin_of(pins, &s.machine_id, l.key).unwrap_or(default_pin(s.own));
                sec = sec.row(
                    Row::new(l.title.clone())
                        .detail(crate::ui::fmt::library_count(l.count, l.is_show))
                        .toggle(on),
                );
            }
            sec
        })
        .collect()
}

/// Global row index → (source, library) — the ONE mapping between a drawn row and the roster.
fn row_at(sources: &[Source], gi: i32) -> Option<(usize, usize)> {
    let mut n = 0i32;
    for (si, s) in sources.iter().enumerate() {
        for (li, _) in s.rows().iter().enumerate() {
            if n == gi {
                return Some((si, li));
            }
            n += 1;
        }
    }
    None
}

/// Flip the selected row's switch. Returns whether anything changed (an empty list cannot).
fn flip(sources: &[Source], pins: &mut Vec<PinRef>, gi: i32) -> bool {
    let Some((si, li)) = row_at(sources, gi) else { return false };
    let s = &sources[si];
    let l = &s.rows()[li];
    let now = pin_of(pins, &s.machine_id, l.key).unwrap_or(default_pin(s.own));
    set_pin(pins, &s.machine_id, l.key, !now);
    true
}

// ---- the copy -------------------------------------------------------------------------------

/// The people who have shared with you, in roster order, deduplicated — one person can share two
/// servers and would otherwise be named twice in one sentence.
fn people(sources: &[Source]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in sources.iter().filter(|s| !s.own) {
        let who = if s.person.is_empty() { SOME_PERSON.to_string() } else { s.person.clone() };
        if !out.contains(&who) {
            out.push(who);
        }
    }
    out
}

/// "bamx23 and kate.w" — an English list, because this sentence names PEOPLE and a comma-separated
/// machine list is exactly the voice this screen is avoiding.
fn join_people(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The paragraph under the heading. It names the people, says what the list is for, and says the
/// thing that makes skipping safe: nothing here decides what you can BROWSE.
fn body_copy(names: &[String], shared_libs: usize) -> String {
    const TAIL: &str = "Pick the ones you want on your Home screen \u{2014} you can browse any of \
                        them from the Library chip whenever you like.";
    if names.is_empty() {
        // Two servers, both yours — no one to name, so the sentence is only the instruction. (The
        // screen is about sources, not about sharing: an account CAN own two servers.)
        return "Pick the libraries you want on your Home screen \u{2014} you can browse any of \
                them from the Library chip whenever you like."
            .to_string();
    }
    let verb = if names.len() == 1 { "has" } else { "have" };
    // "a library" only when we KNOW there is exactly one; while the lists are still landing the
    // plural is the safe claim (it can only be wrong about a single-library share for a moment).
    let what = if shared_libs == 1 { "a library" } else { "libraries" };
    format!("{} {verb} shared {what} with you. {TAIL}", join_people(names))
}

// ---- the screen -----------------------------------------------------------------------------

/// Mount the question. Focus starts on the action, so the fast path is one press.
pub(crate) fn enter() {
    set_focus(Focus::Action);
    table().sel = 0;
    // its groups ARE its content while the servers answer — see `TableView::empty_note`
    table().empty_note = None;
    DIRTY.store(true, Relaxed);
    kick();
    crate::log(&format!("firstrun: asking about {} sources", source_count()));
}

pub(crate) fn update(dt: f32) {
    unsafe { *addr_of_mut!(SPIN_MS) += dt * 1000.0 };
    pump();
    // Re-ask for anything still un-asked. `kick` is single-flight, so this is a no-op in the normal
    // case; it exists because `task::spawn_small` can REFUSE (it returns false rather than panicking
    // — the whole reason that module exists), and a refused spawn left this screen spinning at a
    // source that would never be fetched again. On a cooldown so a machine that is out of threads
    // is asked once a second rather than sixty times.
    unsafe {
        *addr_of_mut!(KICK_CD) -= dt;
        if *addr_of_mut!(KICK_CD) <= 0.0 {
            *addr_of_mut!(KICK_CD) = KICK_RETRY_S;
            kick();
        }
    }
    if DIRTY.swap(false, Relaxed) {
        let v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
        let pins = PINS.lock().unwrap_or_else(|e| e.into_inner());
        let sel = table().sel;
        // slide, not snap: the only thing that changes under the pill is a word at the row's
        // trailing edge, so the highlight must stay exactly where the user left it.
        table().set_sections(build_sections(&v, &pins), sel, true);
        table().focused = focus() == Focus::Rows;
    }
    table().update(dt, list_rect().h - crate::ui::table::PAD_V);
}

/// The list column — its right edge is the page margin and its height is bounded by the action's
/// own bottom line, so a long roster is cut level with the control instead of running off the frame.
fn list_rect() -> Rect {
    let h = table().measured_height().min(ROUTE_LIST_H_MAX);
    Rect::new(ROUTE_LIST_X, ROUTE_TOP, ROUTE_LIST_W, h)
}

/// The action pill — measured from its own label (`Button::pill_w`), bottom-anchored.
fn action_rect() -> Rect {
    let w = Button::pill_w(action_label().as_ptr(), theme::size::BODY, false);
    let y = SCR_H as f32 - ROUTE_ACTION_BOTTOM - ROUTE_ACTION_H;
    Rect::new(ROUTE_COPY_X, y, w, ROUTE_ACTION_H)
}

fn action_label() -> CString {
    CString::new(ACTION).unwrap_or_default()
}

pub(crate) fn draw() {
    // The clear IS the app surface (`theme::CLEAR_RGB` == `SURFACE_APP`), which is the ground both
    // columns sit on: no sheet, no radius, no shadow anywhere on this screen.
    crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    let p = Painter::root();
    let env = Env::inert();

    let (names, shared_libs, loading) = {
        let v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
        let shared = v.iter().filter(|s| !s.own).map(|s| s.rows().len()).sum::<usize>();
        (people(&v), shared, v.iter().any(|s| matches!(s.libs, Libs::Loading)))
    };

    // ---- left column: what this is, and the one way out of it ----
    let title = TextView::new(TITLE, theme::size::HERO, theme::TEXT_HEADING).bold().max_lines(2);
    let th = title.draw(p, Rect::new(ROUTE_COPY_X, ROUTE_TOP, ROUTE_COPY_W, 0.0));
    let body = body_copy(&names, shared_libs);
    TextView::new(&body, theme::size::BODY, theme::TEXT_READING).max_lines(6).draw(
        p,
        Rect::new(ROUTE_COPY_X, ROUTE_TOP + th + theme::space::MD, ROUTE_COPY_W, 0.0),
    );
    let label = action_label();
    Button::new(label.as_ptr(), theme::size::BODY, action_rect())
        .focused(focus() == Focus::Action)
        .draw(&env, p);

    // ---- right column: the rows themselves, on the same ground ----
    // Drawn from the first frame, headers and all: each group is a server, and naming them before
    // they answer is what makes the column fill in rather than appear. (`empty_note` is off for
    // exactly that reason — the placeholder would claim the list is empty while it is early.)
    let lr = list_rect();
    table().draw(p, lr);
    if loading {
        // …with the wait stated UNDER the list, where the rows still to come will be. It reports to
        // `ui::idle` by being drawn, which is what keeps the frame alive while the fetches are out.
        let ph = unsafe { *addr_of_mut!(SPIN_MS) } as u32;
        Spinner::new(lr.x + lr.w * 0.5, lr.y + lr.h + theme::space::LG, Spinner::R_PAGE)
            .phase(ph)
            .tint(theme::TEXT_TERTIARY)
            .draw(&env, p);
    }
}

/// A key on this screen. Returns **true when the question is answered** and the caller should route
/// on to Home — the screen never routes itself, exactly as the sign-in and picker screens do not.
pub(crate) fn key(sym: c_uint, wcode: c_uint) -> bool {
    if is_back(sym, wcode) {
        // The skip, and it is honest: whatever is on screen is what gets recorded, which for an
        // untouched screen is your own libraries pinned and everyone else's off Home.
        commit();
        return true;
    }
    if is_ok(sym) {
        match focus() {
            Focus::Action => {
                commit();
                return true;
            }
            Focus::Rows => toggle_selected(),
        }
        return false;
    }
    match sym {
        SDLK_RIGHT => {
            if table().n_rows() > 0 {
                set_focus(Focus::Rows);
            }
        }
        SDLK_LEFT => set_focus(Focus::Action),
        SDLK_UP if focus() == Focus::Rows => table().move_sel(-1),
        SDLK_DOWN if focus() == Focus::Rows => table().move_sel(1),
        _ => {}
    }
    false
}

/// Pointer hover: the ring follows the cursor between the two columns.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    if action_rect().contains(mx, my) {
        set_focus(Focus::Action);
        return;
    }
    if let Some(gi) = table().hit_row(list_rect(), mx, my) {
        set_focus(Focus::Rows);
        table().sel = gi;
    }
}

/// Pointer click — the same two commitments as OK. Returns true when the question is answered.
pub(crate) fn click(mx: f32, my: f32) -> bool {
    if action_rect().contains(mx, my) {
        commit();
        return true;
    }
    if let Some(gi) = table().hit_row(list_rect(), mx, my) {
        set_focus(Focus::Rows);
        table().sel = gi;
        toggle_selected();
    }
    false
}

fn toggle_selected() {
    let v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    let mut pins = PINS.lock().unwrap_or_else(|e| e.into_inner());
    if flip(&v, &mut pins, table().sel) {
        DIRTY.store(true, Relaxed);
    }
}

/// Answer the question: freeze what is on screen, mark it asked, and persist both.
///
/// Reached by BOTH exits — the action and BACK — because a skip is an answer too, and the whole
/// promise of "asked once" is that neither of them can leave the question open.
fn commit() {
    let pins = record_answer();
    // `load`, not `peek`: it is the one reader that guarantees a `client_id` (minting one if the
    // file has never been written), so a save built on it can never drop the identity the whole
    // session hangs off.
    let mut sess = crate::plex::session::load();
    sess.home_pins = pins;
    sess.home_pins_asked = true;
    crate::plex::session::save(&sess);
}

/// The half of [`commit`] that is only state: materialise the answer and close the question.
/// Split out from the file write so it can be driven in a test — a host suite that persisted would
/// be writing a real session file on somebody's machine.
fn record_answer() -> Vec<PinRef> {
    let v = SOURCES.lock().unwrap_or_else(|e| e.into_inner());
    let mut pins = PINS.lock().unwrap_or_else(|e| e.into_inner());
    materialize(&v, &mut pins);
    ASKED.store(true, Relaxed);
    let on = pins.iter().filter(|p| p.on).count();
    crate::log(&format!("firstrun: answered — {on} of {} libraries on Home", pins.len()));
    pins.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(server: &str, person: &str, own: bool, libs: &[(i64, &str, i64, bool)]) -> Source {
        Source {
            sid: ServerId::UNSET,
            machine_id: format!("mid-{server}"),
            server: server.to_string(),
            person: person.to_string(),
            own,
            libs: Libs::Ready(
                libs.iter()
                    .map(|&(key, title, count, is_show)| Lib {
                        key,
                        title: title.to_string(),
                        count,
                        is_show,
                    })
                    .collect(),
            ),
            inflight: false,
        }
    }

    /// The roster of the design's own frame: your server and two friends'.
    fn roster() -> Vec<Source> {
        vec![
            src("mac-mini", "", true, &[(1, "Movies", 412, false), (2, "TV Shows", 238, true)]),
            src("bx23-ldn", "bamx23", false, &[(1, "LDN Films", 185, false)]),
            src("kate-nas", "kate.w", false, &[(1, "Movies", 96, false), (3, "Kids", 34, false)]),
        ]
    }

    /// The whole reason the screen is conditional: with one source there is no question, and 90% of
    /// installs are that case. With two there is, exactly once — and an automated boot never sees it
    /// (a headless run must land on a deterministic Home) unless the trigger asks for it by name.
    #[test]
    fn the_question_is_asked_only_when_it_means_something() {
        assert!(gate(2, false, false, false), "two sources, never asked");
        assert!(!gate(1, false, false, false), "a single-server install goes straight to Home");
        assert!(!gate(0, false, false, false), "no server at all is the sign-in screen's problem");
        assert!(!gate(2, true, false, false), "asked once means once");
        assert!(!gate(2, false, true, false), "an automated boot lands on Home, like the picker rule");
        // the dev trigger beats every other term, which is what makes the screen capturable on a
        // set whose session answered months ago
        assert!(gate(1, true, true, true), "the trigger forces it");
    }

    /// Your own libraries arrive On and a friend's arrive Off — stated as the WORD at the row's
    /// trailing edge, which is what a switch says and a tick does not.
    #[test]
    fn own_libraries_arrive_on_and_a_friends_arrive_off() {
        let v = roster();
        let secs = build_sections(&v, &[]);
        assert_eq!(secs.len(), 3, "one group per source");
        assert_eq!(secs[0].header, "mac-mini", "the header names the MACHINE");
        assert_eq!(secs[0].accessory, OWN_PERSON, "the accessory names the person");
        assert_eq!(secs[1].accessory, "bamx23");
        assert_eq!(secs[0].rows.iter().map(|r| r.toggle).collect::<Vec<_>>(), vec![Some(true), Some(true)]);
        assert_eq!(secs[1].rows[0].toggle, Some(false));
        assert_eq!(secs[2].rows.iter().map(|r| r.toggle).collect::<Vec<_>>(), vec![Some(false), Some(false)]);
        // the sub-line says how big the library is, in its own units
        assert_eq!(secs[0].rows[1].detail, "238 shows");
        assert_eq!(secs[1].rows[0].detail, "185 films");
    }

    /// BACK records what is on screen, and on an untouched screen that IS the honest default: your
    /// own libraries stay pinned, everyone else's stay off Home. Nothing is left undecided.
    #[test]
    fn skipping_records_the_honest_default() {
        let v = roster();
        let mut pins: Vec<PinRef> = Vec::new();
        materialize(&v, &mut pins);
        assert_eq!(pins.len(), 5, "every library that was on screen is recorded");
        assert!(pin_of(&pins, "mid-mac-mini", 1).unwrap());
        assert!(pin_of(&pins, "mid-mac-mini", 2).unwrap());
        assert!(!pin_of(&pins, "mid-bx23-ldn", 1).unwrap());
        assert!(!pin_of(&pins, "mid-kate-nas", 3).unwrap());
        // …and the two servers' section `1` are two different libraries, which is the whole reason
        // a pin is keyed by machineIdentifier AND key (both servers in the live capture use 1)
        assert_ne!(pin_of(&pins, "mid-mac-mini", 1), pin_of(&pins, "mid-kate-nas", 1));
    }

    /// OK on a row flips exactly that row, and the flip survives the answer. The row it flips is
    /// found by the same index the list drew, so a group of a different size cannot shift it.
    #[test]
    fn a_flip_moves_one_row_and_survives_the_answer() {
        let v = roster();
        let mut pins: Vec<PinRef> = Vec::new();
        assert!(flip(&v, &mut pins, 2), "global row 2 is the first source's shared library");
        assert_eq!(row_at(&v, 2), Some((1, 0)));
        assert!(pin_of(&pins, "mid-bx23-ldn", 1).unwrap(), "bamx23's library is now on Home");
        materialize(&v, &mut pins);
        assert!(pin_of(&pins, "mid-bx23-ldn", 1).unwrap(), "the answer keeps it");
        assert!(pin_of(&pins, "mid-mac-mini", 1).unwrap(), "…and the untouched rows keep theirs");
        assert!(!flip(&v, &mut pins, 99), "a row that is not drawn is not a row");
        assert_eq!(row_at(&v, 5), None);
    }

    /// A library nobody has been shown takes its arrival default — which is what makes a share that
    /// turns up LATER arrive off Home without any extra state, and a new library of your own arrive
    /// on it. An explicit decision always wins over both.
    #[test]
    fn a_library_with_no_record_takes_its_arrival_default() {
        let pins = vec![PinRef { machine_id: "mid-a".into(), key: 1, on: false }];
        assert_eq!(pin_of(&pins, "mid-a", 1), Some(false), "an explicit OFF is a decision");
        assert_eq!(pin_of(&pins, "mid-a", 9), None);
        assert!(default_pin(true), "your own library arrives On");
        assert!(!default_pin(false), "a friend's arrives Off");
    }

    /// A server that will not say who it is cannot be answered ABOUT: a pin keyed `""` would be a
    /// pin on that section number of every unnamed server, and every server numbers its first
    /// library `1`. `fetch_libs` turns that into an unreachable source before it can reach a row;
    /// this pins the last line of defence, so the invariant survives a future caller.
    #[test]
    fn an_unidentified_server_records_nothing() {
        let mut pins: Vec<PinRef> = Vec::new();
        set_pin(&mut pins, "", 1, true);
        assert!(pins.is_empty(), "a nameless server must not claim section 1 for all of them");
        let mut v = roster();
        v[1].machine_id = String::new();
        materialize(&v, &mut pins);
        assert_eq!(pins.len(), 4, "the four libraries whose servers ARE named");
        assert!(pins.iter().all(|p| !p.machine_id.is_empty()));
        // …and it reads back as its arrival default rather than as somebody else's decision
        assert!(!is_pinned("", 1, false));
    }

    /// A source we could not reach keeps its place and says why — dimmed as a whole GROUP, header
    /// included, because unreachability is a fact about the source and not about any one row.
    #[test]
    fn an_unreachable_source_keeps_its_place_and_says_why() {
        let mut v = roster();
        v[1].libs = Libs::Unreachable;
        let secs = build_sections(&v, &[]);
        assert!(secs[1].dim, "the whole group carries the weight");
        assert_eq!(secs[1].accessory, "bamx23 \u{b7} Can't be reached");
        assert!(secs[1].rows.is_empty(), "no cached grant exists on a FIRST run");
        assert!(!secs[0].dim, "…and the reachable ones are untouched");
        // the rows that ARE drawn still map by their drawn order, with the dead group contributing
        // none of them
        assert_eq!(row_at(&v, 2), Some((2, 0)));
    }

    /// The copy names the people, in a sentence, with the number agreement right — the machine
    /// names stay in the group headers where they identify which box a library sits on.
    #[test]
    fn the_copy_names_the_people() {
        let v = roster();
        let names = people(&v);
        assert_eq!(names, vec!["bamx23".to_string(), "kate.w".to_string()]);
        let copy = body_copy(&names, 3);
        assert!(copy.starts_with("bamx23 and kate.w have shared libraries with you."), "{copy}");
        assert!(copy.contains("browse any of them from the Library chip"), "the skip is safe: {copy}");
        assert!(!copy.contains("bx23-ldn"), "a machine name must not reach the copy: {copy}");
        // one person with one library, which is the commonest shape of all
        assert_eq!(join_people(&["bamx23".to_string()]), "bamx23");
        assert!(body_copy(&["bamx23".to_string()], 1).starts_with("bamx23 has shared a library with you."));
        // three, so the list separator and the final "and" are both exercised
        let three: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert_eq!(join_people(&three), "a, b and c");
        // a share whose owner nobody named is still described as a person, never as a machine
        let anon = vec![src("box", "", false, &[(1, "Films", 4, false)])];
        assert_eq!(people(&anon), vec![SOME_PERSON.to_string()]);
        // …and two servers that are BOTH yours name nobody and claim no sharing, with the
        // instruction still stated exactly once
        let mine = body_copy(&[], 0);
        assert!(!mine.contains("shared"), "{mine}");
        assert_eq!(mine.matches("Pick").count(), 1, "{mine}");
    }

    /// The end-to-end promise, on the real globals: a roster of two sources asks, answering it
    /// records every library that was on screen, and from then on nothing asks again — a boot, a
    /// profile switch, a share that arrives next year.
    ///
    /// Takes `testlock::serial()` for its whole body: the roster, the pins and the asked flag are
    /// crate globals, and `restore` is the only thing that can put them back.
    #[test]
    fn answering_closes_the_question_for_good() {
        let _serial = crate::testlock::serial();
        let restore_to = crate::plex::session::Session::default();
        if let Ok(mut v) = SOURCES.lock() {
            *v = roster();
        }
        if let Ok(mut p) = PINS.lock() {
            p.clear();
        }
        ASKED.store(false, Relaxed);

        assert!(should_show(false), "two sources and no answer: ask");
        assert!(!should_show(true), "…but never on an automated boot");
        let pins = record_answer();
        assert!(!should_show(false), "answered once is answered");
        assert_eq!(pins.len(), 5, "the answer names every library that was on screen");
        assert!(is_pinned("mid-mac-mini", 1, true), "your own library stayed on Home");
        assert!(!is_pinned("mid-bx23-ldn", 1, false), "a friend's stayed off it");
        // a library nobody was shown still answers by its arrival default, which is what makes a
        // later share arrive off Home with no extra state
        assert!(!is_pinned("mid-new-friend", 1, false));
        assert!(is_pinned("mid-mac-mini", 77, true));

        // …and the whole thing comes back from the session file, which is what makes it survive a
        // reboot rather than being re-asked every night
        let saved = crate::plex::session::Session { home_pins: pins, home_pins_asked: true, ..restore_to.clone() };
        restore(&crate::plex::session::Session::default());
        assert!(!ASKED.load(Relaxed), "a fresh install has never been asked");
        assert!(should_show(false), "…and with the same two sources it would ask again");
        restore(&saved);
        assert!(ASKED.load(Relaxed));
        // THE NEXT BOOT, with the roster unchanged and the answer read back off disk: no question.
        assert!(!should_show(false), "a boot after the answer must go straight to Home");
        assert!(is_pinned("mid-mac-mini", 2, true));
        assert!(!is_pinned("mid-kate-nas", 3, false));

        // And the case that is 90% of installs: ONE source, never asked. The route must not exist
        // for them at all — not once, not ever — and their own library is on Home regardless,
        // which is what "goes straight to Home, as it does today" has to mean underneath.
        reset_sources();
        restore(&restore_to);
        if let Ok(mut v) = SOURCES.lock() {
            *v = vec![src("mac-mini", "", true, &[(1, "Movies", 412, false)])];
        }
        assert_eq!(source_count(), 1);
        assert!(!should_show(false), "a single-server install is never asked");
        assert!(is_pinned("mid-mac-mini", 1, true), "…and its library is on Home by default");

        // leave the globals as this test found them
        reset_sources();
        restore(&restore_to);
    }

    /// The fast path is ONE press: the ring starts on *Start watching*, the rows are one press
    /// right, and OK on a row flips that row's word rather than answering the screen.
    ///
    /// Drives the real `enter`/`update`/`key` against the screen's `static mut` list and cursor
    /// (the `ui::home` pattern), so it holds `testlock::serial()`. It deliberately never presses
    /// BACK or OK-on-the-action: both answer, and answering writes the session file — which a host
    /// suite must not do on somebody's machine. What those two commit is [`materialize`], which
    /// `skipping_records_the_honest_default` grades directly.
    #[test]
    fn focus_starts_on_the_action_and_the_rows_are_one_press_right() {
        let _serial = crate::testlock::serial();
        if let Ok(mut v) = SOURCES.lock() {
            *v = roster();
        }
        if let Ok(mut p) = PINS.lock() {
            p.clear();
        }
        enter();
        update(0.016);
        assert_eq!(focus(), Focus::Action, "one press answers");
        assert!(!table().focused, "…so the list marks its selection without claiming the ring");
        assert_eq!(table().n_rows(), 5, "every library across the three groups");

        key(SDLK_RIGHT, 0);
        assert_eq!(focus(), Focus::Rows);
        assert!(table().focused, "now the ring IS in the list");
        assert_eq!(table().sel, 0);

        assert!(!key(SDLK_RETURN, 0), "OK on a row is not an answer to the screen");
        assert!(!is_pinned("mid-mac-mini", 1, true), "…it flipped that row's switch");
        assert!(is_pinned("mid-mac-mini", 2, true), "…and only that one");

        key(SDLK_DOWN, 0);
        assert_eq!(table().sel, 1);
        key(SDLK_LEFT, 0);
        assert_eq!(focus(), Focus::Action, "LEFT comes back to the action");
        assert!(!key(SDLK_DOWN, 0), "and nothing on the action row answers by accident");

        reset_sources();
        restore(&crate::plex::session::Session::default());
    }

    /// One person sharing two servers is named once — the sentence is about people.
    #[test]
    fn a_person_who_shares_twice_is_named_once() {
        let v = vec![
            src("mine", "", true, &[(1, "Movies", 1, false)]),
            src("box-a", "bamx23", false, &[(1, "Films", 2, false)]),
            src("box-b", "bamx23", false, &[(1, "More Films", 3, false)]),
        ];
        assert_eq!(people(&v), vec!["bamx23".to_string()]);
        assert!(body_copy(&people(&v), 2).starts_with("bamx23 has shared libraries with you."));
    }
}
