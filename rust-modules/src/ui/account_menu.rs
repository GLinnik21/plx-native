//! The Home **profile menu** — a small popover opened from the top-left profile chip, on the SAME
//! animated [`TableView`] as the in-player subtitle/audio menu. Switch Plex Home profile ("Change
//! profile" → who's-watching), "Sign out", or "Sign in". The menu only reports the chosen action
//! via [`on_ok`]; `app.rs` performs the routing.
//!
//! **The rows are a function of the account state, and that state is the persisted session** —
//! `Session::account`, read fresh at each [`open`]. It used to be `session::current().is_some()`,
//! which is a *sentinel*, not a fact: the single-user (no Plex Home) path leaves the active profile
//! an empty `UserRef`, so every surface deciding on its emptiness told a signed-in owner they were
//! signed out — this popover headed itself "Account", and the chip that opens it says "Sign in".
//!
//! **The chip is still on the sentinel** (`ui/widgets.rs:117` `profile_chip`, `title.is_empty()`),
//! so the two surfaces currently disagree on one screen. It is a one-block change — word the label
//! and the avatar initial from `Session::account(current().as_ref())` inside the generation-keyed
//! cache — left out of this pass only because that file belongs to another change in flight.
#![allow(non_upper_case_globals)]
use crate::plex::session::Account;
use crate::ui::consts::*;
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::{Glass, GlassFrame};
use crate::ui::Rect;
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// What the highlighted row does on OK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    ChangeProfile,
    SignIn,
    SignOut,
}

/// Header for a session we cannot name — signed in but no roster has landed yet (and the signed-out
/// case, where naming an account we do not have would be the same lie in reverse).
const HEADER_FALLBACK: &str = "Account";

/// The first production user of dynamic widget glass: Home stays at presentation rate while its
/// dirty blurred backdrop is refreshed at most every third successful present through the policy.
static mut POP: Popover = Popover::with_glass(Glass::DYNAMIC_BACKDROP);
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The ordered rows captured at [`open`] — the ONE place row order lives, so [`on_ok`]'s index
/// mapping cannot drift from what was actually drawn.
static mut ROWS: &[Action] = &[];

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

/// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the key
/// ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes nothing
/// else, so without this the fingerprint records the panel opening and closing and nothing between.
/// Through `addr_of!` rather than the module's own `table()`, which hands out a `&'static mut`.
pub(crate) fn sel() -> i32 {
    unsafe { (*addr_of!(TABLE)).sel }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// The rows for an account state, in order. Signed out, the only truthful action is signing in;
/// offering "Change profile" there dead-ends in an empty who's-watching screen. Signed in, "Sign
/// in" is a lie, so it is never offered — "Change profile" is, whenever plex.tv can serve a roster.
fn rows_for(acc: &Account) -> &'static [Action] {
    match (acc.signed_in, acc.can_switch) {
        (false, _) => &[Action::SignIn],
        (true, true) => &[Action::ChangeProfile, Action::SignOut],
        (true, false) => &[Action::SignOut],
    }
}

fn label(a: Action) -> &'static str {
    match a {
        Action::ChangeProfile => "Change profile",
        Action::SignIn => "Sign in",
        Action::SignOut => "Sign out",
        Action::None => "",
    }
}

/// Rows that leave for another screen carry the drill-in chevron; "Sign out" acts in place.
fn drills_in(a: Action) -> bool {
    matches!(a, Action::ChangeProfile | Action::SignIn)
}

pub fn open() {
    // The persisted session is the file of record — a roster refresh or a sign-out anywhere in the
    // app lands THERE, and the in-memory profile carries no account state at all — so the menu
    // re-reads it per open (a few hundred bytes, once per key press) instead of trusting a snapshot.
    // `peek`, not `load`: a menu opening must never be able to WRITE the session file.
    let sess = crate::plex::session::peek();
    let cur = crate::plex::session::current();
    let acc = sess.account(cur.as_ref());
    let rows = rows_for(&acc);
    unsafe { addr_of_mut!(ROWS).write(rows) };
    let mut sec = Section::new(acc.name.unwrap_or_else(|| HEADER_FALLBACK.to_string()));
    for a in rows {
        sec = sec.row(Row::new(label(*a)).chevron(drills_in(*a)));
    }
    table().compact = true; // small one-word action list — BODY labels, not menu-size HEADLINE bold
    table().set_sections(vec![sec], 0, false);
    // ROWS *is* the index→action map, so it must stay one-to-one with what was built above; a row
    // appended here and not to `rows_for` is exactly the drift this replaced.
    debug_assert_eq!(rows.len() as i32, table().n_rows());
    pop().open();
}

