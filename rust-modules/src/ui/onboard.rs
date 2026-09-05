//! **"Which libraries do you want?"** — the **Favorite libraries** route, `Shared Sources.dc.html`
//! deliverable F. First run asks it once, after the profile picker and before Home; Settings hosts
//! the same screen as the editor, titled with the setting's own name.
//!
//! One question: which of the libraries this account can reach are this television's favorites.
//! Your own arrive **On**; a friend's arrives **Off** only when you already have a library of that
//! TYPE — a shared library of a type you own none of arrives On, or answering the default would
//! take the whole type off the strip before the user ever saw it (`plex::pins::default_on`).
//!
//! **It governs the whole app, not Home alone, and that changed on 2026-09-05.** Favorites fill
//! Home's shelves, decide which type pills the top strip draws at all, and scope the Library's own
//! Sources picker. Nothing here grants or blocks ACCESS — the grant is plex.tv's and is untouched,
//! Search still reaches every granted library, and **this screen is the one surface that lists them
//! all** (`browse::all_source_rows`), which is what makes a non-favourite recoverable rather than
//! lost. That is also why skipping is a real answer rather than a deferral.
//!
//! ## It is a ROUTE, not a sheet
//!
//! The same standing as sign-in and the who's-watching picker: it has a beginning and an end and
//! never comes back. That is what the app's no-full-screen-sheets rule protects — a sheet claims
//! there is a screen behind it, and there is not one here — so the rows sit directly on the shared
//! frozen route ground with no panel, shadow or radius. The ground takes Home's UltraBlur envelope
//! once; it is atmosphere, not a second navigable screen behind this one.
//!
//! ## The list is A's, not a second expression of it
//!
//! [`crate::ui::source_list`] builds it, exactly as the Library toolbar's Source panel does — same
//! groups, same rows, same marks, same "The app needs one library" refusal. The two differences are
//! stated as arguments and not as a second builder: this one lists EVERY GRANTED library rather
//! than the browsed type's FAVOURITES (`browse::all_source_rows` — there is no tab bar here to be
//! scoped to, and a list that hid non-favourites could never turn one back on), and it
//! carries no *Check for new shares* row, because a share arriving later must not reopen a
//! first-run screen.
//!
//! ## BACK is navigation, never an answer
//!
//! `Start watching` is the only first-run commit, and Settings mode's `Done` is its exact twin —
//! between mount and one of those two, every toggle edits a local DRAFT (`DRAFT` below) and nothing
//! else. [`toggle_selected`] never calls `browse::toggle_pin`: that function both mutates the LIVE
//! section table and records it (`record_pins(true)` — "a profile that has flipped a switch has
//! plainly been asked"), so a screen that called it on every press had already answered the
//! first-run question the instant a first row was touched, whichever way BACK was then pressed.
//! [`commit`] is the only place `browse::apply_pins` is called, which is what makes BACK/Cancel free
//! to be a pure discard — there is nothing on disk to undo, because nothing was written until this
//! screen said so explicitly. Before a real library lands the action retries discovery and still
//! cannot persist an empty answer.
use crate::ui::consts::*;
use crate::ui::route_screen::{
    PressFrom, RouteFocus, RouteGround, RouteLayout, RouteShape, RouteStep,
};
use crate::ui::source_list::{self, Level, SrcAction, Tail};
use crate::ui::table::TableView;
use crate::ui::widgets::{Button, Spinner, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::os::raw::c_uint;
use std::ptr::{addr_of, addr_of_mut};

/// The heading, and the one action. Both are the design's words: *Start watching* rather than
/// *Continue*, "a verb that says what happens next rather than one that says nothing".
pub(crate) const TITLE: &str = "Which libraries do you want?";
const SETTINGS_TITLE: &str = "Favorite libraries";
const ACTION: &std::ffi::CStr = c"Start watching";
const DONE: &std::ffi::CStr = c"Done";
const RETRY: &std::ffi::CStr = c"Try again";
/// Where BACK goes, named on the crumb above the title rather than as a hint in the action band.
const CRUMB_SETTINGS: &str = "Settings";
const CRUMB_PROFILES: &str = crate::ui::profiles::TITLE;

/// Whether the shared bottom action row holds a control.
///
/// **This used to be a THREE-way choice, and the third arm was the BACK hint.** First run drew
/// `Start watching` beside `Press [BACK] to return`, and a clean Settings editor drew that hint
/// alone. Both are gone: where BACK goes is the crumb above the title now, so the band carries a
/// real action or nothing — see `ui::route_screen`'s module doc. What survives is the distinction
/// the enum was actually for: a pristine Settings editor has nothing to commit, while a dirty
/// one, first run, and a failed/empty load all do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BottomActions {
    None,
    Primary,
}

fn bottom_actions(settings: bool, changed: bool, no_sections: bool) -> BottomActions {
    if !settings || no_sections || changed {
        BottomActions::Primary
    } else {
        BottomActions::None
    }
}

/// **What the one action pill MEANS right now** — `Try again`, `Start watching` or `Done`.
///
/// It exists because the pill's meaning changes underneath a press that is already armed: first run
/// with nothing discovered shows `Try again`, and if the roster lands during the ~210 ms the tvOS
/// press takes to spring back, the SAME focus stop has become `Start watching` and the deferred
/// `on_ok` would commit and leave the ceremony instead of retrying. Focus location cannot see that
/// — `Focus::Action` is `Focus::Action` either way — so the press records this and refuses to
/// commit a different verb than the one that was pressed (Codex review, 2026-09-04).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ActionKind {
    Retry,
    Start,
    Done,
}

fn action_kind() -> ActionKind {
    if crate::browse::section_count() == 0 {
        ActionKind::Retry
    } else if settings_mode() {
        ActionKind::Done
    } else {
        ActionKind::Start
    }
}

impl ActionKind {
    fn label(self) -> &'static std::ffi::CStr {
        match self {
            ActionKind::Retry => RETRY,
            ActionKind::Done => DONE,
            ActionKind::Start => ACTION,
        }
    }
}

/// Where focus is. Two stops, and the list is ONE PRESS RIGHT of the action — which is the
/// design's fast path stated as a focus model: the defaults are already an answer, so the screen
/// opens on the way out of itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Action,
    List,
}

static mut TABLE: TableView = TableView::new();
static mut FOCUS: Focus = Focus::Action;
/// The section-table generation the rows were built from — sources and their libraries land
/// asynchronously, so the list fills in while the screen is up and must rebuild when it does.
static mut TABLE_GEN: u32 = u32::MAX;
/// One [`SrcAction`] per global row index, indexed exactly as the `TableView` reports.
static mut ACTS: Vec<SrcAction> = Vec::new();
/// Spinner phase accumulator — the list is a read-out until the roster answers, and a phase driven
/// off a CLOCK rather than a spring is invisible to `ui::idle::note_spring`. `Spinner::draw`
/// reports for itself (it shipped FROZEN before it did), so this only has to be advanced.
static mut PHASE_MS: f32 = 0.0;
/// The action pill's drawn frame, recorded at draw for the pointer — the `TOOL_RECTS` idiom. Parked
/// OFF the panel rather than at the origin, because `Rect::contains` is inclusive and a zero-size
/// rect at (0,0) would "contain" a click at exactly (0,0).
static mut ACTION_RECT: Rect = Rect::new(-1.0, -1.0, 0.0, 0.0);
static mut SETTINGS_MODE: bool = false;
/// [`dirty`]'s fixed anchor: "did the user change anything since this row was first seen" —
/// captured once, at [`enter_settings`] for a row already on the table, or at first sight in
/// [`draft_rows`] for one that lands later, and otherwise left alone by everything except a real
/// toggle (an untouched row's baseline still tracks the live default rather than going stale —
/// [`draft_rows`]'s own doc covers the self-heal, which moves this together with [`DRAFT`]).
/// (A second snapshot, `BASE`, mirrored the live table for `back_action`'s restore loop; both
/// went on 2026-09-04 — nothing is written before `commit`, so there is nothing to restore.)
static mut ENTRY: Vec<(usize, bool)> = Vec::new();
/// The editor's own copy of the pin set. Every [`toggle_selected`] mutates THIS — never
/// `browse::toggle_pin` — and [`commit`] is the only thing that ever applies it to the live table
/// (`browse::apply_pins`, one call for the whole session instead of one per press). Keyed exactly as
/// [`ENTRY`] is: `(section index, pinned)`. Seeded from the live pins at
/// [`enter`]/[`enter_settings`] and self-healed in [`draft_rows`] for a section whose worker lands
/// mid-edit.
static mut DRAFT: Vec<(usize, bool)> = Vec::new();
/// [`crate::browse::table_epoch`] at the moment [`DRAFT`]/[`ENTRY`] were last (re)seeded
/// from the live table — the guard against [`rebuild`] applying draft edits keyed by an INDEX the
/// table no longer agrees with. The epoch only moves on `browse::reset`, which runs on an ordinary
/// profile switch AND from inside `sync_roster`'s own every-frame maintenance whenever the live
/// roster has dropped a source this table still holds (a share revoked mid-session) — reachable
/// while this screen is open and pumping `discover_pump` every frame (Codex review, 2026-09-04:
/// without this, a mid-edit reset left every `(index, bool)` pair in the draft pointing at
/// whatever library happened to re-land at that same index, silently misapplying a toggle — or a
/// whole commit — to the wrong library).
static mut TABLE_EPOCH: u32 = 0;
static mut GROUND: RouteGround = RouteGround::new();

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

