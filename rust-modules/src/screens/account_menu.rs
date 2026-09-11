//! The **profile menu** — a registered `Style::Sheet` surface on the shared `ModalStack`, opened
//! from the top-left profile chip. Switch Plex Home profile ("Change profile" → who's-watching),
//! "Sign out", "Sign in", Settings, and — in a lab build — "Send diagnostics".
//!
//! **Navigation owns its lifetime, its phase, its input scope and its dim.** It was
//! `ui/account_menu.rs` — a `Popover` plus three `static mut`s (`POP`, `TABLE`, `ROWS`) driven by
//! a `Route::Account { over: BarHost }` and a `key_account` ladder in the loop — until restructure
//! phase 10. What that route existed for is exactly what a surface gives for free: the page under
//! the panel stays on screen and stays the top PAGE, so there is nothing to name and nowhere to
//! "close back to". `BarHost` and the route variant are gone with it.
//!
//! Two things stay here that a reader may expect to find elsewhere, and both are deliberate:
//!
//! **The rows are a function of the account state, and that state is the persisted session** —
//! `Session::account`, read fresh at [`ScreenEvent::Mount`]. It used to be
//! `session::current().is_some()`, which is a *sentinel*, not a fact: the single-user (no Plex
//! Home) path leaves the active profile an empty `UserRef`, so every surface deciding on its
//! emptiness told a signed-in owner they were signed out. `peek`, not `load`: a menu opening must
//! never be able to WRITE the session file (`ci/check-deps.sh`'s `sessionwrite` gate).
//!
//! **[`chip_label`] lives here, beside the rows it has to agree with.** `ui/widgets.rs`'s
//! `profile_chip` labelled itself from `title.is_empty()`, so on a single-user account the two
//! surfaces disagreed on one screen: the menu headed itself with the owner's name while the chip
//! that opens it said "Sign in". Fixed 2026-08-23 by MOVING THE WORDS to one resolver rather than
//! writing the match a second time; two surfaces cannot drift on a question only one of them
//! answers. That is why `ui/widgets.rs` and `app/chrome.rs` both call INTO this module — the one
//! place in the tree where `ui/` names `screens/` in production code, and a debt that dies with
//! `profile_chip`'s standalone re-derivation when the frame plan lands (phase 11).

use std::borrow::Cow;

use crate::plex::session::Account;
use crate::screens::registry::{AppFx, AppLike, LoopReq};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InputKind, Key,
    LogicalState, Machine, NavOp,
};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable, GroupKind, GroupSpec,
    Hover, Placed, RenderStrategy, Screen, ScreenEvent, Scrim, Seat, Step, Stop,
};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::{Glass, GlassState};
use crate::ui::Rect;

/// What the highlighted row does on OK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    None,
    ChangeProfile,
    SignIn,
    SignOut,
    /// **Settings** — the reachable owner of Home sources, Privacy, Legal notices and About
    /// ([`crate::screens::registry::AppArg::Settings`]). Offered in EVERY build and in **both**
    /// account states:
    /// someone who cannot sign in has still received a copy of this software, and LG's Privacy
    /// Guideline requires the policy to be readable *in the app* rather than only on the store
    /// listing.
    Settings,
    /// **Lab builds only** — snapshot the diagnostic ring and upload it (`crate::lab`). It is in
    /// this menu because it must be reachable with the D-PAD ALONE: the remote trigger is a colour
    /// button (BLUE, `wcode` 489 on the dev set), and an LG Cloud Test Lab virtual remote may not
    /// offer colour buttons at all — nor is that code guaranteed on a set nobody here has touched
    /// (`docs/lab-diagnostics.md` §7). Never offered in any other build —
    /// [`crate::lab::menu_row_enabled`] is `false` at compile time.
    SendDiagnostics,
}

/// Header for a session we cannot name — signed in but no roster has landed yet (and the
/// signed-out case, where naming an account we do not have would be the same lie in reverse).
const HEADER_FALLBACK: &str = "Account";

/// How dark the page goes behind this menu — the peak the container ramps with the appear spring
/// (`ModalStack::draw_scrims`).
const SCRIM_A: f32 = 0.5;

/// The pinned ~24px corner radius.
const PANEL_RAD: f32 = 24.0;