/// Pointer hover: focus follows the cursor over the popover rows.
pub fn pointer_focus(mx: f32, my: f32) {
    if !is_open() {
        return;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
    }
}

/// Pointer click: commit the row under the cursor (same as OK); a click elsewhere reports
/// Action::None and the caller dismisses like BACK.
pub fn click(mx: f32, my: f32) -> Action {
    if !is_open() {
        return Action::None;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
        return on_ok();
    }
    Action::None
}

pub fn close() {
    pop().close();
}

pub fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    }
}

/// Commit the highlighted row and close.
pub fn on_ok() -> Action {
    let sel = table().sel;
    close();
    action_at(unsafe { addr_of!(ROWS).read() }, sel)
}

/// The row list IS the mapping — a selection outside it (an empty menu, a stale index) is `None`
/// rather than whatever action happens to sit at that position in the other row set.
fn action_at(rows: &[Action], sel: i32) -> Action {
    usize::try_from(sel).ok().and_then(|i| rows.get(i)).copied().unwrap_or(Action::None)
}

/// Top-left popover, tucked under the profile chip.
fn panel_rect() -> Rect {
    let pw = 440.0f32;
    let px = 80.0f32;
    let py = 150.0f32;
    let ph = table().measured_height().clamp(120.0, 440.0);
    Rect::new(px, py, pw, ph)
}

pub fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let ph = panel_rect().h;
    table().update(dt, ph - 40.0);
}

/// Resolve cadence + source RGB before Home draws. The snapshot itself is deliberately deferred
/// until [`draw`], where the panel sits after every underlay pixel in draw order.
pub fn prepare_present(underlay_changed: bool) -> GlassFrame {
    if is_open() {
        pop().prepare_present(underlay_changed)
    } else {
        GlassFrame::IDENTITY
    }
}

pub fn draw() {
    if !is_open() {
        return;
    }
    use crate::ui::profile::phase;
    // Glass::DYNAMIC routes this requested dim through the source prepass instead of drawing the
    // 1920x1080 scrim. Keeping the ordinary painter call makes that policy own the distinction.
    let p = pop().painter(0.5, -16.0);
    let r = panel_rect();
    pop().panel(p, r, 24.0);
    phase("glass.foreground", || {
        table().draw(p, r);
    });
}