/// What an OK / click means for `app.rs`.
pub enum Action {
    None,
    /// the question is explicitly answered — continue through the first-run ceremony
    Done,
    /// first-run BACK — return to Who's Watching without recording an answer
    Back,
    /// Settings BACK — restore the captured draft and return to Settings
    Cancel,
}

/// Should this profile be asked at all? See `plex::pins::asks` for the two conditions.
pub fn asks() -> bool {
    crate::browse::first_run_asks()
}

/// The live pin set as the editor's own `(section, pinned)` pairs — the seed for [`DRAFT`] (and
/// [`ENTRY`], in Settings mode) at entry, and what [`draft_rows`] falls back to for a section [`DRAFT`] has not
/// seen yet.
fn snapshot_pins() -> Vec<(usize, bool)> {
    crate::browse::all_source_rows()
        .into_iter()
        .map(|r| (r.section, r.pinned))
        .collect()
}

/// Mount the route. Focus starts on the action, so the fast path is one press.
pub fn enter() {
    unsafe {
        SETTINGS_MODE = false;
        FOCUS = Focus::Action;
        TABLE_GEN = u32::MAX; // force the first build
        (*addr_of_mut!(GROUND)).reset();
        *addr_of_mut!(DRAFT) = snapshot_pins();
        TABLE_EPOCH = crate::browse::table_epoch();
    }
    table().list_focused = false;
    rebuild(false);
}

/// Reuse the same source picker as a Settings editor. BACK discards every toggle made since entry
/// (nothing was ever written — see the module doc); Done applies the draft in one step and returns
/// to the still-open Settings modal.
pub fn enter_settings() {
    unsafe {
        SETTINGS_MODE = true;
        FOCUS = Focus::List;
        TABLE_GEN = u32::MAX;
        let base = snapshot_pins();
        *addr_of_mut!(DRAFT) = base.clone();
        *addr_of_mut!(ENTRY) = base;
        TABLE_EPOCH = crate::browse::table_epoch();
    }
    table().list_focused = true;
    rebuild(false);
}

pub fn settings_mode() -> bool {
    unsafe { *addr_of!(SETTINGS_MODE) }
}
pub fn finish_settings() {
    unsafe { SETTINGS_MODE = false };
}
/// Whether this frame should present as the SETTINGS-hosted variant — every CONTENT choice
/// (crumb, title, action label and whether it is drawn at all, palette, whether the frozen
/// first-run ground draws), as opposed to the PAINTER question `home_editor_visible`/`child_push`
/// answer on their own. **Not simply `settings_mode()`**: that flag flips false the instant
/// Done/Cancel commits, on the SAME frame the exit animation starts (`app.rs`'s
/// `enter_home_from_onboard` calls `finish_settings` inline with the commit) — but `app.rs` keeps
/// calling [`draw`] for as long as `home_editor_visible()` says there is still something on
/// screen, so a caller reading the raw flag during that window shows the wrong copy. It bit
/// [`dirty`] as well as [`draw`]: BACK dismissing a PRISTINE editor (nothing toggled, so no action
/// row was ever drawn) used to pop `Start watching` into existence for the fade's own duration,
/// because `dirty`'s `!settings_mode()` guard flipped true the instant the flag did and forced
/// [`bottom_actions`] to draw a control that had never been on screen while the editor was
/// interactive. Device/sim-verified: both were exactly what the un-fixed function showed, over the
/// ghosted Settings root mid slide-out.
fn hosted() -> bool {
    settings_mode() || crate::ui::settings::home_editor_visible()
}
fn dirty() -> bool {
    if !hosted() {
        return true;
    }
    // Against the DRAFT, not the live table — a toggle no longer touches the live table at all
    // (see the module doc), so comparing against it would read every session as clean. Against
    // ENTRY: the fixed "what was true when this row was first seen" snapshot, which is exactly
    // what this comparison needs.
    let entry = unsafe { addr_of!(ENTRY).as_ref().cloned().unwrap_or_default() };
    let draft = unsafe { addr_of!(DRAFT).as_ref().cloned().unwrap_or_default() };
    entry.iter().any(|(section, was)| {
        draft
            .iter()
            .find(|(s, _)| s == section)
            .is_some_and(|(_, now)| now != was)
    })
}

/// (Re)build the rows from the section table. `keep` holds the cursor and glides the scroll — a
/// source landing mid-screen must not move the row under the user's thumb.
fn rebuild(keep: bool) {
    reseed_if_table_identity_changed();
    let gen = crate::browse::source_list_gen();
    let groups = crate::browse::source_groups();
    let rows = draft_rows();
    // **On Home and nothing else.** There is no library being browsed here, so the picker level
    // has nothing to point its tick at — the question this screen asks is what is SET, which is
    // the level that answers in words.
    let (secs, acts) = source_list::sections(Level::OnHome, &groups, &rows, Tail::None);
    let sel = if keep { table().sel } else { 0 };
    unsafe {
        TABLE_GEN = gen;
        *addr_of_mut!(ACTS) = acts;
    }
    table().compact = false; // two-line rows: HEADLINE titles over CAPTION sub-lines
    table().set_sections(secs, sel, keep);
    crate::ui::idle::invalidate();
}

/// Guard against [`DRAFT`]/[`ENTRY`] outliving the table INDICES they were seeded from.
/// `browse::reset` (see [`TABLE_EPOCH`]'s doc for when it can fire while this screen is open) makes
/// every existing section index mean a different library, or none — so a draft captured before it
/// is not stale data to patch up, it is answering a question about a table that no longer exists.
/// The only sound response is the one [`enter`]/[`enter_settings`] already give a fresh mount:
/// reseed both from whatever the table holds now. A toggle in flight when this fires is lost,
/// not misapplied — the alternative this replaced.
fn reseed_if_table_identity_changed() {
    let epoch = crate::browse::table_epoch();
    unsafe {
        if addr_of!(TABLE_EPOCH).read() == epoch {
            return;
        }
        TABLE_EPOCH = epoch;
        let fresh = snapshot_pins();
        *addr_of_mut!(DRAFT) = fresh.clone();
        if settings_mode() {
            *addr_of_mut!(ENTRY) = fresh;
        }
    }
}

/// [`crate::browse::all_source_rows`], with every row's `pinned`/`last_pinned` overridden by
/// [`DRAFT`] — what every draw and every row decision on this screen actually reads, so that
/// nothing here ever consults the live table's own pin state while an edit is in progress.
///
/// Self-heals [`DRAFT`] for a row it has not seen: a source's libraries land asynchronously (see
/// [`update`]), so a rebuild mid-edit can meet a section this profile has never been asked about.
/// It enters the draft at that row's own live default (what the table would show if the editor had
/// never opened) rather than at `false`, which would silently turn every not-yet-toggled library
/// off the moment any other row was touched.
///
/// **In Settings mode, also rides an UNTOUCHED row's live drift into [`ENTRY`]** (together with
/// `DRAFT` itself, so what's shown keeps tracking the live default until the user actually
/// presses something). A row nobody has toggled yet is not a decision, only a default —
/// `browse::resolve_pins` re-derives every still-unrecorded row's default from the WHOLE roster's
/// ownership mix every time a new source lands, so a friend's library shown "On" at entry (no
/// server of your own known yet) can resolve to "Off" a moment later once your own server
/// answers, with nobody having pressed anything. A row the user HAS toggled is left alone (its
/// `ENTRY` stays exactly what it was before the user's first touch), so [`dirty`] never silently
/// reabsorbs a real edit as a new baseline.
fn draft_rows() -> Vec<crate::browse::SrcRow> {
    let mut rows = crate::browse::all_source_rows();
    let settings = settings_mode();
    unsafe {
        let draft = &mut *addr_of_mut!(DRAFT);
        let entry = &mut *addr_of_mut!(ENTRY);
        for r in &rows {
            match draft.iter().position(|(s, _)| *s == r.section) {
                None => {
                    draft.push((r.section, r.pinned));
                    if settings {
                        entry.push((r.section, r.pinned));
                    }
                }
                Some(di) if settings => {
                    if let Some(ei) = entry.iter().position(|(s, _)| *s == r.section) {
                        if entry[ei].1 == draft[di].1 && entry[ei].1 != r.pinned {
                            entry[ei].1 = r.pinned;
                            draft[di].1 = r.pinned;
                        }
                    }
                }
                Some(_) => {}
            }
        }
        // The never-empty floor, read off the DRAFT's own count — the whole-roster fact
        // `browse::rows_where` computes from the live table, re-derived here because while this
        // screen is open the two can disagree by every toggle made so far.
        let last = draft.iter().filter(|(_, on)| *on).count() == 1;
        for r in rows.iter_mut() {
            if let Some(&(_, on)) = draft.iter().find(|(s, _)| *s == r.section) {
                r.pinned = on;
                r.last_pinned = last && on;
            }
        }
    }
    rows
}

