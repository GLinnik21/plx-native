//! The **profile menu** — a small popover opened from the top-left profile chip, on the SAME
//! animated [`TableView`] as the in-player subtitle/audio menu. Switch Plex Home profile ("Change
//! profile" → who's-watching), "Sign out", "Sign in" and Settings. The menu only
//! reports the chosen action via [`on_ok`]; `app.rs` performs the routing.
//!
//! It is a popover on **whichever of the three screens wears the shared top bar** — Home, the
//! Library or Search — because the chip is a stop on all three. That page is drawn once into the
//! shared host snapshot ([`crate::ui::popover::host`]); later menu frames reuse the one texture
//! while the page's focus/artwork springs pause until dismissal. `app.rs`'s
//! `Route::Account { over: BarHost }` carries both that host and the return destination. This
//! module was "the HOME profile menu" while Home was the only screen whose chip could be pressed.
//!
//! **This module owned that mechanism privately until 2026-09-02** — a `static mut HOST_FRAME:
//! gfx::FrameCache` plus a `draw_cached_host`/`capture_host` pair `app.rs` special-cased around the
//! page closure — and no other popover could reach it. It is `popover::host` now, opted into with
//! one `caching_host()` on the constructor, and the account arm in `app.rs` lost its special case
//! with it.
//!
//! **The rows are a function of the account state, and that state is the persisted session** —
//! `Session::account`, read fresh at each [`open`]. It used to be `session::current().is_some()`,
//! which is a *sentinel*, not a fact: the single-user (no Plex Home) path leaves the active profile
//! an empty `UserRef`, so every surface deciding on its emptiness told a signed-in owner they were
//! signed out — this popover headed itself "Account", and the chip that opens it says "Sign in".
//!
//! **The chip was still on the sentinel** — `ui/widgets.rs`'s `profile_chip` labelled itself from
//! `title.is_empty()`, so on a single-user account the two surfaces disagreed on one screen: the
//! menu headed itself with the owner's name while the chip that opens it said "Sign in". Fixed
//! 2026-08-23, and fixed by MOVING THE WORDS HERE rather than by writing the same match a second
//! time: [`chip_label`] is the one resolver, this module owns it beside the rows it has to agree
//! with, and the chip calls it. Two surfaces cannot drift on a question only one of them answers.
#![allow(non_upper_case_globals)]
use crate::plex::session::Account;
use crate::ui::consts::*;
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
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
    /// **Settings** — the reachable owner of Home sources, Privacy, Legal notices and About
    /// ([`crate::ui::settings`]). Offered in EVERY build and in **both** account states: someone
    /// who cannot sign in has still received a copy of this software, and LG's Privacy Guideline
    /// requires the policy to be readable *in the app* rather than only on the store listing.
    Settings,
    /// **Lab builds only** — snapshot the diagnostic ring and upload it (`crate::lab`). It is in
    /// this menu because it must be reachable with the D-PAD ALONE: the remote trigger is a colour
    /// button (BLUE, `wcode` 489 on the dev set), and an LG Cloud Test Lab virtual remote may not
    /// offer colour buttons at all — nor is that code guaranteed on a set nobody here has touched
    /// (`docs/lab-diagnostics.md` §7). Never offered in any other build —
    /// [`crate::lab::menu_row_enabled`] is `false` at compile time.
    SendDiagnostics,
}

/// Header for a session we cannot name — signed in but no roster has landed yet (and the signed-out
/// case, where naming an account we do not have would be the same lie in reverse).
const HEADER_FALLBACK: &str = "Account";

/// One cached snapshot per open. The host page is deliberately frozen while this modal owns input,
/// so a dynamic policy would repeatedly resample identical pixels during the menu's own appear
/// spring and spend the exact blur work the freeze exists to avoid — and `caching_host` is the
/// other half of that same argument, applied to the page rather than to the blur.
static mut POP: Popover = Popover::new().caching_host();
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
    // The lab row is a THIRD axis rather than an append, so every row set stays a `&'static`
    // slice and [`action_at`]'s index mapping keeps working unchanged. Six arms is the price of
    // not allocating a row vector per open; the alternative was a `Vec` in a static.
    match (
        acc.signed_in,
        acc.can_switch,
        crate::lab::menu_row_enabled(),
    ) {
        (false, _, false) => &[Action::SignIn, Action::Settings],
        (false, _, true) => &[Action::SignIn, Action::Settings, Action::SendDiagnostics],
        (true, true, false) => &[Action::ChangeProfile, Action::SignOut, Action::Settings],
        (true, true, true) => &[
            Action::ChangeProfile,
            Action::SignOut,
            Action::Settings,
            Action::SendDiagnostics,
        ],
        (true, false, false) => &[Action::SignOut, Action::Settings],
        (true, false, true) => &[Action::SignOut, Action::Settings, Action::SendDiagnostics],
    }
}