pub(crate) const SHAPE: &str =
    "AccountMenu{header:str,rows:[u32],sel:u32,table:TableViewMotion}";

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
fn panel_rect(table: &TableView) -> Rect {
    let pw = 440.0f32;
    let px = crate::ui::consts::MARGIN_X;
    let py = 154.0f32;
    let ph = table.measured_height().clamp(120.0, 440.0);
    Rect::new(px, py, pw, ph)
}

/// The panel at its TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]) — the clamp
/// ceiling rather than a measured height, since the audit grades the widest state a surface can be
/// in and the height comes from a `TableView` no host test can measure.
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, crate::ui::Rect)>) {
    let r = panel_rect(&TableView::new());
    out.push((
        "account menu panel",
        crate::ui::Rect::new(r.x, r.y, r.w, 440.0),
    ));
}

pub(crate) struct AccountMenuScreen {
    entry: EntryId,
    header: String,
    rows: &'static [Action],
    table: TableView,
    glass: GlassState,
    /// Has the session been read yet? The rows are a snapshot taken ONCE, at `Mount` — a roster
    /// landing under an open menu must not renumber the rows the user is aiming at.
    built: bool,
}

impl AccountMenuScreen {
    pub(crate) fn new(entry: EntryId) -> Self {
        Self {
            entry,
            header: HEADER_FALLBACK.to_string(),
            rows: &[],
            table: TableView::new(),
            glass: GlassState::new(),
            built: false,
        }
    }

    /// The one read of the persisted session, at `Mount`. `peek`, never `load`.
    fn build(&mut self) {
        if self.built {
            return;
        }
        self.built = true;
        // The persisted session is the file of record — a roster refresh or a sign-out anywhere in
        // the app lands THERE, and the in-memory profile carries no account state at all — so the
        // menu reads it per open (a few hundred bytes, once per key press) instead of trusting a
        // snapshot.
        let sess = crate::plex::session::peek();
        let cur = crate::plex::session::current();
        let acc = sess.account(cur.as_ref());
        self.rows = rows_for(&acc);
        self.header = acc.name.unwrap_or_else(|| HEADER_FALLBACK.to_string());
        let mut sec = Section::new(self.header.clone());
        for a in self.rows {
            sec = sec.row(Row::new(label(*a)).chevron(drills_in(*a)));
        }
        // small one-word action list — BODY labels, not menu-size HEADLINE bold
        self.table.compact = true;
        self.table.set_sections(vec![sec], 0, false);
        // `rows` *is* the index→action map, so it must stay one-to-one with what was built above;
        // a row appended here and not to `rows_for` is exactly the drift this replaced.
        debug_assert_eq!(self.rows.len() as i32, self.table.n_rows());
    }

    fn frame(&self) -> Rect {
        panel_rect(&self.table)
    }

    /// Commit the focused row. **Every action dismisses**, exactly as the legacy `on_ok` did by
    /// closing before it returned; what differs per action is the request the loop then performs.
    fn activate<H: AppLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let act = action_at(self.rows, elem as i32);
        // The five that need the LOOP: three flip `app.route` after an `auth` call, one presents
        // another surface (whose `Style` is the application's to choose, not a screen's), and one
        // reaches `crate::lab`. None of them is expressible as a `Fx::Nav`, which is why they are
        // requests rather than effects a screen performs itself (§2.1, §14).
        let req = match act {
            Action::ChangeProfile => Some(LoopReq::AccountChangeProfile),
            Action::SignIn => Some(LoopReq::AccountSignIn),
            Action::SignOut => Some(LoopReq::AccountSignOut),
            Action::Settings => Some(LoopReq::AccountSettings),
            Action::SendDiagnostics => Some(LoopReq::AccountSendDiagnostics),
            Action::None => None,
        };
        if let Some(req) = req {
            fx.push(Fx::App(AppFx::Loop(req)));
        }
        fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
    }

    /// The highlighted row, for the focus probe — a READ of the cursor the engine moves.
    pub(crate) fn sel(&self) -> i32 {
        self.table.sel
    }
}