/// The same sectioned-content frame used by Settings and Legal.  Source groups always have a
/// server label, so its cap-top shares the route title's anchor rather than inheriting a second
/// screen-local top guide.
fn list_frame() -> Rect {
    RouteLayout::screen().sectioned_table()
}

/// The action pill's press surface ([`crate::ui::route_screen::ActionRow`], the shared type every
/// route-family action row now owns rather than a private `CtlPop`) — focus walks between it and
/// the library list beside it, so it animates both ways.
static mut ACTION_POP: crate::ui::route_screen::ActionRow<1> =
    crate::ui::route_screen::ActionRow::new();

pub fn update(dt: f32) {
    // The roster half of the pump, and only that half: sources, their sections, their machine
    // names and their library counts all land on workers that `browse::pump` schedules — and that
    // runs from the Library screen alone, so without this a share discovered after boot would
    // never reach the one screen whose entire job is to list it. `discover_pump` rather than
    // `pump` because this screen wants no items: paging a grid nobody is looking at would be a
    // page of requests per library for a list of NAMES.
    unsafe { PHASE_MS += dt * 1000.0 };
    crate::browse::discover_pump();
    // …and the generation watched is `source_list_gen`, not the table's SHAPE: a library's count
    // and a server's own name land without adding a row, and a screen keyed on the shape alone
    // sits there reading "Films" under a group with no header.
    if unsafe { addr_of!(TABLE_GEN).read() } != crate::browse::source_list_gen() {
        rebuild(true);
    }
    // **Rule 10 on the frame clock, and AFTER the rebuild above rather than before it.** The roster
    // lands on a worker, so this screen's shape changes with no key pressed — an editor that opened
    // on an empty list must grow its `Try again` ring without waiting for one. Settling ahead of
    // `discover_pump`/`rebuild` left the newly rebuilt shape unsettled for the frame that was then
    // DRAWN (Codex review, 2026-09-04).
    let focus = settled_focus();
    unsafe {
        (*addr_of_mut!(ACTION_POP)).step((focus == Focus::Action).then_some(0), dt);
    }
    table().update(dt, list_frame().h);
}

pub fn draw() {
    // Every CONTENT choice below reads `hosted()`, never `settings_mode()` directly — see that
    // function's doc for why the two disagree during the exit fade.
    let hosted = hosted();
    // A first-run route is not a sheet, but it belongs to the same visual family as Settings. Its
    // frozen UltraBlur envelope is drawn once and never follows focus or child content.
    if !hosted {
        unsafe { (*addr_of_mut!(GROUND)).draw_home(Painter::root()) };
    }
    // Settings-hosted mode draws through its OWN `RoutePush` (`settings::HOME_PUSH`), never a
    // bare `Painter::root()` — the missing half of that push was exactly why this screen used to
    // mount at full opacity on its very first frame instead of sliding in. NOT `settings::CHILD`,
    // the root's own push: the two used to be one field, and reusing it here meant Privacy/Legal
    // opening also satisfied "the Home editor has something to draw" (Codex review, 2026-09-04 —
    // see `settings::HOME_PUSH`'s own doc for the split and why each still needs the other's
    // current state on a mid-flight handoff, `RoutePush::sync_to`). Gated on the push's own AMOUNT
    // (`home_editor_visible`, folded into `hosted` above) rather than on the raw flag, for the
    // same reason `hosted` is — the amount stays positive for exactly as long as there is still
    // something to draw, in both directions (mirrors `consent.rs`'s `stage_visible`/`draw_stage`
    // pair). A standalone first-run boot
    // never opens Settings, so `settings::CHILD` never leaves zero and this is always a bare
    // `Painter::root()` there.
    let p = if crate::ui::settings::home_editor_visible() {
        crate::ui::settings::child_push(Painter::root())
    } else {
        Painter::root()
    };
    let env = Env::inert();

    let layout = RouteLayout::screen();
    let body = body_copy();
    layout.draw_narrative(
        p,
        Some(if hosted { CRUMB_SETTINGS } else { CRUMB_PROFILES }),
        if hosted { SETTINGS_TITLE } else { TITLE },
        &body,
        theme::size::LABEL,
    );

    // The DRAW reads `hosted` (the push's amount), not `action_kind()`'s `settings_mode()` (the
    // logical flag): the flag flips the instant Done/Cancel commits, and the exit fade would show
    // the other mode's copy for its last frames. Keys act on the logical state; pixels follow the
    // push (`hosted`'s doc).
    let action = if crate::browse::section_count() == 0 {
        ActionKind::Retry
    } else if hosted {
        ActionKind::Done
    } else {
        ActionKind::Start
    }
    .label();
    let actions = bottom_actions(hosted, dirty(), crate::browse::section_count() == 0);
    if actions != BottomActions::None {
        let w = Button::pill_w(action.as_ptr(), theme::size::BODY, false).min(layout.action.w);
        let r = Rect::new(layout.action.x, layout.action.y, w, layout.action.h);
        unsafe { ACTION_RECT = r };
        Button::new(action.as_ptr(), theme::size::BODY, r)
            .focused(unsafe { addr_of!(FOCUS).read() } == Focus::Action)
            .scale(unsafe { addr_of!(ACTION_POP).as_ref().unwrap().scale(0) })
            .palette(if hosted {
                crate::ui::settings::control_palette()
            } else {
                unsafe { (*addr_of!(GROUND)).palette() }
            })
            .draw(&env, p);
    } else {
        // Nothing to commit, so the band is EMPTY — the crumb above the title already says where
        // BACK goes, and a control that is not drawn must not be hit-testable either.
        unsafe { ACTION_RECT = Rect::new(-1.0, -1.0, 0.0, 0.0) };
    }

    // ---- right column: the list, on the ground ----
    let lf = list_frame();
    if table().n_rows() == 0 {
        // Nothing has answered YET — sections land on a worker, so the first frames of this route
        // have an empty roster. A spinner, not the table: `TableView` draws its own "No tracks"
        // when it is empty, which on this screen would state the opposite of the truth (there ARE
        // libraries; nobody has listed them yet) on the one screen whose whole subject is that list.
        if crate::browse::discovery_state() == crate::browse::SecFetch::Failed {
            StatusOverlay::new(lf, c"Couldn't load libraries", StatusKind::Failed)
                .reason(c"Check the connection, then try again.")
                .draw(&env, p);
        } else {
            Spinner::new(lf.x + lf.w * 0.5, lf.y + StatusOverlay::CTRL_H, 22.0)
                .phase(unsafe { addr_of!(PHASE_MS).read() } as u32)
                .tint(theme::TEXT_TERTIARY)
                .draw(&env, p);
        }
        return;
    }
    table().draw(p, lf);
}

/// The body paragraph, which names the PEOPLE — "because that is what a user recognises". Machine
/// names stay in the group headers, where they identify which box a library sits on; this is the
/// app's own "people in content, machines in settings" rule applied to a sentence.
///
/// Built from the roster rather than written out, so it says who has actually shared with you.
fn body_copy() -> String {
    let who: Vec<String> = crate::browse::source_groups()
        .iter()
        .filter(|g| !g.handle.is_empty())
        .map(|g| g.handle.clone())
        .collect();
    body_copy_for(&who)
}

