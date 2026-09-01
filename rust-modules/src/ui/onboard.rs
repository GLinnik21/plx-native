//! **"What goes on your Home?"** — the first-run route, `Shared Sources.dc.html` deliverable F.
//!
//! One question, asked once, after the profile picker and before Home: which of the libraries this
//! account can reach should merge into the front door. Your own arrive **On**; a friend's arrive
//! **Off**, because the point of asking is not to put a stranger's shelves on your Home unannounced.
//! Nothing here grants or blocks access — every library listed is browsable from the Library chip
//! either way, which is what makes skipping a real answer rather than a deferral.
//!
//! ## It is a ROUTE, not a sheet
//!
//! The same standing as sign-in and the who's-watching picker: it has a beginning and an end and
//! never comes back. That is what the app's no-full-screen-sheets rule protects — a sheet claims
//! there is a screen behind it, and there is not one here — so the rows sit on the app's own ground
//! with no panel, no shadow and no radius, and the [`TableView`](crate::ui::table::TableView) is
//! mounted directly rather than inside a [`Popover`](crate::ui::popover::Popover).
//!
//! ## The list is A's, not a second expression of it
//!
//! [`crate::ui::source_list`] builds it, exactly as the Library toolbar's Source panel does — same
//! groups, same rows, same marks, same "Home needs one library" refusal. The two differences are
//! stated as arguments and not as a second builder: this one lists EVERY library rather than the
//! browsed type's (`browse::all_source_rows` — there is no tab bar here to be scoped to), and it
//! carries no *Check for new shares* row, because a share arriving later must not reopen a
//! first-run screen.
//!
//! ## Skipping is honest
//!
//! Once at least one real library row has landed, BACK commits exactly what is drawn and records
//! the profile as asked; `Start watching` makes the same commit. Before that, both controls retry
//! discovery and keep the question recoverable instead of persisting an empty answer.
use crate::ui::consts::*;
use crate::ui::source_list::{self, Level, SrcAction, Tail};
use crate::ui::table::TableView;
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Button, Spinner, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, Rect, View};
use std::os::raw::c_uint;
use std::ptr::{addr_of, addr_of_mut};

/// **The one top guide both columns hang from** (`--route-top`). Clear of the 130px top chrome the
/// bar-wearing screens use, which this route does not draw — the alignment is what makes the copy
/// and the list read as one screen rather than two panes.
const ROUTE_TOP: f32 = 150.0;
/// The copy column: the product's one text column, at the page margin. It IS
/// [`home::HERO_COL_W`](crate::ui::home::HERO_COL_W) rather than a second 660 — this doc named that
/// constant while the literal beside it was a copy, which is one edit away from two text columns.
const COPY_W: f32 = crate::ui::home::HERO_COL_W;
/// The list column's left edge (`--route-list-x`); it runs to the opposite page margin.
const LIST_X: f32 = 930.0;
/// The one action's minimum measure. A pill still SIZES itself from its label through the
/// product's one formula ([`Button::pill_w`]) — this is a floor, so a short verb does not leave a
/// stub of a control on a screen whose only other content is a list.
const ACTION_W: f32 = 240.0;

/// The heading, and the one action. Both are the design's words: *Start watching* rather than
/// *Continue*, "a verb that says what happens next rather than one that says nothing".
const TITLE: &str = "What goes on your Home?";
const SETTINGS_TITLE: &str = "What appears on Home?";
const ACTION: &std::ffi::CStr = c"Start watching";
const DONE: &std::ffi::CStr = c"Done";
const RETRY: &std::ffi::CStr = c"Try again";

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
static mut BASE: Vec<(usize, bool)> = Vec::new();

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

/// What an OK / click means for `app.rs`.
pub enum Action {
    None,
    /// the question is answered (or skipped, which is an answer) — go to Home
    Done,
    Cancel,
}

/// Should this profile be asked at all? See `plex::pins::asks` for the two conditions.
pub fn asks() -> bool {
    crate::browse::first_run_asks()
}

/// Mount the route. Focus starts on the action, so the fast path is one press.
pub fn enter() {
    unsafe {
        SETTINGS_MODE = false;
        FOCUS = Focus::Action;
        TABLE_GEN = u32::MAX; // force the first build
    }
    table().list_focused = false;
    rebuild(false);
}