/// **What the profile CHIP calls the user** — the unfurled name beside the avatar, and the initial
/// inside it (its first character).
///
/// It lives here, not in `ui::widgets`, because it is a statement about the ACCOUNT and it has to
/// agree with the menu the chip opens. Every arm is one of this module's own answers:
///
/// - a name — the active managed profile, else the persisted roster's owner ([`Account::name`]);
/// - signed in and nameless — [`HEADER_FALLBACK`], the same word the menu heads itself with, which
///   is a missing NAME and not a missing user;
/// - signed out — the label of the one row the menu then offers, so the chip and the menu behind it
///   cannot say different things about the same press.
///
/// **The bug this replaced** was the chip deciding all three from `current().title.is_empty()`. An
/// account **without Plex Home** never gets a profile written at all, so that title is empty for a
/// signed-in owner and the chip offered them "Sign in" — which is the first thing a reviewer on a
/// fresh test account sees, and the last thing they should.
pub(crate) fn chip_label(acc: &Account) -> String {
    match (&acc.name, acc.signed_in) {
        (Some(n), _) => n.clone(),
        (None, true) => HEADER_FALLBACK.to_string(),
        (None, false) => label(Action::SignIn).to_string(),
    }
}

fn label(a: Action) -> &'static str {
    match a {
        Action::ChangeProfile => "Change profile",
        Action::SignIn => "Sign in",
        Action::SignOut => "Sign out",
        Action::Settings => "Settings",
        Action::SendDiagnostics => "Send diagnostics",
        Action::None => "",
    }
}

/// Rows that leave for another screen carry the drill-in chevron; "Sign out" acts in place.
fn drills_in(a: Action) -> bool {
    matches!(a, Action::ChangeProfile | Action::SignIn | Action::Settings)
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
        let row = Row::new(label(*a)).chevron(drills_in(*a));
        sec = sec.row(row);
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
    pop().dismiss();
}

pub fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    } else {
        return;
    }
    // The selection moved and nothing behind this menu did — see `popover::note_own_damage`.
    crate::ui::popover::note_own_damage();
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
    usize::try_from(sel)
        .ok()
        .and_then(|i| rows.get(i))
        .copied()
        .unwrap_or(Action::None)
}

/// Top-left popover, tucked under the profile chip.
///
/// `px` is the app's own side margin: it was a literal 80, which sat 16px outside the 5% overscan
/// frame — and the chip it hangs off is at `MARGIN_X`, so aligning the two is what the design meant
/// anyway. `py` clears `widgets::TOP_BAR_BOTTOM` (130) by a `space::MD`.
fn panel_rect() -> Rect {
    let pw = 440.0f32;
    let px = crate::ui::consts::MARGIN_X;
    let py = 154.0f32;
    let ph = table().measured_height().clamp(120.0, 440.0);
    Rect::new(px, py, pw, ph)
}

/// The panel at its TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]) — the clamp
/// ceiling rather than this session's measured height, since the audit grades the widest state a
/// surface can be in and the height comes from a `TableView` no host test can measure.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, crate::ui::Rect)>) {
    let r = panel_rect();
    out.push((
        "account menu panel",
        crate::ui::Rect::new(r.x, r.y, r.w, 440.0),
    ));
}

pub fn update(dt: f32) {
    if !pop().visible() {
        return;
    }
    // Scoped: the appear spring and the selection glide are this menu's motion, not the frozen
    // page's — see `popover::own_motion`.
    let _own = crate::ui::popover::own_motion();
    pop().update(dt);
    // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
    table().update(dt, panel_rect().h);
}

/// How dark the page goes behind this menu — [`draw_scrim`]'s weight, and the module's own, in the
/// same per-module shape `item_menu` and `alt_sources` use.
///
/// It has ONE reader: [`draw`] used to pass it to `Popover::painter`, and now takes
/// `content_painter`, which draws no scrim at all. Named rather than inlined because the scrim and
/// the panel are two calls in two functions that have to describe one composite.
const SCRIM_A: f32 = 0.5;

/// The modal dim, drawn as part of the HOST PAGE rather than with the panel.
///
/// This is what the panel's cached glass looks through, so `app.rs` draws it at the end of the host
/// page before the panel takes its one snapshot. Keeping it in the page closure also makes the
/// composition valid if the policy changes again; there is one draw order for both source paths.
///
/// It also LIFTS this menu's opener — the profile chip — back out of the dim. The chip is what the
/// panel unfurls from and the only thing on screen the panel is about, so dimming it under its own
/// menu is the same bug the focused card had. Only the DRAW half of the [`Opener`] is used: this
/// panel's placement is its own (it hangs under the top bar), not a function of the chip's rect.
///
/// [`Opener`]: crate::ui::popover::Opener
pub fn draw_scrim() {
    if pop().visible() {
        let _live = crate::ui::popover::host::live();
        pop().scrim_lifting(SCRIM_A, &OPENER);
    }
}