fn body_copy_for(who: &[String]) -> String {
    // **The tail no longer promises the Library chip**, and that is a correction rather than a
    // retune: it read "you can browse any of them from the Library chip whenever you like" while
    // the switch governed Home alone. It governs the whole app now — a library that is not a
    // favourite is not offered by the Library's own Sources picker either — so the old sentence
    // would be telling the user something the product stopped doing on the same commit. Settings
    // remains the one unscoped list, which is what the second sentence points at.
    let tail = " Pick your favorites \u{2014} they are what Home and the Library show, and you \
                can change them in Settings whenever you like.";
    match join_names(who) {
        // No owner handle says NOTHING about server count. The common one-server/two-library case
        // lands here too, so the fallback asks the screen's actual question without inventing a
        // second server or a person who shared it.
        None => "Choose the libraries this television shows. Your favorites fill Home's shelves \
                 and the Library's own tabs; Settings lists every one you have."
            .to_string(),
        Some(names) => {
            let verb = if who.len() == 1 { "has" } else { "have" };
            format!("{names} {verb} shared libraries with you.{tail}")
        }
    }
}

/// `a`, `a and b`, `a, b and c` — the one list-of-people formatter this screen needs. Pure, so the
/// comma-and-conjunction rule is graded on the host rather than eyeballed on a television.
fn join_names(who: &[String]) -> Option<String> {
    match who {
        [] => None,
        [a] => Some(a.clone()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
}

/// Commit the answer and leave once a real library exists. Before then both entry points retry.
/// the skip is honest precisely because it records what the screen was showing rather than
/// deferring the question to a prompt that never comes.
///
/// The ONE place [`DRAFT`] ever reaches `browse`'s live pins — through `apply_pins`, one call for
/// however many rows were toggled this session, rather than the one-write-per-press `toggle_pin`
/// would have cost.
fn commit() -> Action {
    if crate::browse::section_count() == 0 {
        crate::browse::retry_discovery();
        crate::log("onboard: no discovered libraries yet — retry queued");
        return Action::None;
    }
    let draft = unsafe { addr_of!(DRAFT).as_ref().cloned().unwrap_or_default() };
    crate::browse::apply_pins(&draft);
    crate::log(&format!(
        "onboard: Home selection recorded — {} of {} libraries on",
        crate::browse::pinned_count(),
        crate::browse::section_count()
    ));
    Action::Done
}

/// The focused stop is the action PILL rather than the list — this screen's one control face, and
/// the only thing on it that takes the tvOS press (`ui::press`). The pill's dip arrives through
/// `ACTION_POP`; a `TableView` row has no `CtlPop` and is not a control face, so OK there keeps
/// flipping the pin on the key-down.
pub fn focus_is_ctl() -> bool {
    // Settled, and therefore also conditional on the action EXISTING: `set_focus` cannot leave
    // focus on a band that is not drawn, so an armed `press::begin_ctl` always has a control face
    // under it.
    settled_focus() == Focus::Action
}

/// Activate the focused stop — the whole of what OK means here, called on the key-down for the list
/// and on the press spring-back for the pill ([`focus_is_ctl`]). Split out of [`key`] so the two
/// timings run ONE activation rather than two that agree by inspection.
pub fn on_ok() -> Action {
    // **An armed press is consumed BEFORE the branch, not inside the Action arm.** The band can go
    // away UNDER a press: arm `Try again` on an empty Settings editor, let the roster land during
    // the spring-back, and rule 10 correctly settles focus onto the newly arrived list — at which
    // point a check inside the Action arm never runs and the deferred commit toggles the first
    // library instead of retrying discovery (Codex review, 2026-09-04).
    let armed = unsafe { addr_of_mut!(ARMED).replace(None) };
    if let Some((_, kind)) = armed.filter(|_| crate::ui::press::is_live()) {
        if settled_focus() != Focus::Action || kind != action_kind() {
            crate::log("onboard: the action changed under an armed press — refusing to commit it");
            return Action::None;
        }
        return commit();
    }
    match settled_focus() {
        Focus::Action => commit(),
        Focus::List => {
            toggle_selected();
            Action::None
        }
    }
}

/// BACK: leave without recording an answer, in either mode. Under the draft model nothing has been
/// WRITTEN before [`commit`] — a toggle touches only [`DRAFT`], which the next open re-snapshots —
/// so there is nothing to restore; this once walked a `BASE` snapshot against the live table through
/// `browse::toggle_pin` to undo per-press writes that no longer happen. Split out of [`key`]
/// because rule 9 gives LEFT the same job on a screen with no action band (a clean Settings editor
/// has none — there is nothing to commit — so LEFT there cannot lose an edit, and on a dirty one
/// the band is what LEFT reaches instead).
fn back_action() -> Action {
    if settings_mode() {
        return Action::Cancel;
    }
    Action::Back
}

/// This route's shape, as `ui::route_screen`'s shared rules see it. Its band is the ONE action
/// pill, present exactly when [`bottom_actions`] draws one; `opens` is `false` because a source row
/// is a switch rather than a door, so RIGHT on one does nothing (rule 8).
fn shape() -> RouteShape {
    let band = usize::from(
        bottom_actions(
            settings_mode(),
            dirty(),
            crate::browse::section_count() == 0,
        ) != BottomActions::None,
    );
    let t = table();
    RouteShape {
        band,
        rows: t.n_rows() > 0,
        at_last_row: t.at_last_row(),
        opens: false,
        // Rule 9's guard, stated rather than inferred from the band — and it is the SETTINGS
        // editor's alone. First run is a QUESTION, not an edit of stored state: nothing has been
        // recorded yet, its toggles live in the draft until `Start watching`, and BACK there
        // (`back_action`) walks to the picker recording nothing — exactly as consent's first run
        // records nothing on its own crumb. So LEFT follows the `‹ Who's watching` crumb; what it
        // leaves behind is an unanswered question, which rule 9 does not protect. (Until the draft
        // model this comment said the rows had "already persisted through `toggle_pin`", which was
        // true then and is not now.) This read `t.n_rows() > 0 && (!settings_mode() || dirty())` for one
        // commit, which made a first-run editor's LEFT change meaning the moment its roster
        // landed — a wall for no reason a person could see (Codex review, 2026-09-04).
        //
        // This once carried `|| !base_covers_every_row()` — a refusal to trust a baseline that a
        // Settings editor opened before discovery answered had captured nothing of, so that a row
        // landing later and then toggled (which persisted at once through `browse::toggle_pin`)
        // could not be walked out of by LEFT. Under the draft model that case no longer exists: a
        // toggle changes only `DRAFT`, `ENTRY` absorbs a row as it lands, and `dirty()` therefore
        // reads the first toggle of a landed row as the edit it is
        // (`a_roster_that_lands_after_the_editor_opened_is_uncommitted_only_once_edited`).
        uncommitted: settings_mode() && dirty(),
    }
}

pub fn key(sym: c_uint, wcode: c_uint) -> Action {
    if is_back(sym, wcode) {
        return back_action();
    }
    if is_ok(sym) {
        return on_ok();
    }
    // Navigation is `ui::route_screen`'s shared model, not this screen's own reading of it — which
    // is what gave it DOWN off the last library into the action pill (the pill was reachable only
    // with LEFT before) without giving it a second answer to what LEFT and RIGHT mean.
    let mut f = if settled_focus() == Focus::Action {
        RouteFocus::band()
    } else {
        RouteFocus::content()
    };
    let s = shape();
    let step = match sym {
        SDLK_LEFT => f.left(s),
        SDLK_RIGHT => f.right(s),
        SDLK_UP => f.updown(s, -1),
        SDLK_DOWN => f.updown(s, 1),
        _ => return Action::None,
    };
    // **Write back on EVERY step, not only on `Moved`.** Every rule re-settles before it decides
    // (rule 10), and a settling transition commonly reports `Wall` or `Scroll` — so persisting only
    // the explicit moves left this screen's `FOCUS` saying `List` while the model had already
    // moved to the band. That is not cosmetic: a Settings editor whose discovery found nothing has
    // an empty list and a `Try again` pill, every direction is a wall, and `Try again` was
    // reachable by neither key nor pointer.
    set_focus(if f.on_content() {
        Focus::List
    } else {
        Focus::Action
    });
    match step {
        RouteStep::Wall => return Action::None,
        RouteStep::Moved => {}
        RouteStep::Scroll(delta) => table().move_sel(delta),
        RouteStep::Enter => return on_ok(),
        RouteStep::Back => return back_action(),
    }
    crate::ui::idle::invalidate();
    Action::None
}

/// The stored focus, re-settled against the shape the screen actually has right now (rule 10).
///
/// Every input path goes through this rather than reading `FOCUS` raw, because the shape moves on a
/// WORKER: the roster lands asynchronously, so a screen that opened on an empty list has a band and
/// no rows, and one that opened on the band gains rows a second later. Without it, `focus_is_ctl`
/// and `on_ok` could disagree with the rules about where the ring is.
fn settled_focus() -> Focus {
    let mut f = if unsafe { addr_of!(FOCUS).read() } == Focus::Action {
        RouteFocus::band()
    } else {
        RouteFocus::content()
    };
    if f.settle(shape()) {
        set_focus(if f.on_content() {
            Focus::List
        } else {
            Focus::Action
        });
    }
    unsafe { addr_of!(FOCUS).read() }
}

fn set_focus(f: Focus) {
    // Rule 10, this screen's spelling: focus cannot enter a list with no rows (the roster lands on
    // a worker, so the first frames of this route have nothing to select), and it cannot sit on an
    // action that is not drawn.
    //
    // The second guard used to read `settings_mode() && !dirty()` — "a clean Settings editor has
    // no Done, so send focus to the list". That is true only when the list HAS rows: when
    // discovery found nothing the band holds `Try again` and the list is empty, so the redirect
    // sent focus to nothing and made the one control on screen unreachable by pointer as well as
    // by key. Asking whether the band exists is the same question without the false case.
    let has_band = bottom_actions(
        settings_mode(),
        dirty(),
        crate::browse::section_count() == 0,
    ) != BottomActions::None;
    let f = if f == Focus::Action && !has_band {
        Focus::List
    } else if f == Focus::List && table().n_rows() == 0 && has_band {
        Focus::Action
    } else {
        f
    };
    unsafe { FOCUS = f };
    // The two are ONE decision (`TableView::list_focused` gates the ink as well as the pill), so
    // they are written together — a list that keeps its selection pill while the action is focused
    // puts two accent capsules on screen and they are indistinguishable.
    table().list_focused = f == Focus::List;
}

/// Flip the focused row's pin **in the draft** — never `browse::toggle_pin`, which both mutates the
/// live table and persists it (see the module doc). A refusal (the last library on Home, judged
/// against the DRAFT's own count so it tracks every toggle made so far this session rather than
/// only what is on disk) changes nothing and says so on the row itself, which is already drawn — so
/// there is nothing to do here but rebuild either way.
fn toggle_selected() {
    let sel = table().sel;
    let acts: &[SrcAction] = unsafe { &*addr_of!(ACTS) };
    let act = usize::try_from(sel).ok().and_then(|i| acts.get(i)).copied();
    if let Some(SrcAction::Library(s)) = act {
        toggle_draft(s);
        // The words on every row can move, not just this one: turning a second library on releases
        // the "The app needs one library" refusal that was dimming another.
        rebuild(true);
    }
}

/// [`toggle_selected`]'s mutation, isolated so the never-empty-draft rule reads as one sentence.
/// [`draft_rows`] has always run at least once by the time this is reachable (a `Focus::List` OK
/// needs a focused row, and a row needs [`rebuild`] to have built one), so `section` is expected to
/// already be in [`DRAFT`]; the early return is defensive rather than a path anything is known to
/// take.
fn toggle_draft(section: usize) {
    unsafe {
        let draft = &mut *addr_of_mut!(DRAFT);
        let Some(idx) = draft.iter().position(|(s, _)| *s == section) else {
            return;
        };
        let on = draft[idx].1;
        let last_pinned = on && draft.iter().filter(|(_, v)| *v).count() == 1;
        if last_pinned {
            return;
        }
        draft[idx].1 = !on;
    }
}

/// Park focus under the pointer. **Reports whether it parked on anything**, which is what makes it
/// safe for [`press_at`] to build on: a click in dead space parks nothing and must not arm a press
/// on whatever happened to be focused already.
pub fn pointer_focus(mx: f32, my: f32) -> bool {
    if unsafe { addr_of!(ACTION_RECT).read() }.contains(mx, my) {
        set_focus(Focus::Action);
        crate::ui::idle::invalidate();
        return true;
    }
    if let Some(r) = table().hit_row(list_frame(), mx, my) {
        set_focus(Focus::List);
        table().sel = r;
        crate::ui::idle::invalidate();
        return true;
    }
    false
}

pub fn click(mx: f32, my: f32) {
    if let Some(r) = table().hit_row(list_frame(), mx, my) {
        set_focus(Focus::List);
        table().sel = r;
        toggle_selected();
    }
}

/// Pointer-down on the action PILL: park focus on it and report the hit, so the caller can arm the
/// tvOS press and spend it on the spring-back — the pointer's half of [`focus_is_ctl`]. A list row
/// is not a control face, so it is [`click`]'s and still flips its pin on the button-down.
///
/// **Through [`pointer_focus`], not beside it.** This began as a copy of that function's first
/// branch and dropped its `idle::invalidate()` in the copying — [`set_focus`] does not invalidate
/// on its own, which is exactly why `pointer_focus` calls it explicitly, so the focus moved to the
/// pill on the button-down with no repaint owed. It survived only because the press spring that the
/// caller arms a moment later reports motion of its own; a press that failed to arm would have
/// moved the ring invisibly.
pub fn press_at(mx: f32, my: f32) -> bool {
    let ok = pointer_focus(mx, my) && focus_is_ctl();
    if ok {
        arm_action_from(PressFrom::Pointer);
    }
    ok
}

/// What the in-flight press was armed on — its VERB and where the press came from.
///
/// **Read through [`armed`], never raw**, so an arm cannot outlive the crate-global press it
/// belongs to: many things cancel that press (a nav key, a fresh click, the lost-key-up ceiling)
/// and none of them knows this static exists. The expiry is `press::is_live` rather than
/// `is_active` — a cancelled press stays ACTIVE for its bounce, and a list row's OK is immediate,
/// so the row's activation was being judged against the pill the user had already left.
static mut ARMED: Option<(PressFrom, ActionKind)> = None;

fn armed() -> Option<(PressFrom, ActionKind)> {
    crate::ui::press::is_live()
        .then(|| unsafe { *addr_of!(ARMED) })
        .flatten()
}

/// Record what a press is being armed on, and from where. Called on the OK key-down (`app.rs`'s
/// `key_onboarding`) and on the pointer-down ([`press_at`]), immediately before `press::begin_ctl`.
///
/// **The verb, not the location.** `Focus::Action` is `Focus::Action` whether the pill says
/// `Try again` or `Start watching`, and the roster landing during the ~210 ms spring-back turns
/// one into the other — so a press meant to retry discovery would commit the first-run answer and
/// leave the ceremony instead (Codex review, 2026-09-04).
pub fn arm_action() {
    arm_action_from(PressFrom::Key);
}

fn arm_action_from(from: PressFrom) {
    unsafe { ARMED = Some((from, action_kind())) };
}

/// Whether an armed press is STILL on the thing it was armed on — `app.rs` cancels when it is not.
/// Parks focus on the way past, so this is also the ordinary hover path.
///
/// The two origins are judged differently ([`PressFrom`]): a pointer-origin press is bound to the
/// pill's own rect, so sliding off it — dead space included — retracts the click; a key-origin
/// press is bound to the focus stop, so hover across dead space, which moves no focus, leaves a key
/// the user is still holding alone.
pub fn pointer_hold(mx: f32, my: f32) -> bool {
    let on_action = unsafe { addr_of!(ACTION_RECT).read() }.contains(mx, my);
    pointer_focus(mx, my);
    match armed() {
        Some((PressFrom::Pointer, kind)) => on_action && kind == action_kind(),
        Some((PressFrom::Key, kind)) => {
            settled_focus() == Focus::Action && kind == action_kind()
        }
        None => true,
    }
}

/// The focus probe's read of this screen — see `focusprobe`'s doc on why every screen owes one.
pub(crate) fn probe_fields() -> (bool, i32) {
    (
        unsafe { addr_of!(FOCUS).read() } == Focus::List,
        table().sel,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_and_back_cannot_answer_before_a_real_library_lands() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        assert!(matches!(commit(), Action::None));
        assert!(
            matches!(key(SDLK_ESCAPE, 0), Action::Back),
            "BACK navigates without recording an empty answer"
        );
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn start_cannot_answer_before_a_real_library_lands_and_back_never_answers() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        assert!(matches!(commit(), Action::None));
        assert!(matches!(key(SDLK_ESCAPE, 0), Action::Back));
    }

    /// **This screen's teardown, wrapped around the shared session guard.** The redirect itself is
    /// `plex::session::TempSession` — which this comment used to CLAIM already existed while
    /// duplicating it here, one of three copies. What is local is the second half of `Drop`: the
    /// two globals this screen owns and must put back. A test using it must hold
    /// [`crate::testlock::serial`] for its whole body, exactly as the inner guard requires.
    struct TempSession {
        _inner: crate::plex::session::TempSession,
    }
    impl TempSession {
        fn new(tag: &str) -> TempSession {
            let inner = crate::plex::session::TempSession::new(tag);
            inner.watching("u-test");
            TempSession { _inner: inner }
        }
    }
    /// **Cleanup lives here, not at the tail of each test, because a Drop guard is the only thing
    /// that still runs when an `assert!` PANICS partway through** (Codex review, 2026-09-04: the
    /// first version of these tests reset `SETTINGS_MODE`/`browse`'s table only on the success
    /// path — a failing assertion in one test left `SETTINGS_MODE` at `true` for whichever test the
    /// harness ran next, which is exactly the cross-test contamination this repo has hit before).
    /// `testlock::serial()` only stops two such tests running AT ONCE; it says nothing about what
    /// one leaves behind after it fails.
    impl Drop for TempSession {
        fn drop(&mut self) {
            finish_settings();
            crate::browse::reset();
            // the inner guard's own Drop runs after this and takes the redirect back
        }
    }

    /// **Issue 9 reproduction, first run.** Before the draft model, [`toggle_selected`] called
    /// `browse::toggle_pin` directly — which both flips the LIVE pin and records it
    /// (`record_pins(true)`, "a profile that has flipped a switch has plainly been asked") — so a
    /// toggle made on this screen was already on disk whichever way BACK was then pressed, and the
    /// profile's next boot never saw this screen again. Historically red: making [`toggle_draft`]
    /// call `crate::browse::toggle_pin(s)` instead of mutating [`DRAFT`] (the exact pre-fix shape)
    /// turns every assertion below red, because the live pin and the recorded answer both move on
    /// the first toggle instead of only at [`commit`].
    #[test]
    fn toggling_never_touches_the_live_pin_or_the_recorded_answer_until_commit() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("draft");
        crate::browse::seed_pins_for_test(&[true, true]);
        enter();

        table().sel = 0;
        toggle_selected();
        assert!(
            !draft_rows()[0].pinned,
            "the draft reflects the toggle immediately — the screen must show it"
        );
        assert!(
            crate::browse::pinned(0),
            "…but the LIVE pin has not moved: nothing is written until commit"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and nothing has been recorded either — BACK has nothing to undo"
        );

        // BACK/Cancel is a pure discard under this model. A fresh `enter` (what a real BACK leaves
        // the next entry looking at) proves the live pin came through untouched rather than merely
        // unread by this test.
        enter();
        assert!(
            crate::browse::pinned(0),
            "a discarded draft leaves the live pin exactly where BACK found it"
        );

        // Toggling and THEN committing is what actually writes it down.
        table().sel = 0;
        toggle_selected();
        assert!(matches!(commit(), Action::Done));
        assert!(
            !crate::browse::pinned(0),
            "commit applies the draft to the live table"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_some_and(|r| r.asked),
            "…and records it — this is what the recorded answer is FOR"
        );
    }

    /// The Settings-hosted twin: `Done` is `commit`'s other name, and BACK there is [`Action::Cancel`]
    /// rather than [`Action::Back`], but the mechanism is the same draft — [`dirty`] tracks it too,
    /// not the live table, or a toggle-then-BACK session would show Done as pressable when there is
    /// nothing left to commit.
    #[test]
    fn settings_mode_dirty_and_persistence_track_the_draft_not_the_live_table() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("settings-draft");
        crate::browse::seed_pins_for_test(&[true, true]);
        enter_settings();
        assert!(!dirty(), "nothing has been touched yet");

        table().sel = 0;
        toggle_selected();
        assert!(dirty(), "the draft moved, so Done has something to commit");
        assert!(
            crate::browse::pinned(0),
            "Settings mode's toggle is a draft edit too — the live table still has not moved"
        );

        assert!(matches!(commit(), Action::Done));
        assert!(!crate::browse::pinned(0), "Done applies the draft");
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_some_and(|r| r.asked)
        );
        // `TempSession`'s Drop resets `SETTINGS_MODE`/`browse`'s table unconditionally — see its
        // own doc for why that has to be a Drop guard rather than a call here.
    }

    /// **Codex review finding, round 1 (2026-09-04).** [`dirty`] compares `ENTRY` against `DRAFT`.
    /// Before this test existed, an UNTOUCHED row's live pin could drift out from under `ENTRY`
    /// purely from `browse::resolve_pins` re-deriving its still-unrecorded default as a second
    /// source lands (see `draft_rows`'s doc), which made a row nobody had pressed anything on
    /// read as "dirty" the moment the world changed around it. [`draft_rows`] rides such drift
    /// into `ENTRY` for any row the DRAFT still agrees with (i.e. the user never touched), so
    /// `dirty()` stays false.
    #[test]
    fn an_untouched_rows_live_drift_is_absorbed_into_entry_not_read_as_an_edit() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("entry-drift");
        crate::browse::seed_pins_for_test(&[true, true]);
        enter_settings();
        let entry_before = unsafe { addr_of!(ENTRY).as_ref().cloned().unwrap_or_default() };
        assert_eq!(entry_before, vec![(0, true), (1, true)]);

        // Nobody touched anything — this is `resolve_pins` re-deriving an unrecorded default as a
        // second source lands, not a user press. `set_pinned_for_test` mutates the live table with
        // none of `toggle_pin`'s side effects, standing in for that.
        crate::browse::set_pinned_for_test(1, false);
        rebuild(true);

        let entry_after = unsafe { addr_of!(ENTRY).as_ref().cloned().unwrap_or_default() };
        assert_eq!(
            entry_after,
            vec![(0, true), (1, false)],
            "ENTRY rode the untouched row's drift, so dirty() reads no difference from DRAFT"
        );
        assert!(
            !dirty(),
            "an untouched row's drift is not something Done should offer to commit either"
        );
    }

    /// **Codex review finding, 2026-09-04.** `browse::reset()` (a profile switch, but ALSO ordinary
    /// `sync_roster` maintenance pumped every frame this screen is open, whenever the live roster
    /// has dropped a source the table still holds — see `TABLE_EPOCH`'s doc) makes every section
    /// INDEX mean a different library, or none. A `DRAFT`/`ENTRY` built from the old indices
    /// would misapply — [`commit`] writing a decision about "index 0" into whatever library now
    /// happens to sit at index 0. Historically red: the assertions below fail if
    /// `reseed_if_table_identity_changed`'s call is removed from [`rebuild`].
    #[test]
    fn a_table_reset_mid_edit_discards_the_stale_draft_instead_of_misapplying_it() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("epoch-reset");
        crate::browse::seed_pins_for_test(&[true, true]);
        enter_settings();
        table().sel = 0;
        toggle_selected(); // draft: section 0 off — a real, in-progress user edit
        assert!(dirty());

        // What `sync_roster`'s `reset()` does mid-session: the table's IDENTITY changes out from
        // under the open editor. Re-seed with a DIFFERENT shape, so a surviving stale index would
        // provably be answering for the wrong library if this guard did nothing.
        crate::browse::seed_pins_for_test(&[true, true, true]);
        rebuild(false);

        assert!(
            !dirty(),
            "the stale in-progress edit was discarded, not carried forward against new indices"
        );
        assert!(matches!(commit(), Action::Done));
        assert_eq!(
            (
                crate::browse::pinned(0),
                crate::browse::pinned(1),
                crate::browse::pinned(2)
            ),
            (true, true, true),
            "commit applied the FRESH table's own state, never the pre-reset decision"
        );
    }

    /// **Codex review finding, 2026-09-04.** Fixed as a direct consequence of the previous test's
    /// fix (both live in the SAME `draft_rows` match arm): a freshly-landed row used to be seeded
    /// into `DRAFT` alone, never `ENTRY`, so [`dirty`]'s `ENTRY`-vs-`DRAFT` comparison had no entry
    /// to compare a toggle on that row against and could never see it — `Done` would never appear
    /// for an edit made on a library that landed after this screen opened.
    #[test]
    fn a_freshly_landed_row_can_independently_make_settings_dirty() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("late-row");
        crate::browse::seed_pins_for_test(&[true]);
        enter_settings();
        assert!(!dirty());

        // A second library lands mid-edit — an ADDITIVE append (`SECTIONS_GEN` moves, `EPOCH`
        // does not), exactly like a real second source answering, never a reset.
        crate::browse::land_pin_for_test(true);
        rebuild(true);
        assert!(!dirty(), "landing alone is not an edit");

        table().sel = 1;
        toggle_selected();
        assert!(
            dirty(),
            "a toggle on a freshly-landed row must make Done appear, or it can never be committed"
        );
    }

    /// **Drives the REAL BACK/Cancel path through `key()`**, not a stand-in for it — the gap
    /// Codex's round-1 review flagged: the other tests around this one prove the draft never
    /// reaches the live table, but none of them actually calls `key()`. This does, for both
    /// routes, and it is the direct no-write regression: `back_action` once walked a snapshot
    /// against the live table and undid differences through `browse::toggle_pin`; that loop is
    /// gone (2026-09-04) and BACK/Cancel must leave both the live pin and the record untouched.
    #[test]
    fn back_and_cancel_through_the_real_key_path_touch_neither_the_live_pin_nor_the_record() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("real-back");

        // First run.
        crate::browse::seed_pins_for_test(&[true, true]);
        enter();
        table().sel = 0;
        toggle_selected();
        assert!(matches!(key(SDLK_ESCAPE, 0), Action::Back));
        assert!(
            crate::browse::pinned(0),
            "first-run BACK left the live pin exactly as it was"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and recorded nothing"
        );

        // Settings mode — the route `key`'s own restore loop actually runs on.
        crate::browse::seed_pins_for_test(&[true, true]);
        enter_settings();
        table().sel = 0;
        toggle_selected();
        assert!(matches!(key(SDLK_ESCAPE, 0), Action::Cancel));
        assert!(
            crate::browse::pinned(0),
            "Settings-mode Cancel left the live pin exactly as it was"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and recorded nothing — the restore loop found nothing to restore"
        );
    }

    /// **Codex review finding, round 2 (2026-09-04).** The narrower race the round-1 fix above left
    /// open: a row the user HAS toggled (so `ENTRY` and `DRAFT` genuinely disagree) that ALSO
    /// drifts independently, live, to a value nobody chose through this screen at all —
    /// `resolve_pins` re-deriving an unrecorded default is not gated on whether the user has
    /// touched that particular row. (A `bool` has only two states, so the drift necessarily lands
    /// on either `ENTRY`'s original value or `DRAFT`'s edited one; the test below picks the latter
    /// — the SAME direction the user's own toggle went — deliberately, because that is the
    /// coincidence that made the old code's "restore" look plausible instead of obviously wrong.)
    /// `draft_rows`'s untouched-row sync is deliberately gated on `entry == draft` and skips a
    /// touched row for exactly that reason (a real edit must never be silently reabsorbed as a new
    /// baseline). The old `back_action` then walked a `BASE` snapshot against the live table and
    /// called `browse::toggle_pin` for every difference — which, with `BASE` frozen at the
    /// pre-toggle value, flipped the live pin AND recorded `asked: true`: a Cancel press
    /// persisting an answer nobody gave. That loop is gone (2026-09-04): nothing is written before
    /// `commit`, so Cancel has nothing to restore and writes nothing. This test is what keeps it
    /// that way — the drifted value (`false`) is chosen to differ from the row's ENTRY snapshot
    /// (`true`), the one choice under which a restore loop would misfire.
    #[test]
    fn a_toggled_rows_independent_drift_is_never_restored_or_recorded_by_cancel() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("toggle-and-drift-race");
        crate::browse::seed_pins_for_test(&[true, true]);
        enter_settings();
        table().sel = 0;
        toggle_selected(); // a real user edit: DRAFT[0] = false, ENTRY[0] stays true

        // A second source answers mid-edit and `resolve_pins` re-derives section 0's own
        // still-unrecorded default independently of the user's press, landing on `false` — the
        // SAME direction the user's own toggle went in, which is precisely what makes the old
        // code's "restore" look plausible instead of obviously wrong: it thinks it is undoing the
        // user's press, when it is really clobbering an unrelated live event. `set_pinned_for_test`
        // stands in for that live mutation, exactly as the sibling test above does for an
        // untouched row.
        crate::browse::set_pinned_for_test(0, false);
        rebuild(true);

        assert!(matches!(key(SDLK_ESCAPE, 0), Action::Cancel));
        assert!(
            !crate::browse::pinned(0),
            "Cancel must leave the drifted live pin exactly as the drift left it (false), not \
             restore the pre-toggle baseline (true)"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and must record nothing — nothing here is an answer the user gave"
        );
    }

    /// The copy names the PEOPLE, and it has to read as a sentence at one, two and three friends —
    /// the case a hand-built "friend1, friend2, " string gets wrong at exactly one of them.
    ///
    /// **The names here are invented, and that is a rule rather than a preference.** A shared
    /// server's owner is a real person whose Plex handle is THEIRS, this repository is PUBLIC, and
    /// a test fixture is as public as a README. It is an easy thing to get wrong because the real
    /// handle is what is on screen while the feature is being written; four pull-request bodies
    /// already had to be redacted for exactly this. Any new fixture that stands in for a person
    /// uses a made-up name.
    #[test]
    fn the_copy_lists_the_people_who_shared_with_you() {
        let _g = crate::testlock::serial();
        assert_eq!(join_names(&names(&[])), None);
        assert_eq!(join_names(&names(&["ada"])).as_deref(), Some("ada"));
        assert_eq!(
            join_names(&names(&["ada", "kate.w"])).as_deref(),
            Some("ada and kate.w")
        );
        assert_eq!(
            join_names(&names(&["ada", "kate.w", "dad"])).as_deref(),
            Some("ada, kate.w and dad")
        );
    }

    #[test]
    fn missing_owner_handles_never_invent_an_extra_server() {
        let _g = crate::testlock::serial();
        let copy = body_copy_for(&[]);
        assert_eq!(
            copy,
            "Choose the libraries this television shows. Your favorites fill Home's shelves and the Library's own tabs; Settings lists every one you have."
        );
        assert!(!copy.contains("More than one server"));
    }

    /// **DOWN off the last library reaches the action pill.** Reported: "on screens containing a
    /// table plus a bottom action such as Done, pressing Down while focused on the last table row
    /// should move focus to the bottom button. Currently the button is generally reachable only by
    /// pressing Left." It was: this screen's DOWN arm was `table().move_sel(1)` unconditionally,
    /// which a clamped selection turns into a no-op at the end of the list.
    ///
    /// The list is seeded by hand rather than by a roster, because the roster lands on a worker and
    /// this is a claim about the FOCUS rule, not about discovery.
    #[test]
    fn down_off_the_last_row_reaches_the_bottom_action() {
        use crate::ui::table::{Row, Section};
        let _g = crate::testlock::serial();
        unsafe {
            SETTINGS_MODE = false;
            FOCUS = Focus::List;
        }
        table().set_sections(
            vec![Section::new("Server")
                .row(Row::new("Films"))
                .row(Row::new("Shows"))],
            0,
            false,
        );
        table().list_focused = true;
        table().sel = table().n_rows() - 1;
        key(SDLK_DOWN, 0);
        assert!(
            matches!(unsafe { addr_of!(FOCUS).read() }, Focus::Action),
            "DOWN off the last library must reach the action, not sit on a clamped selection"
        );
        assert!(!table().list_focused, "…and only one accent capsule is drawn");
        // …and UP comes straight back, so the two are one vertical walk (rule 3).
        key(SDLK_UP, 0);
        assert!(matches!(unsafe { addr_of!(FOCUS).read() }, Focus::List));
        assert_eq!(table().sel, table().n_rows() - 1, "on the row it left from");
    }

    /// **A Settings editor that discovered nothing can still reach `Try again`.** Codex review,
    /// 2026-09-04: the band holds one control and the list is empty, so every direction is a wall
    /// — and this screen persisted the shared model's decision only on an explicit `Moved`, so
    /// `FOCUS` stayed `List` while the model had already settled onto the band. `set_focus`'s old
    /// guard made it worse by redirecting `Action` back to the empty list whenever a Settings
    /// editor was clean, which is exactly this state. The control was reachable by neither key nor
    /// pointer.
    #[test]
    fn a_settings_editor_that_discovered_nothing_can_still_reach_try_again() {
        use crate::ui::table::Section;
        let _g = crate::testlock::serial();
        crate::browse::reset();
        unsafe {
            SETTINGS_MODE = true;
            FOCUS = Focus::List;
        }
        table().set_sections(Vec::<Section>::new(), 0, false);
        table().list_focused = true;
        assert_eq!(crate::browse::section_count(), 0);
        assert_eq!(table().n_rows(), 0, "nothing was discovered");
        assert_eq!(
            bottom_actions(true, dirty(), true),
            BottomActions::Primary,
            "…so the band holds Try again"
        );
        assert!(
            focus_is_ctl(),
            "rule 10: with no rows and a band, focus settles onto the band with no key at all —              and that is what arms the press and draws the ring"
        );
        assert!(
            matches!(on_ok(), Action::None),
            "OK on it queues a retry rather than committing an empty answer"
        );
        // …and rule 9's guard lets LEFT follow the crumb here, because Try again commits nothing.
        unsafe { SETTINGS_MODE = false };
    }

    /// **A press commits the VERB it was armed on.** `Focus::Action` is `Focus::Action` whether
    /// the pill says `Try again` or `Start watching`, and the roster can land during the ~210 ms
    /// the tvOS press takes to spring back — so a press meant to retry discovery would commit the
    /// first-run answer and leave the ceremony instead (Codex review, 2026-09-04).
    #[test]
    fn an_action_that_changes_verb_under_an_armed_press_refuses_to_commit() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        unsafe {
            SETTINGS_MODE = false;
            FOCUS = Focus::Action;
            ARMED = None;
        }
        table().set_sections(Vec::new(), 0, false);
        assert_eq!(action_kind(), ActionKind::Retry, "nothing discovered yet");
        // `arm_action` records what the press is FOR; `begin_ctl` is the press itself, and `armed`
        // deliberately reads through it — so a test that omits it is testing the un-armed path.
        arm_action();
        crate::ui::press::begin_ctl(1);
        // …the roster lands while the press is still springing back.
        crate::browse::seed_two_source_table_for_test();
        assert_eq!(action_kind(), ActionKind::Start, "the same stop, a different verb");
        assert!(
            matches!(on_ok(), Action::None),
            "the deferred commit must refuse rather than answer the first-run question"
        );
        // …and an unchanged verb still commits normally.
        arm_action();
        crate::ui::press::begin_ctl(2);
        assert!(!matches!(on_ok(), Action::None));
        // …while an arm the press outlived is ignored rather than obeyed, from the CANCEL rather
        // than from the end of the bounce — see the next test for why that difference is visible.
        arm_action();
        crate::ui::press::begin_ctl(3);
        crate::ui::press::cancel();
        assert!(
            crate::ui::press::is_active(),
            "the cancelled press is still on screen"
        );
        assert!(armed().is_none(), "…and already disarmed, by construction");
        end_press();
        crate::browse::reset();
    }

    /// **An editor whose baseline never saw these rows must not claim there is nothing to lose.**
    /// Codex review, 2026-09-04: `enter_settings` snapshots the pin set ONCE and the roster lands
    /// on a worker, so a Settings editor opened before discovery answered holds an empty `ENTRY`.
    /// Under the old per-press model a row that arrived afterwards and was toggled persisted at
    /// once while `dirty()` still read false: no Done appeared, and rule 9 read the silence as
    /// "nothing uncommitted". Under the draft model `draft_rows` absorbs the landing row into
    /// `ENTRY` at first sight, so its first toggle IS an edit
    /// (`a_roster_that_lands_after_the_editor_opened_is_uncommitted_only_once_edited`).
    /// Spring the crate-global press all the way back to idle. A test that arms one owes this to
    /// the next test in the file: `press` is process-wide state, and `testlock::serial` orders the
    /// tests without cleaning up after them.
    fn end_press() {
        crate::ui::press::cancel();
        for i in 0..200 {
            crate::ui::press::tick(4 + i * 16, 0.016);
        }
    }

    /// **A press the user walked away from must not swallow the next one.** Codex review,
    /// 2026-09-04: `press::cancel` clears the commit but leaves the press ACTIVE for the ~200 ms
    /// of its spring-back, so an arm expired against `is_active` outlived the press that owned it.
    /// A `TableView` row's OK is immediate and starts no press of its own, so the row's activation
    /// was judged against the pill the user had already left, and the pin never flipped: press the
    /// action, press UP into the list, press OK — nothing happens.
    /// The draft's answer for one section — what the live table will hold after Done.
    fn draft_on(section: usize) -> bool {
        unsafe {
            (*addr_of!(DRAFT))
                .iter()
                .find(|(s, _)| *s == section)
                .map(|(_, on)| *on)
                .unwrap_or(false)
        }
    }

    #[test]
    fn a_press_abandoned_for_the_list_does_not_swallow_the_row_s_own_ok() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        crate::browse::seed_two_source_table_for_test();
        unsafe {
            SETTINGS_MODE = false;
            FOCUS = Focus::Action;
            ARMED = None;
        }
        rebuild(false);
        table().list_focused = false;
        table().sel = 0;
        let section = crate::browse::all_source_rows()[0].section;
        let before = draft_on(section);
        // The user presses the pill…
        arm_action();
        crate::ui::press::begin_ctl(1);
        // …then navigates instead. `app.rs`'s `note_global_press` cancels on any bound non-OK key,
        // and this screen's UP moves focus into the list — both while the bounce still plays.
        crate::ui::press::cancel();
        key(SDLK_UP, 0);
        assert!(
            crate::ui::press::is_active(),
            "the abandoned press is still springing back — the whole window this bug lived in"
        );
        assert!(matches!(settled_focus(), Focus::List), "focus is on the row");
        // …and OK on the row is the row's own, immediate, and starts no press.
        assert!(matches!(on_ok(), Action::None));
        assert_ne!(
            draft_on(section),
            before,
            "the row toggles (in the DRAFT — a toggle never touches the live table until Done); \
             it is not judged against a pill the user already left"
        );
        end_press();
        crate::browse::reset();
    }

    /// Rule 9 against the draft model (2026-09-04, two lanes meeting): an editor opened BEFORE
    /// discovery answered has no baseline for the rows that land afterwards — and rather than
    /// walling on that ignorance, `ENTRY` absorbs a landed row as it arrives, so nothing is
    /// uncommitted until the user toggles one, and then it is.
    #[test]
    fn a_roster_that_lands_after_the_editor_opened_is_uncommitted_only_once_edited() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        unsafe {
            SETTINGS_MODE = true;
            FOCUS = Focus::List;
            *addr_of_mut!(ENTRY) = Vec::new(); // opened before discovery answered
            *addr_of_mut!(DRAFT) = Vec::new();
        }
        // …and the roster lands afterwards.
        crate::browse::seed_two_source_table_for_test();
        rebuild(false);
        assert!(table().n_rows() > 0, "there are rows to edit");
        assert!(!dirty(), "nothing has been edited, so nothing reports as changed");
        assert_eq!(
            bottom_actions(true, dirty(), false),
            BottomActions::None,
            "…so no Done is drawn, and LEFT has no band to land on"
        );
        assert!(
            !shape().uncommitted,
            "a landed roster nobody has touched is committed; rule 9 has nothing to wall"
        );
        let section = crate::browse::all_source_rows()[0].section;
        toggle_draft(section);
        assert!(dirty(), "the first toggle of a landed row is an edit");
        assert!(
            shape().uncommitted,
            "…and rule 9 walls LEFT until it is committed or cancelled"
        );
        table().list_focused = true;
        assert!(
            matches!(key(SDLK_LEFT, 0), Action::None),
            "LEFT stays on the screen instead of discarding an edit it cannot restore"
        );
        // …and toggling it back leaves nothing uncommitted again: LEFT is BACK.
        toggle_draft(section);
        assert!(!dirty());
        assert!(!shape().uncommitted, "a draft that matches its entry has nothing to wall");
        assert!(matches!(key(SDLK_LEFT, 0), Action::Cancel));
        unsafe { SETTINGS_MODE = false };
        crate::browse::reset();
    }

    #[test]
    fn the_bottom_row_expresses_forward_back_and_commit_as_distinct_states() {
        let _g = crate::testlock::serial();
        assert_eq!(
            bottom_actions(false, true, false),
            BottomActions::Primary,
            "first run always offers its commit"
        );
        assert_eq!(
            bottom_actions(true, false, false),
            BottomActions::None,
            "a clean Settings editor has nothing to commit, and no longer spends the band saying \
             how to leave"
        );
        assert_eq!(
            bottom_actions(true, true, false),
            BottomActions::Primary,
            "Done appears after an edit"
        );
        assert_eq!(
            bottom_actions(true, false, true),
            BottomActions::Primary,
            "Retry is a real action even on a pristine editor"
        );
    }
}