/// Reuse the same source picker as a Settings editor. BACK restores the captured pin set; Done
/// records the edited set and returns to the still-open Settings modal.
pub fn enter_settings() {
    unsafe {
        SETTINGS_MODE = true;
        FOCUS = Focus::List;
        TABLE_GEN = u32::MAX;
        *addr_of_mut!(BASE) = crate::browse::all_source_rows()
            .into_iter().map(|r| (r.section, r.pinned)).collect();
    }
    table().list_focused = true;
    rebuild(false);
}

pub fn settings_mode() -> bool { unsafe { *addr_of!(SETTINGS_MODE) } }
pub fn finish_settings() { unsafe { SETTINGS_MODE = false }; }
fn dirty() -> bool {
    if !settings_mode() { return true; }
    let now = crate::browse::all_source_rows();
    let base = unsafe { addr_of!(BASE).as_ref().cloned().unwrap_or_default() };
    base.iter().any(|(section, was)| now.iter().find(|r| r.section == *section).is_some_and(|r| r.pinned != *was))
}

/// (Re)build the rows from the section table. `keep` holds the cursor and glides the scroll — a
/// source landing mid-screen must not move the row under the user's thumb.
fn rebuild(keep: bool) {
    let gen = crate::browse::source_list_gen();
    let groups = crate::browse::source_groups();
    let rows = crate::browse::all_source_rows();
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

/// The list's visible frame — the right column, from the shared top guide down to a bottom margin
/// matching the page's own side margin. Derived rather than a literal so the list simply grows a
/// row when a friend's server answers, and scrolls when it runs out of screen.
fn list_frame() -> Rect {
    Rect::new(
        LIST_X,
        ROUTE_TOP,
        SCR_W as f32 - MARGIN_X - LIST_X,
        SCR_H as f32 - ROUTE_TOP - MARGIN_X,
    )
}

/// The action pill's FOCUS POP ([`crate::ui::widgets::CtlPop`]) — focus walks between it and the
/// library list beside it, so it animates both ways.
static mut ACTION_POP: crate::ui::widgets::CtlPop<1> = crate::ui::widgets::CtlPop::new();

pub fn update(dt: f32) {
    unsafe {
        let f = (addr_of!(FOCUS).read() == Focus::Action).then_some(0);
        (*addr_of_mut!(ACTION_POP)).step(f, dt);
    }
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
    table().update(dt, list_frame().h);
}

pub fn draw() {
    // The clear IS the app surface (see `ui::login::draw`) — the rows sit on the app's own ground,
    // which is the whole difference between a route and a panel.
    if !settings_mode() {
        crate::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
    }
    let p = Painter::root();
    let env = Env::inert();

    // ---- left column: the statement, then the one action ----
    let mut y = ROUTE_TOP;
    let title = TextView::new(if settings_mode() { SETTINGS_TITLE } else { TITLE }, theme::size::HERO, theme::TEXT_HEADING)
        .bold()
        .max_lines(2);
    y += title.draw(p, Rect::new(MARGIN_X, y, COPY_W, 240.0)) + theme::space::MD;
    let body = body_copy();
    let para = TextView::new(&body, theme::size::BODY, theme::TEXT_READING).max_lines(6);
    y += para.draw(p, Rect::new(MARGIN_X, y, COPY_W, 320.0)) + theme::space::XL;

    let action = if crate::browse::section_count() == 0 {
        RETRY
    } else if settings_mode() {
        DONE
    } else {
        ACTION
    };
    if !settings_mode() || dirty() || crate::browse::section_count() == 0 {
        let w = Button::pill_w(action.as_ptr(), theme::size::BODY, false).max(ACTION_W);
        let r = Rect::new(MARGIN_X, y, w, StatusOverlay::CTRL_H);
        unsafe { ACTION_RECT = r };
        Button::new(action.as_ptr(), theme::size::BODY, r)
            .focused(unsafe { addr_of!(FOCUS).read() } == Focus::Action)
            .scale(unsafe { addr_of!(ACTION_POP).as_ref().unwrap().scale(0) })
            .draw(&env, p);
    } else {
        unsafe { ACTION_RECT = Rect::new(-1.0, -1.0, 0.0, 0.0) };
        crate::ui::widgets::KeyHint::new(c"Press", c"BACK", c"to return").draw(p, MARGIN_X, SCR_H - 90.0);
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
    let tail =
        " Pick the ones you want on your Home screen \u{2014} you can browse any of them from \
                the Library chip whenever you like.";
    match join_names(&who) {
        // A roster of two servers with no handle on either (plex.tv described neither, or both are
        // ours) still has a question worth asking; it just has nobody to name in it.
        None => format!("More than one server is available to this account.{tail}"),
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
fn commit() -> Action {
    if crate::browse::section_count() == 0 {
        crate::browse::retry_discovery();
        crate::log("onboard: no discovered libraries yet — retry queued");
        return Action::None;
    }
    crate::browse::record_pins(true);
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
    let f = unsafe { addr_of!(FOCUS).read() };
    f == Focus::Action
}

/// Activate the focused stop — the whole of what OK means here, called on the key-down for the list
/// and on the press spring-back for the pill ([`focus_is_ctl`]). Split out of [`key`] so the two
/// timings run ONE activation rather than two that agree by inspection.
pub fn on_ok() -> Action {
    match unsafe { addr_of!(FOCUS).read() } {
        Focus::Action => commit(),
        Focus::List => {
            toggle_selected();
            Action::None
        }
    }
}

pub fn key(sym: c_uint, wcode: c_uint) -> Action {
    if is_back(sym, wcode) {
        if settings_mode() {
            let base = unsafe { addr_of!(BASE).as_ref().cloned().unwrap_or_default() };
            let now = crate::browse::all_source_rows();
            for (section, was) in base {
                if now.iter().find(|r| r.section == section).is_some_and(|r| r.pinned != was) {
                    crate::browse::toggle_pin(section);
                }
            }
            return Action::Cancel;
        }
        return commit();
    }
    let focus = unsafe { addr_of!(FOCUS).read() };
    if is_ok(sym) {
        return on_ok();
    }
    match sym {
        // LEFT/RIGHT cross between the two columns; the list is one press right of the action.
        SDLK_RIGHT if focus == Focus::Action => set_focus(Focus::List),
        SDLK_LEFT if focus == Focus::List => set_focus(Focus::Action),
        SDLK_DOWN if focus == Focus::List => table().move_sel(1),
        SDLK_UP if focus == Focus::List => table().move_sel(-1),
        // …and UP/DOWN on the action reach the list too, so a remote whose user only ever presses
        // down is not stuck on one control with a list beside it doing nothing.
        SDLK_DOWN | SDLK_UP if focus == Focus::Action => set_focus(Focus::List),
        _ => return Action::None,
    }
    crate::ui::idle::invalidate();
    Action::None
}

fn set_focus(f: Focus) {
    // Focus cannot enter a list that has no rows: the roster lands on a worker, so the first
    // frames of this route have nothing to select and a RIGHT there would take the accent off the
    // one control on screen and put it nowhere.
    let f = if settings_mode() && !dirty() && f == Focus::Action {
        Focus::List
    } else if f == Focus::List && table().n_rows() == 0 {
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

/// Flip the focused row's pin. A refusal (the last library on Home) changes nothing and says so on
/// the row itself, which is already drawn — so there is nothing to do here but rebuild either way.
fn toggle_selected() {
    let sel = table().sel;
    let acts: &[SrcAction] = unsafe { &*addr_of!(ACTS) };
    let act = usize::try_from(sel).ok().and_then(|i| acts.get(i)).copied();
    if let Some(SrcAction::Library(s)) = act {
        crate::browse::toggle_pin(s);
        // The words on every row can move, not just this one: turning a second library on releases
        // the "Home needs one library" refusal that was dimming another.
        rebuild(true);
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
    pointer_focus(mx, my) && focus_is_ctl()
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

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn start_and_back_cannot_answer_before_a_real_library_lands() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        assert!(matches!(commit(), Action::None));
        assert!(matches!(key(SDLK_ESCAPE, 0), Action::None));
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
}