/// The (account state → header + rows) table, which is the whole of this bug: the words the menu
/// says about the user, and the actions it maps them to.
///
/// All but one drive the pure functions — `Session::account`, [`rows_for`], [`action_at`] —
/// with sessions built in the test, so they touch no global and need no lock; the seventh drives
/// the live `session::set_current` and takes `crate::testlock::serial()` for its whole body.
/// `open()` itself is not exercised: it owns `static mut TABLE`/`POP` (main-thread-only,
/// deliberately not `Sync`), so it is unrunnable under a parallel harness. Keeping the mapping in
/// pure functions is what makes the interesting half testable at all.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::session::{HomeUserRef, ServerRef, Session, UserRef};

    fn owner(title: &str) -> HomeUserRef {
        HomeUserRef { title: title.to_string(), admin: true, ..Default::default() }
    }
    fn managed(title: &str) -> HomeUserRef {
        HomeUserRef { title: title.to_string(), ..Default::default() }
    }
    /// A session that can reach its server, i.e. one the app actually boots into Home on.
    fn local(mut s: Session) -> Session {
        s.server = ServerRef { address: "192.0.2.10".into(), port: 32400, token: "srv".into(), ..Default::default() };
        s
    }
    fn menu(s: &Session, active: Option<&UserRef>) -> (String, Vec<&'static str>) {
        let acc = s.account(active);
        let rows = rows_for(&acc);
        (acc.name.unwrap_or_else(|| HEADER_FALLBACK.to_string()), rows.iter().map(|a| label(*a)).collect())
    }

    /// No session at all: the one honest action is signing in.
    #[test]
    fn signed_out_offers_only_sign_in() {
        let (name, rows) = menu(&Session::default(), None);
        assert_eq!(name, "Account");
        assert_eq!(rows, vec!["Sign in"]);
    }

    /// THE BUG: a signed-in account with no Plex Home never gets a profile written, so the active
    /// UserRef is empty — which must read as "signed in, unnamed roster entry aside", never as
    /// "signed out". The roster's admin entry is what names it.
    #[test]
    fn signed_in_without_plex_home_is_named_and_never_offered_sign_in() {
        let s = local(Session { account_token: "acct".into(), home_users: vec![owner("Gleb")], ..Default::default() });
        let (name, rows) = menu(&s, Some(&UserRef::default()));
        assert_eq!(name, "Gleb");
        assert_eq!(rows, vec!["Change profile", "Sign out"]);
    }

    /// A picked managed profile names the header even though the roster also could.
    #[test]
    fn active_profile_outranks_the_roster_owner() {
        let s = local(Session {
            account_token: "acct".into(),
            home_users: vec![owner("Gleb"), managed("Kid")],
            ..Default::default()
        });
        let active = UserRef { title: "Kid".into(), ..Default::default() };
        let (name, rows) = menu(&s, Some(&active));
        assert_eq!(name, "Kid");
        assert_eq!(rows, vec!["Change profile", "Sign out"]);
    }

    /// The roster hop looks for a NAMED entry, admin first: an admin tile that happens to carry an
    /// empty title must not swallow the name sitting behind it (find-then-filter, the very shape of
    /// bug this change exists to remove).
    #[test]
    fn an_unnamed_admin_does_not_hide_a_named_roster_entry() {
        let s = local(Session {
            account_token: "acct".into(),
            home_users: vec![owner(""), managed("Kid")],
            ..Default::default()
        });
        assert_eq!(menu(&s, Some(&UserRef::default())).0, "Kid");
    }

    /// An empty roster is UNKNOWN, not "no profiles" (a failed fetch persists an empty vec), so the
    /// row that re-fetches it stays — hiding it would strand a Plex Home created later.
    #[test]
    fn unknown_roster_keeps_the_switch_row_and_says_account() {
        let s = local(Session { account_token: "acct".into(), ..Default::default() });
        let (name, rows) = menu(&s, Some(&UserRef::default()));
        assert_eq!(name, "Account");
        assert_eq!(rows, vec!["Change profile", "Sign out"]);
    }

    /// A server-only session (no plex.tv token) is still signed IN — it is streaming — but cannot
    /// switch profiles, because the roster and per-user tokens both come from plex.tv. Only a
    /// legacy/hand-written auth.json reaches this today (`login_thread` stores the account token
    /// before discovery), which is exactly why it is pinned rather than assumed away.
    #[test]
    fn server_only_session_can_sign_out_but_not_switch() {
        let s = local(Session { user: UserRef { title: "Gleb".into(), ..Default::default() }, ..Default::default() });
        let (name, rows) = menu(&s, None);
        assert_eq!(name, "Gleb");
        assert_eq!(rows, vec!["Sign out"]);
    }

    /// The seam `open()` actually uses: the crate-global active profile really does reach the
    /// header, and clearing it (sign-out) really does fall back through the persisted session.
    /// Takes `testlock::serial()` for the whole test — `set_current`/`current` are process-global.
    #[test]
    fn the_live_profile_global_feeds_the_header() {
        let _serial = crate::testlock::serial();
        let restore = crate::plex::session::current();
        let s = local(Session {
            account_token: "acct".into(),
            home_users: vec![owner("Gleb"), managed("Kid")],
            ..Default::default()
        });
        crate::plex::session::set_current(Some(UserRef { title: "Kid".into(), ..Default::default() }));
        let picked = menu(&s, crate::plex::session::current().as_ref()).0;
        crate::plex::session::set_current(None);
        let cleared = menu(&s, crate::plex::session::current().as_ref()).0;
        crate::plex::session::set_current(restore); // BEFORE the asserts: a failure must not leak
        assert_eq!(picked, "Kid");
        assert_eq!(cleared, "Gleb");
    }

    /// Every row set maps position → action by the list it drew, and anything off the end is None
    /// (not the other set's action at that index, which is exactly what the old fixed 0/1 map did).
    #[test]
    fn selection_maps_by_the_drawn_row_list() {
        let signed_out = rows_for(&Session::default().account(None));
        assert_eq!(action_at(signed_out, 0), Action::SignIn);
        assert_eq!(action_at(signed_out, 1), Action::None);
        let s = local(Session { account_token: "acct".into(), ..Default::default() });
        let full = rows_for(&s.account(None));
        assert_eq!(action_at(full, 0), Action::ChangeProfile);
        assert_eq!(action_at(full, 1), Action::SignOut);
        assert_eq!(action_at(full, 2), Action::None);
        assert_eq!(action_at(full, -1), Action::None);
        let no_switch = rows_for(&local(Session::default()).account(None));
        assert_eq!(action_at(no_switch, 0), Action::SignOut);
        assert_eq!(action_at(no_switch, 1), Action::None);
    }
}