impl<H: AppLike> Machine<H> for AccountMenuScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => self.build(),
            ScreenEvent::Tick(tick) => {
                self.table.sel = cx
                    .focus
                    .current
                    .filter(|key| key.entry == self.entry)
                    .map(|key| key.elem as i32)
                    .unwrap_or(self.table.sel);
                self.table.update(tick.dt(), self.frame().h);
            }
            ScreenEvent::FocusMoved { to, .. } => self.table.sel = to.elem as i32,
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            ScreenEvent::PressCommit(_) => {
                if let Some(key) = cx.focus.current {
                    self.activate(key.elem, fx);
                }
            }
            ScreenEvent::Input(input) => {
                if let InputKind::Key {
                    key: Key::Back,
                    edge: Edge::Down,
                    ..
                } = input.kind
                {
                    fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
                    return Handled::Yes;
                }
            }
            _ => {}
        }
        Handled::No
    }
}

impl<H: AppLike> Focusable<H> for AccountMenuScreen {
    fn groups(&self, _: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: GroupId(0),
            kind: GroupKind::Column,
            seat: Seat::Remembered,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.frame(),
            len: self.rows.len(),
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, elem: &u32, _: &Cx<'_, H>) -> Option<GroupId> {
        ((*elem as usize) < self.rows.len()).then_some(GroupId(0))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let next = match dir {
            Dir::Up => (key.elem as usize).checked_sub(1),
            Dir::Down => Some(key.elem as usize + 1),
            _ => None,
        };
        match next.filter(|i| *i < self.rows.len()) {
            Some(i) => Step::Move(FocusKey {
                entry: self.entry,
                elem: i as u32,
            }),
            None => Step::Edge,
        }
    }
    fn place(&self, elem: &u32, _: &Cx<'_, H>, _: At) -> Option<Placed> {
        if (*elem as usize) >= self.rows.len() {
            return None;
        }
        let rect = self.table.row_frame(self.frame(), *elem as i32)?;
        Some(Placed {
            rect,
            rest_rect: rect,
            clip: self.frame(),
            index: Some(*elem),
        })
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        if self.group_of(&want.elem, cx).is_some() {
            want
        } else {
            FocusKey {
                entry: self.entry,
                elem: 0,
            }
        }
    }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        FocusKey {
            entry: self.entry,
            elem: self.table.sel.max(0) as u32,
        }
    }
}

impl<H: AppLike> Screen<H> for AccountMenuScreen {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn name(&self) -> &'static str {
        crate::screens::registry::word::ACCOUNT
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    /// The modal dim, and the CHIP lifted back out of it. The chip is what the panel unfurls from
    /// and the only thing on screen the panel is about, so dimming it under its own menu is the
    /// same bug the focused card had. Only the DRAW half of the legacy `Opener` is used: this
    /// panel's placement is its own (it hangs under the top bar), not a function of the chip's
    /// rect.
    fn scrim(&self) -> Scrim {
        Scrim::lifting(SCRIM_A, crate::ui::widgets::redraw_profile_chip)
    }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {
        Glass::CACHED.prepare(&mut self.glass, false);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        let r = self.frame();
        let measure = f.measure;
        Glass::CACHED.panel(p, r, 0.0, PANEL_RAD);
        crate::ui::profile::phase("glass.foreground", || {
            self.table.draw(p, r, measure);
        });
        for elem in 0..self.rows.len() as u32 {
            if let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) {
                f.stop(
                    p,
                    Stop {
                        key: FocusKey {
                            entry: self.entry,
                            elem,
                        },
                        rect: placed.rect,
                        rest_rect: placed.rest_rect,
                        clip: placed.clip,
                        hover: Hover::Focus,
                        activate: Activate::Immediate,
                    },
                );
            }
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

impl LogicalState for AccountMenuScreen {
    fn write(&self, c: &mut Canon) {
        c.str(&self.header).seq(self.rows.len());
        for a in self.rows {
            c.u32(*a as u32);
        }
        c.u32(self.table.sel as u32);
        self.table.write_motion(c);
    }
    fn probe(&self, out: &mut String) {
        out.push_str("account_menu");
    }
}

/// The (account state → header + rows) table, which is the whole of this module's history: the
/// words the menu says about the user, and the actions it maps them to.
///
/// All eleven moved by NAME from `ui/account_menu.rs` (restructure phase 10). They drive the pure
/// functions — `Session::account`, [`rows_for`], [`action_at`] — with sessions built in the test,
/// so they touch no global and need no lock; the seventh drives the live `session::set_current`
/// and takes `crate::testlock::serial()` for its whole body.
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

    /// The seam the mount actually uses: the crate-global active profile really does reach the
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