/// This menu's opener: the top-left profile chip, which `ui::widgets` draws (it owns the chip's
/// rect and its unfurl spring). A `const` even though the menu now has THREE hosts — `Route::
/// Account { over }` is any of the bar-wearing pages with this popover over it — because the chip
/// is the same shared control on all three, at the same rect, so the lift does not vary with the
/// page underneath.
const OPENER: crate::ui::popover::Opener =
    crate::ui::popover::Opener::drawn(crate::ui::widgets::redraw_profile_chip);

pub fn draw() {
    if !pop().visible() {
        return;
    }
    use crate::ui::profile::phase;
    // Everything below is LIVE over the frozen host page — see `popover::host::live`.
    let _live = crate::ui::popover::host::live();
    // `content_painter`, NOT `painter`: the scrim is already on the page — see [`draw_scrim`].
    let p = pop().content_painter(-16.0);
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
        HomeUserRef {
            title: title.to_string(),
            admin: true,
            ..Default::default()
        }
    }
    fn managed(title: &str) -> HomeUserRef {
        HomeUserRef {
            title: title.to_string(),
            ..Default::default()
        }
    }
    /// A session that can reach its server, i.e. one the app actually boots into Home on.
    fn local(mut s: Session) -> Session {
        s.server = ServerRef {
            address: "192.0.2.10".into(),
            port: 32400,
            token: "srv".into(),
            ..Default::default()
        };
        s
    }
    fn menu(s: &Session, active: Option<&UserRef>) -> (String, Vec<&'static str>) {
        let acc = s.account(active);
        let rows = rows_for(&acc);
        (
            acc.name.unwrap_or_else(|| HEADER_FALLBACK.to_string()),
            rows.iter().map(|a| label(*a)).collect(),
        )
    }

    /// No session at all: the one honest ACCOUNT action is signing in, with Settings beside it so
    /// privacy, legal and diagnostics remain reachable without an account.
    #[test]
    fn signed_out_profile_menu_does_not_offer_playback_diagnostics() {
        let (name, rows) = menu(&Session::default(), None);
        assert_eq!(name, "Account");
        assert_eq!(rows, vec!["Sign in", "Settings"]);
    }

    /// THE BUG: a signed-in account with no Plex Home never gets a profile written, so the active
    /// UserRef is empty — which must read as "signed in, unnamed roster entry aside", never as
    /// "signed out". The roster's admin entry is what names it.
    #[test]
    fn signed_in_without_plex_home_is_named_and_never_offered_sign_in() {
        let s = local(Session {
            account_token: "acct".into(),
            home_users: vec![owner("Gleb")],
            ..Default::default()
        });
        let (name, rows) = menu(&s, Some(&UserRef::default()));
        assert_eq!(name, "Gleb");
        assert_eq!(rows, vec!["Change profile", "Sign out", "Settings"]);
    }

    /// A picked managed profile names the header even though the roster also could.
    #[test]
    fn active_profile_outranks_the_roster_owner() {
        let s = local(Session {
            account_token: "acct".into(),
            home_users: vec![owner("Gleb"), managed("Kid")],
            ..Default::default()
        });
        let active = UserRef {
            title: "Kid".into(),
            ..Default::default()
        };
        let (name, rows) = menu(&s, Some(&active));
        assert_eq!(name, "Kid");
        assert_eq!(rows, vec!["Change profile", "Sign out", "Settings"]);
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
        let s = local(Session {
            account_token: "acct".into(),
            ..Default::default()
        });
        let (name, rows) = menu(&s, Some(&UserRef::default()));
        assert_eq!(name, "Account");
        assert_eq!(rows, vec!["Change profile", "Sign out", "Settings"]);
    }

    /// A server-only session (no plex.tv token) is still signed IN — it is streaming — but cannot
    /// switch profiles, because the roster and per-user tokens both come from plex.tv. Only a
    /// legacy/hand-written auth.json reaches this today (`login_thread` stores the account token
    /// before discovery), which is exactly why it is pinned rather than assumed away.
    #[test]
    fn server_only_session_can_sign_out_but_not_switch() {
        let s = local(Session {
            user: UserRef {
                title: "Gleb".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let (name, rows) = menu(&s, None);
        assert_eq!(name, "Gleb");
        assert_eq!(rows, vec!["Sign out", "Settings"]);
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
        crate::plex::session::set_current(Some(UserRef {
            title: "Kid".into(),
            ..Default::default()
        }));
        let picked = menu(&s, crate::plex::session::current().as_ref()).0;
        crate::plex::session::set_current(None);
        let cleared = menu(&s, crate::plex::session::current().as_ref()).0;
        crate::plex::session::set_current(restore); // BEFORE the asserts: a failure must not leak
        assert_eq!(picked, "Kid");
        assert_eq!(cleared, "Gleb");
    }

    /// **The chip and the menu, on one account state.** The chip used to answer this from
    /// `current().title.is_empty()` and so told a signed-in owner with no Plex Home to sign in; the
    /// menu behind that same press already headed itself "Gleb" and offered "Sign out". One
    /// resolver now, and this is the test that says the two agree.
    #[test]
    fn the_chip_and_its_menu_say_the_same_thing_about_the_account() {
        // THE BUG: single-user account, empty active profile, named by the roster's admin entry
        let s = local(Session {
            account_token: "acct".into(),
            home_users: vec![owner("Gleb")],
            ..Default::default()
        });
        let acc = s.account(Some(&UserRef::default()));
        assert_eq!(chip_label(&acc), "Gleb");
        assert_eq!(
            chip_label(&acc),
            menu(&s, Some(&UserRef::default())).0,
            "chip and header, one name"
        );
        assert!(
            !rows_for(&acc).contains(&Action::SignIn),
            "…and the menu never offered Sign in"
        );

        // signed in, no roster has ever landed: a missing NAME, not a missing user
        let nameless = local(Session {
            account_token: "acct".into(),
            ..Default::default()
        })
        .account(None);
        assert_eq!(chip_label(&nameless), HEADER_FALLBACK);

        // Signed out: the chip says exactly what the ACCOUNT row behind it says. That row is
        // first, and the assertion is on `[0]` rather than on the whole set — the set also carries
        // Settings, which is not an account action and which the chip has never claimed to speak
        // for.
        let out = Session::default().account(None);
        assert_eq!(chip_label(&out), label(Action::SignIn));
        assert_eq!(rows_for(&out)[0], Action::SignIn);
    }

    /// Settings is about the SOFTWARE rather than the account and is offered in every state.
    #[test]
    fn the_rows_that_need_no_account_are_offered_in_every_account_state() {
        // LG's Privacy Guideline requires the privacy notice to be reachable IN the app, and the
        // one state where it is easiest to forget is signed OUT — where someone who cannot get past
        // the QR screen has still received a copy of this software. Asserted across every row set
        // rather than on one, because `rows_for` is a six-arm match and five of the arms are the
        // easy ones.
        for s in [
            Session::default(),
            local(Session {
                account_token: "acct".into(),
                ..Default::default()
            }),
            local(Session::default()),
        ] {
            let rows = rows_for(&s.account(None));
            assert!(
                rows.contains(&Action::Settings),
                "no Settings row in {rows:?}"
            );
        }
    }

    #[test]
    fn settings_is_reachable_in_every_account_state() {
        for s in [
            Session::default(),
            local(Session {
                account_token: "acct".into(),
                ..Default::default()
            }),
            local(Session::default()),
        ] {
            let rows = rows_for(&s.account(None));
            assert!(
                rows.iter().any(|a| label(*a) == "Settings"),
                "no Settings row in {rows:?}"
            );
        }
    }

    /// Every row set maps position → action by the list it drew, and anything off the end is None
    /// (not the other set's action at that index, which is exactly what the old fixed 0/1 map did).
    #[test]
    fn selection_maps_by_the_drawn_row_list() {
        let signed_out = rows_for(&Session::default().account(None));
        assert_eq!(action_at(signed_out, 0), Action::SignIn);
        assert_eq!(action_at(signed_out, 1), Action::Settings);
        assert_eq!(action_at(signed_out, 2), Action::None);
        let s = local(Session {
            account_token: "acct".into(),
            ..Default::default()
        });
        let full = rows_for(&s.account(None));
        assert_eq!(action_at(full, 0), Action::ChangeProfile);
        assert_eq!(action_at(full, 1), Action::SignOut);
        assert_eq!(action_at(full, 2), Action::Settings);
        assert_eq!(action_at(full, 3), Action::None);
        assert_eq!(action_at(full, -1), Action::None);
        let no_switch = rows_for(&local(Session::default()).account(None));
        assert_eq!(action_at(no_switch, 0), Action::SignOut);
        assert_eq!(action_at(no_switch, 1), Action::Settings);
        assert_eq!(action_at(no_switch, 2), Action::None);
    }
}
