//! **The sign-in screen, as an owned `Screen`** (restructure spec §13, phase 6 — `ui/login.rs`
//! moved). Plex's own server-rendered QR PNG (fetched by [`crate::auth`], decoded + tinted here)
//! plus the typed short-code fallback, driven by the flow's phase. Scanning the QR on a phone
//! opens plex.tv pre-filled with the pin; the flow's background poll then advances us onward.
//!
//! It has no panel of its own and is not a table: this is the one owned screen in the family with
//! at most a SINGLE focusable control (the failed/stalled/deleted read-out's action pill, or —
//! while the code is simply unscanned for a long time — the QR screen's own "press OK for a new
//! code" sentence). Both are `ElemKind::Bare`: they fire on the OK key-down edge with no hold and
//! no press bounce, exactly as they did under the old `key()` ladder, because `StatusOverlay`'s
//! action never wore the app's tvOS press treatment either (see that widget's own doc).
//!
//! **`step` is the ONLY place this screen ever reads [`crate::auth`].** The QR triple (code,
//! bitmap, generation) has to come off ONE lock or a frame can mix two codes — `auth::QrCode`'s own
//! doc — and `draw`/`prepare` must not each take a second, independent snapshot of a flow another
//! thread can move between them. So every `Tick` refreshes `phase`/`qr_gen`/`qr_code`/`qr_replaced`/
//! `error`/`delete_leftovers` from `crate::auth` exactly once, and every other method — `draw`,
//! `prepare`, `Focusable`'s geometry — reads only those cached fields. This is a stricter rule than
//! `screens::onboard` follows for its own external reads (`crate::browse::…` stays fine to poll live
//! from `draw`), and it is stricter on purpose here: the QR bitmap is a GL upload this screen must
//! not repeat on every frame, and the digits/bitmap/generation triple is the one place in this
//! family where "read it twice" is an observable bug rather than a style question.

use std::ffi::{CStr, CString};
use std::os::raw::c_int;

use crate::auth::{self, Phase};
use crate::ui::frame::Budget;
use crate::ui::label::HAlign;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, Fx, GroupId, Handled, InputEvent, InputKind, Key,
    LogicalState, Machine, Measure,
};
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::screen::{
    Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusSource, FocusTarget,
    Focusable, GroupKind, GroupSpec, HitSource, Hover, Placed, RenderStrategy, Screen,
    ScreenEvent, Seat, Step, Stop,
};
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Spinner, StatusKind, StatusOverlay, BTN_PILL_AIR};
use crate::ui::{theme, Env, Painter, Rect, View};

use super::registry::{word, AppFx, AppLike, LoopReq};

/// The one focusable element this screen ever mints, and the group it lives in — there is never a
/// second, so both are constants rather than an index space. `GroupId(0)` matches the container's
/// own default fresh-mount target (`stack.rs::fresh`), which is what lets a screen that mounts
/// straight into `Phase::Deleted` (a real control from frame one) get seated by the ordinary Mount
/// → Enter sequence with no correction of its own.
const CONTROL: u32 = 0;
const CONTROL_GROUP: GroupId = GroupId(0);

/// How long a working phase runs before the read-out grows a way out.
///
/// **Not zero**: a healthy LAN discovery finishes in well under a second, and a control that
/// flashes past on every sign-in is noise that teaches people to ignore it. **Not longer**: from
/// the sofa a spinner that will never stop looks exactly like one that is about to, and until this
/// existed there was no way at all out of a wedged sign-in — BACK is the root press (see
/// `Machine::step`'s BACK arm), and on a first-ever boot there is no stored session for it to
/// resume, so the only exit was killing the app.
const ESCAPE_AFTER_MS: f32 = 12_000.0;

/// Whether a wait has run long enough to be worth offering an escape from.
///
/// Pure and separate from the draw so the threshold is gradeable on the host.
fn escape_offered(phase_ms: f32) -> bool {
    phase_ms >= ESCAPE_AFTER_MS
}

/// The phases that are genuinely WAITING ON A NETWORK CALL and can therefore stall.
///
/// **An allowlist, not "everything that is not terminal".** It was the latter for an hour, which
/// swept in `Ready`, `Profiles` and `Switching` — phases the main loop routes away from on its next
/// pass. A stalled-wait control answered on one of those would call `auth::retry`/
/// `auth::restart_stalled_wait` on a flow that had already SUCCEEDED, replacing a completed handoff
/// with a fresh sign-in. `Idle` is excluded for the same reason from the other side: nothing is
/// owed, so there is nothing to retry.
fn working_phase(phase: Phase) -> bool {
    matches!(phase, Phase::Creating | Phase::Discovering)
}

/// The verb on both the failed and the stuck read-out, because it is the same call underneath.
///
/// `auth::retry`/`auth::restart_stalled_wait` bump the auth epoch, so a worker still blocked in the
/// wedged request has its result discarded when it finally returns, and it re-runs only the leg
/// that failed — discovery when the pin already yielded an account credential, a whole fresh pin
/// when it did not.
const ESCAPE: &CStr = c"Try again";
const SIGN_IN: &CStr = c"Sign in";

/// How long a QR code may go unscanned before the screen offers to replace it on request.
///
/// **A separate, much longer clock than [`ESCAPE_AFTER_MS`], because this wait is not a stall.**
/// Twelve seconds is right for a spinner that should have finished in one; a code on screen is
/// waiting for a person to find their phone, unlock it, open a camera and tap a link, and nagging
/// them at twelve seconds would be wrong every time. A full minute of a code that has already been
/// scanned is not.
///
/// It exists because the automatic replacement (a pin that runs out is re-minted automatically)
/// cannot cover the case the issue reported: the phone says *Account linked* while our polls are
/// being answered `Pending` or nothing at all, and the person watching knows something the
/// television does not. Waiting out the rest of a fifteen-minute lease is not a recovery.
const QR_ESCAPE_AFTER_MS: f32 = 60_000.0;

/// Whether the QR screen is offering its own replacement right now. Pure, and — like
/// [`escape_offered`] — the ONE predicate behind both the sentence and the key, so a control that
/// is not drawn can never be activated.
fn qr_escape_offered(phase_ms: f32) -> bool {
    phase_ms >= QR_ESCAPE_AFTER_MS
}

/// **What the screen is waiting ON**, as the pair [`LoginScreen::phase_ms`] is timing.
///
/// The phase alone was the whole identity while a code could only change by leaving `Waiting`. It
/// cannot be any more: a pin that runs out is replaced automatically, `Waiting → Creating →
/// Waiting`, and a `Tick` samples once a frame — so a replacement completed between two samples (a
/// paused main loop, a long frame) is invisible, and the FRESH code inherits the dead one's age. It
/// would then offer "press OK for a new code" about a code that had existed for a millisecond.
/// Including the generation makes the reset exact rather than probable.
type Wait = (Phase, u64);

/// Is what the screen is waiting on a DIFFERENT thing from what it was waiting on last frame?
///
/// Trivial, and separate anyway, because the rule it encodes is not: a new CODE restarts the clock
/// exactly as a new PHASE does, and the version that compared phases alone is the one that would
/// offer to replace a code a millisecond old.
fn wait_restarted(seen: Wait, live: Wait) -> bool {
    seen != live
}

/// Release the cached QR texture as soon as it stops describing the code the flow is showing.
///
/// **Keyed on the QR generation, not on the phase, and that swap is this screen's half of issue
/// #30.** The rule used to be "a retry enters `Creating`, so drop it there" — which was true of the
/// only way a code could ever change. It no longer is: a pin that runs out is now replaced
/// automatically, and the flow returns to the same `Waiting` it was already in. A cache keyed on
/// the phase would have gone on drawing the dead code — sharp, scannable, and pointing at a pin
/// plex.tv had forgotten — for the whole of its successor's life. `Creating` is still checked, as
/// the belt to the generation's braces: it is the one moment a flow is known to have thrown its
/// code away before any replacement exists.
fn qr_cache_stale(cached: u64, live: u64, phase: Phase) -> bool {
    cached != live || phase == Phase::Creating
}

/// What the delete actually achieved, as the two lines it is honest to draw.
///
/// **A partial wipe may not be reported as a whole one**, and that is not pedantry: the files this
/// sweep can fail on include the TELEMETRY decision, so a survivor is re-read on the next launch
/// and a consent the user believed they had deleted comes back. The session is gone either way, so
/// the verdict stays true and the reason carries the qualification.
fn deleted_readout(leftovers: usize) -> (&'static CStr, &'static CStr) {
    if leftovers == 0 {
        (
            c"Local data deleted",
            c"Credentials, preferences, telemetry and local diagnostics have been removed.",
        )
    } else {
        (
            c"Signed out, and most local data deleted",
            c"Some files could not be removed and may still be on this television.",
        )
    }
}

/// The line under the code, which has to answer a question that only exists now that a code can be
/// replaced: *why is this not the code I was looking at*.
///
/// A pin lives fifteen minutes and is re-minted when it runs out, so somebody who walked away
/// mid-sign-in — or whose phone has just told them the OLD code was linked — comes back to
/// different digits. Saying nothing there reads as the television having lost track of itself, and
/// it is exactly the moment they need to be told to scan again. **`stalled` outranks
/// `code_replaced`**: one of these sentences carries an ACTION, and a line that explains history is
/// worth less than the one that offers a way forward.
fn waiting_status(code_replaced: bool, stalled: bool) -> &'static CStr {
    if stalled {
        c"Still waiting — press OK for a new code"
    } else if code_replaced {
        c"That code expired — scan this one"
    } else {
        c"Waiting for you to sign in…"
    }
}

/// The complete manual-link stack in the content column.
///
/// The QR itself is centred on the television's Y axis as requested, while the URL, manual code
/// and waiting state flow from its edges. That makes the scan target visually central without
/// separating the fallback credentials into unrelated screen coordinates.
#[derive(Clone, Copy)]
struct QrLayout {
    url: Rect,
    card: Rect,
    code: Rect,
    status: Rect,
}

fn qr_layout(layout: RouteLayout) -> QrLayout {
    const SIDE: f32 = 420.0;
    let card = Rect::new(
        layout.content.cx() - SIDE * 0.5,
        Rect::FULL.cy() - SIDE * 0.5,
        SIDE,
        SIDE,
    );
    let url_h = theme::size::TITLE as f32 + theme::space::XS;
    let code_h = theme::size::DISPLAY as f32 + theme::space::XS;
    let status_h = theme::size::BODY as f32 + theme::space::SM;
    QrLayout {
        url: Rect::new(
            layout.content.x,
            card.y - theme::space::LG - url_h,
            layout.content.w,
            url_h,
        ),
        card,
        code: Rect::new(
            layout.content.x,
            card.y + card.h + theme::space::LG,
            layout.content.w,
            code_h,
        ),
        status: Rect::new(
            layout.content.x,
            card.y + card.h + theme::space::LG + code_h + theme::space::MD,
            layout.content.w,
            status_h,
        ),
    }
}

/// The action pill's rect, reproduced through the [`Measure`] capability rather than
/// `crate::text::text_width`/`text_height` directly — mirrors `widgets::StatusOverlay::bands`/
/// `action_frame` exactly (`TtfMeasure` forwards straight to those two functions, so the numbers
/// agree on a real device) but stays linkable with no font loaded, which is what lets
/// [`Focusable::groups`]/[`Focusable::place`] answer the SAME rect the draw uses instead of a
/// second, drifting copy of it (`ui/table_screen.rs`'s rule: "the DRAW reads the same formula").
/// `StatusOverlay` itself is unchanged and is still what actually PAINTS the pill; this only
/// answers where it painted it.
fn status_action_rect(measure: &dyn Measure, label: &CStr, working: bool, has_reason: bool) -> Rect {
    let frame = Rect::FULL;
    let cy = frame.cy();
    let cap_h = measure.line_h(theme::size::BODY);
    // `Working` straddles the frame centre with the spinner above it; every other kind this screen
    // ever seats a control in (Failed, Deleted, and Working once it has stalled) centres the
    // caption on the frame — `StatusOverlay::bands`'s own asymmetry, ported as-is.
    let cap_y = if working { cy + theme::space::XS } else { cy - cap_h * 0.5 };
    let mut below = cap_y + cap_h;
    if has_reason {
        below += theme::space::SM + measure.line_h(theme::size::CAPTION);
    }
    let action_y = below + theme::space::LG;
    let w = measure.width(label, theme::size::BODY, true) + BTN_PILL_AIR;
    Rect::new(frame.cx() - w * 0.5, action_y, w, StatusOverlay::CTRL_H)
}

/// What the one control (when it exists at all) does when pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ControlKind {
    /// The stalled-wait escape — either the Working spinner's `Try again` (12 s) or the QR
    /// screen's `press OK for a new code` (60 s). Both restart the SAME wait this screen has been
    /// timing (`auth::restart_stalled_wait`), which is why they share one verb here even though
    /// they read different sentences on screen.
    RestartWait,
    /// `Phase::Error`'s `Try again` — acts unconditionally; there is no live worker to race.
    Retry,
    /// `Phase::Deleted`'s `Sign in` — starts a whole fresh flow.
    StartLogin,
}

fn label_for(kind: ControlKind) -> &'static CStr {
    match kind {
        ControlKind::RestartWait | ControlKind::Retry => ESCAPE,
        ControlKind::StartLogin => SIGN_IN,
    }
}

/// Every branch but the two SETTLED read-outs (`Failed`/`Deleted`) draws the spinner — the one
/// thing on this screen that animates from a raw clock (`spin_ms`) rather than a spring `ui::idle`
/// can see on its own. `Spinner::draw`'s own module note is the standing warning that this class of
/// animator ships FROZEN if it forgets to report every frame it is on screen.
fn control_has_spinner(phase: Phase) -> bool {
    !matches!(phase, Phase::Error | Phase::Deleted)
}

fn phase_disc(p: Phase) -> u8 {
    match p {
        Phase::Idle => 0,
        Phase::Creating => 1,
        Phase::Waiting => 2,
        Phase::Discovering => 3,
        Phase::Profiles => 4,
        Phase::Switching => 5,
        Phase::Ready => 6,
        Phase::Error => 7,
        Phase::Deleted => 8,
    }
}

struct LoginState {
    phase: u8,
    qr_gen: u64,
    qr_replaced: bool,
    has_control: bool,
    delete_leftovers: u32,
}

impl LogicalState for LoginState {
    fn write(&self, w: &mut Canon) {
        w.u8(self.phase);
        w.u64(self.qr_gen);
        w.bool(self.qr_replaced);
        w.bool(self.has_control);
        w.u32(self.delete_leftovers);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "login phase={} qr_gen={} replaced={} control={} leftovers={}",
            self.phase, self.qr_gen, self.qr_replaced, self.has_control, self.delete_leftovers
        ));
    }
}

pub(crate) struct LoginScreen {
    entry: EntryId,
    /// Free-running rotation clock for the spinner. Render-only, never hashed.
    spin_ms: f32,
    /// How long the CURRENT wait has been on screen — reset by `tick` whenever [`Wait`] changes.
    phase_ms: f32,
    wait: Wait,
    /// The uploaded GL texture of Plex's QR PNG (0 until decoded+uploaded) and which generation it
    /// describes. Render resources only — built and freed in [`Screen::prepare`], never in `step`,
    /// so a host test driving `step` alone never touches GL.
    qr_tex: u32,
    qr_tex_gen: u64,
    /// The pixel size that texture was uploaded at, so [`Screen::render_report`] can state the
    /// bytes this screen holds of the frame's render residency (§8.3) instead of guessing them:
    /// the bitmap is whatever plex.tv's PNG decoded to, not a constant. `(0, 0)` while `qr_tex`
    /// is 0, and the two are set and cleared together.
    qr_px: (u32, u32),
    /// PNG bytes `tick` captured this frame, awaiting [`Screen::prepare`]'s decode+upload — the
    /// hand-off between "read `crate::auth` once" (step) and "touch GL" (prepare). `None` once
    /// consumed, or when nothing new has been published.
    qr_png_pending: Option<(u64, Vec<u8>)>,
    /// Everything this screen ever reads from [`crate::auth`], refreshed exactly once per `Tick` —
    /// see the module doc for why `draw`/`prepare` may not read `crate::auth` themselves.
    phase: Phase,
    qr_gen: u64,
    qr_code: String,
    qr_replaced: bool,
    error: String,
    delete_leftovers: usize,
    ground: RouteGround,
    state: LoginState,
}

impl LoginScreen {
    pub(crate) fn new(entry: EntryId) -> Self {
        let mut s = Self {
            entry,
            spin_ms: 0.0,
            phase_ms: 0.0,
            wait: (Phase::Idle, 0),
            qr_tex: 0,
            qr_tex_gen: 0,
            qr_px: (0, 0),
            qr_png_pending: None,
            phase: Phase::Idle,
            qr_gen: 0,
            qr_code: String::new(),
            qr_replaced: false,
            error: String::new(),
            delete_leftovers: 0,
            ground: RouteGround::new(),
            state: LoginState {
                phase: 0,
                qr_gen: 0,
                qr_replaced: false,
                has_control: false,
                delete_leftovers: 0,
            },
        };
        // Read once at construction — not a `draw`-time poll, a one-time seed exactly mirroring
        // `ui/login.rs`'s own `Scene::new`/`init`, which read `auth::qr_generation()` the same way.
        // Without it, a mount that lands straight in `Phase::Deleted` (Settings' "Delete all local
        // data", confirmed) would draw its FIRST frame from the constructor's own placeholder
        // `Phase::Idle` — the wrong branch — because the container's default `Enter` (and this
        // screen's own first `draw`) both run before this screen's first `Tick` ever could.
        s.wait = s.resync();
        s
    }

    /// Refresh every field this screen caches from [`crate::auth`] — see the struct's own doc for
    /// why this is the ONLY place any of them is read. Called once at construction and again on
    /// every `Tick`.
    ///
    /// **Returns the `(phase, qr_generation)` pair it just sampled**, so a caller that ALSO needs
    /// to know what changed — `tick`'s own wait-restart clock — reads the exact values this method
    /// cached instead of asking `crate::auth` a second time. `tick` used to do exactly that: it
    /// computed `let live: Wait = (auth::phase(), auth::qr_generation());` several lines before
    /// calling `resync`, which then read the identical pair out of `crate::auth` again on its own.
    /// That is precisely the "read it twice" bug the module doc calls out by name — a retry (or a
    /// pin running out and being replaced) landing in the gap between the two independent reads
    /// could hand the wait-restart clock a phase that disagreed with the one this method cached,
    /// so the clock could reset (or fail to reset) against a `Wait` that was never actually drawn.
    /// Threading the sample through the return value makes the two agree by construction.
    fn resync(&mut self) -> Wait {
        self.phase = auth::phase();
        self.qr_gen = auth::qr_generation();
        if self.phase == Phase::Waiting {
            // ONE read of the code, generation and bitmap together — `auth::QrCode`'s own doc says
            // why: taking them separately can mix the new digits with the old bitmap, or cache a
            // fresh bitmap under a stale generation.
            let qr = auth::qr_snapshot();
            self.qr_code = qr.code;
            self.qr_replaced = qr.replaced;
            self.qr_png_pending = Some((qr.generation, qr.png));
        }
        if self.phase == Phase::Error {
            self.error = auth::error();
        }
        if self.phase == Phase::Deleted {
            // `app::input::delete_all_local_data_and_sign_out` calls `auth::note_delete_leftovers`
            // (moved off `ui::login`'s static onto `auth`'s own — see that setter's doc), and this
            // reads the matching `auth::delete_leftovers()` getter. It has to come off a shared
            // static rather than a constructor parameter: this screen is reconstructed fresh every
            // time the route re-enters Login (`AppMounter::mount`), so nothing else ever holds a
            // live `LoginScreen` instance to push the count onto before its first frame draws, and
            // the frozen contract fixes `LoginScreen::new(EntryId)`'s signature. Reading the SAME
            // static the setter already writes is the smallest connection between the two that
            // needs no other lane's file.
            self.delete_leftovers = crate::auth::delete_leftovers();
        }
        self.state = LoginState {
            phase: phase_disc(self.phase),
            qr_gen: self.qr_gen,
            qr_replaced: self.qr_replaced,
            has_control: self.has_control(),
            delete_leftovers: self.delete_leftovers as u32,
        };
        (self.phase, self.qr_gen)
    }

    fn tick<H: AppLike>(&mut self, dt: f32, fx: &mut Effects<'_, H>) {
        self.spin_ms += dt * 1000.0;
        let had_control = self.has_control();

        // ONE sample of `crate::auth` feeds both the wait-restart clock below and every cached
        // field `resync` publishes — see `resync`'s own doc, and the module doc's "read it twice
        // is an observable bug" rule. This used to read `(auth::phase(), auth::qr_generation())`
        // again independently right here, a few lines before calling `resync`, which read the
        // identical pair a second time on its own; a retry (or an automatic pin replacement)
        // landing in the gap between the two reads could disagree with itself within one tick.
        let live: Wait = self.resync();

        // Each wait gets its own clock. A flow that walks Creating → Waiting → Discovering is
        // making progress, and restarting the timer at every step is what stops a slow-but-healthy
        // sign-in from being offered a way out of itself.
        if wait_restarted(self.wait, live) {
            self.wait = live;
            self.phase_ms = 0.0;
        } else {
            self.phase_ms += dt * 1000.0;
        }

        if self.has_control() && !had_control {
            // The control just appeared (a stalled wait grew its escape, or a phase moved straight
            // to `Error`) — seat focus on it now. The container's default `Enter` already ran, at
            // mount time, against whatever `groups()` answered THEN; nothing else will ever ask
            // the engine to look again unless this screen does, exactly the correction
            // `screens::onboard`'s first-run constructor makes for the same underlying reason
            // (that module's own doc has the longer argument for why a REACTION is the right shape
            // rather than a second write at some earlier point).
            let me = fx.from();
            fx.push(Fx::Deliver(
                me,
                Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::ContainerGroup(CONTROL_GROUP),
                })),
            ));
        }
        if control_has_spinner(self.phase) {
            fx.note(crate::ui::present::PresentEvent::Motion);
        }
    }

    fn control_kind(&self) -> Option<ControlKind> {
        match self.phase {
            Phase::Deleted => Some(ControlKind::StartLogin),
            Phase::Error => Some(ControlKind::Retry),
            Phase::Waiting if qr_escape_offered(self.phase_ms) => Some(ControlKind::RestartWait),
            p if working_phase(p) && escape_offered(self.phase_ms) => Some(ControlKind::RestartWait),
            _ => None,
        }
    }

    fn has_control(&self) -> bool {
        self.control_kind().is_some()
    }

    /// The one control's rect — shared verbatim by `draw`'s `Stop` and every `Focusable` query, so
    /// the two can never drift apart (`ui/table_screen.rs`'s rule).
    fn control_rect(&self, measure: &dyn Measure) -> Rect {
        if self.phase == Phase::Waiting {
            // The QR screen's escape is a SENTENCE, not a button (see `waiting_status`'s doc), so
            // its geometry is the status line's own rect rather than a computed pill.
            return qr_layout(RouteLayout::screen()).status;
        }
        let Some(kind) = self.control_kind() else {
            // Never actually reached while nothing is offered — `groups`/`place` gate on
            // `has_control()` first — kept as a documented, harmless fallback rather than a panic
            // a future caller outside this file could trip.
            return Rect::FULL;
        };
        let working = working_phase(self.phase);
        let has_reason = match kind {
            ControlKind::RestartWait => true, // Working's own stall reason is unconditional once offered
            ControlKind::Retry => !self.error.is_empty(),
            ControlKind::StartLogin => true, // `deleted_readout` always states one
        };
        status_action_rect(measure, label_for(kind), working, has_reason)
    }

    /// The control, pressed. Calls straight into `crate::auth`, exactly as `ui/login.rs`'s `key()`
    /// did — these are synchronous, void-returning controller calls, not effects the loop performs
    /// on this screen's behalf, so there is no `Fx` for them to travel through.
    fn activate(&mut self) {
        match self.control_kind() {
            Some(ControlKind::StartLogin) => auth::start_login(),
            Some(ControlKind::Retry) => auth::retry(),
            Some(ControlKind::RestartWait) => {
                // "requested", not "restarted": the press may still be refused (the flow moved on
                // between this screen's last `Tick` and this key), and the event log is the one
                // place that failure is read from — a claim it did something is exactly the wrong
                // thing to have written there.
                crate::log("login: user requested a restart of a stalled sign-in");
                if auth::restart_stalled_wait(self.wait) {
                    // The restart usually re-enters the phase it just left (a stalled `Creating`
                    // starts another `Creating`), and `tick` only zeroes the clock when the wait's
                    // IDENTITY changes — a fresh code changes it, a re-entered phase may not — so
                    // without this the new attempt could inherit the dead one's age and show its
                    // way out immediately.
                    self.phase_ms = 0.0;
                }
            }
            None => {}
        }
    }

    fn draw_readout<H: AppLike>(
        &self,
        f: &mut DrawFrame<'_, '_, H>,
        p: Painter,
        env: &Env,
        caption: &CStr,
        kind: StatusKind,
        reason: Option<&CStr>,
        action: Option<&'static CStr>,
        focused: bool,
    ) {
        let mut o = StatusOverlay::new(Rect::FULL, caption, kind).phase(self.spin_ms as u32);
        if let Some(r) = reason {
            o = o.reason(r);
        }
        if let Some(a) = action {
            o = o.action(a).focused(focused);
        }
        o.draw(env, p);
        if let Some(a) = action {
            let rect = status_action_rect(f.measure, a, kind == StatusKind::Working, reason.is_some());
            f.stop(
                p,
                Stop {
                    key: crate::ui::machine::FocusKey { entry: self.entry, elem: CONTROL },
                    rect,
                    rest_rect: rect,
                    clip: Rect::FULL,
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
    }

    fn draw_working<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, env: &Env, msg: &str, focused: bool) {
        let caption = CString::new(msg).unwrap_or_default();
        let stuck = self.has_control();
        self.draw_readout(
            f,
            p,
            env,
            &caption,
            StatusKind::Working,
            // The reason arrives WITH the control, and only then: it exists to explain why a
            // button just appeared under a spinner that was doing fine a moment ago.
            stuck.then_some(c"This is taking longer than usual."),
            stuck.then_some(ESCAPE),
            focused,
        );
    }

    fn draw_failed<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, env: &Env, focused: bool) {
        let reason = CString::new(self.error.clone()).unwrap_or_default();
        self.draw_readout(
            f,
            p,
            env,
            c"Couldn\u{2019}t sign in",
            StatusKind::Failed,
            (!reason.is_empty()).then_some(reason.as_c_str()),
            Some(ESCAPE),
            focused,
        );
    }

    /// **Empty, not Failed.** Deleting everything is a completed action the user asked for, so it
    /// must not wear the danger tint — the same distinction `StatusKind::Empty` carries for a
    /// library with nothing in it. A partial one is still not a FAILURE either: what it did do, it
    /// did.
    fn draw_deleted<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter, env: &Env, focused: bool) {
        let (verdict, reason) = deleted_readout(self.delete_leftovers);
        self.draw_readout(f, p, env, verdict, StatusKind::Empty, Some(reason), Some(SIGN_IN), focused);
    }

    /// **Deliberately takes no `focused` parameter, unlike its three siblings above.**
    /// `draw_readout` (shared by `draw_working`/`draw_failed`/`draw_deleted`) paints its action as
    /// a real `Button` face, whose fill genuinely differs when focused — legacy hardcoded
    /// `.focused(true)` there because "the only control on the screen … holds focus by
    /// construction"; the owned screen instead threads the engine's actual focus state through,
    /// which happens to answer the same `true` here since there is still nowhere else focus could
    /// be. The QR screen's 60-second escape is not that kind of control: it is a SENTENCE (the
    /// module doc's own word for it), drawn as plain text beside a spinner with no button chrome
    /// for a `focused` bool to vary — legacy's own `draw_waiting` (what this ports) never took one
    /// either, for the identical reason, so this is not a dropped behaviour. Giving this sentence a
    /// focus-dependent look would be a NEW affordance, and a visual one belongs in front of
    /// `ui/CLAUDE.md`'s own design review, not slipped in unreviewed by a bug-fix pass.
    fn draw_waiting<H: AppLike>(&self, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
        let layout = RouteLayout::screen();
        layout.draw_narrative(
            p,
            None,
            "Sign in to Plex",
            "Use your phone camera to scan the code, or link this television manually with the address and code shown here.",
            theme::size::LABEL,
        );
        let right = qr_layout(layout);

        TextView::new("plex.tv/link", theme::size::TITLE, theme::TEXT_HEADING)
            .bold()
            .h(HAlign::Center)
            .draw(p, right.url);

        // QR on a bright card (the white border is the scan quiet-zone). Plex's own PNG → we just
        // show it.
        let card = right.card;
        p.rrect(card, 24.0, 24.0, theme::SURFACE_QR_PLATE);
        if self.qr_tex != 0 {
            let pad = 30.0;
            let inner = Rect::new(card.x + pad, card.y + pad, card.w - 2.0 * pad, card.h - 2.0 * pad);
            // Plex's PNG is WHITE modules on a transparent ground; tint black so the modules render
            // dark on the white card (the transparent ground shows the card) → a scannable
            // black-on-white QR.
            p.tex(self.qr_tex, inner, 0.0, theme::scrim_black(1.0));
        } else {
            Spinner::new(card.x + card.w * 0.5, card.y + card.h * 0.5, 22.0)
                .phase(self.spin_ms as u32)
                .tint(theme::scrim_black(0.5))
                .draw(&Env::inert(), p);
        }

        // The manual code and waiting state remain in the same right-column stack as the URL and
        // QR. Both use couch-readable type rungs; this is an alternative sign-in path, not fine
        // print.
        if let Ok(code) = CString::new(self.qr_code.to_uppercase()) {
            p.text(
                code.as_ptr(),
                right.code.cx(),
                right.code.y,
                theme::size::DISPLAY,
                theme::TEXT_PRIMARY,
                1,
                1,
            );
        }

        let wr = 15.0;
        let wy = right.status.cy();
        let escaping = qr_escape_offered(self.phase_ms);
        let status = waiting_status(self.qr_replaced, escaping);
        let status_w = crate::text::text_width(status.as_ptr(), theme::size::BODY, 0);
        let sx = right.status.cx() - (wr * 2.0 + theme::space::SM + status_w) * 0.5;
        Spinner::new(sx + wr, wy, wr)
            .phase(self.spin_ms as u32)
            .tint(theme::TEXT_SECONDARY)
            .draw(&Env::inert(), p);
        let ty = crate::text::text_vcenter_y(theme::size::BODY, 0, wy);
        p.text(
            status.as_ptr(),
            sx + wr * 2.0 + theme::space::SM,
            ty,
            theme::size::BODY,
            theme::TEXT_SECONDARY,
            0,
            0,
        );

        if escaping {
            f.stop(
                p,
                Stop {
                    key: crate::ui::machine::FocusKey { entry: self.entry, elem: CONTROL },
                    rect: right.status,
                    rest_rect: right.status,
                    clip: Rect::FULL,
                    hover: Hover::Focus,
                    activate: Activate::Direct,
                },
            );
        }
    }

    /// Build+free the QR texture. GL only, called from [`Screen::prepare`] — never from `step`, so
    /// a host test driving `Machine::step` alone never touches it.
    fn prepare_qr_tex(&mut self) {
        self.drop_stale_qr_tex(self.qr_gen);
        let Some((gen, png)) = self.qr_png_pending.take() else {
            return;
        };
        // The belt to the drop above's braces: a code replaced again between the `Tick` that
        // captured this PNG and this `prepare` (rare — it needs two replacements inside one frame)
        // must not upload a bitmap for a code that has already died.
        self.drop_stale_qr_tex(gen);
        if self.qr_tex != 0 || png.is_empty() {
            return;
        }
        let (mut w, mut h): (c_int, c_int) = (0, 0);
        let px = crate::img::img_decode_rgba(png.as_ptr(), png.len() as c_int, &mut w, &mut h);
        if !px.is_null() {
            self.qr_tex = crate::img::img_upload_rgba(px, w, h);
            self.qr_px = if self.qr_tex != 0 { (w.max(0) as u32, h.max(0) as u32) } else { (0, 0) };
            self.qr_tex_gen = gen;
            crate::img::img_free(px);
        }
    }

    fn drop_stale_qr_tex(&mut self, live: u64) {
        if !qr_cache_stale(self.qr_tex_gen, live, self.phase) {
            return;
        }
        crate::gfx::delete_tex(self.qr_tex);
        self.qr_tex = 0;
        self.qr_px = (0, 0);
        self.qr_tex_gen = live;
    }
}

impl<H: AppLike> Focusable<H> for LoginScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if !self.has_control() {
            return;
        }
        out.push(GroupSpec {
            id: CONTROL_GROUP,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            // Nothing else is ever focusable on this screen, so every direction simply stays put
            // rather than searching for a sibling group that does not exist.
            edge: [EdgeRule::Stop; 4],
            extent: self.control_rect(cx.measure),
            len: 1,
            elem: ElemKind::Bare,
        });
    }
    fn group_of(&self, key: &u32, _cx: &Cx<'_, H>) -> Option<GroupId> {
        (self.has_control() && *key == CONTROL).then_some(CONTROL_GROUP)
    }
    fn neighbour(&self, _key: crate::ui::machine::FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, H>) -> Step<u32> {
        Step::Edge
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        if !self.has_control() || *key != CONTROL {
            return None;
        }
        let rect = self.control_rect(cx.measure);
        Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
    }
    fn reconcile(&self, want: crate::ui::machine::FocusKey<u32>, _cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        want
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::machine::FocusKey { entry: self.entry, elem: CONTROL }
    }
}

impl<H: AppLike> Machine<H> for LoginScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, _cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(t.dt(), fx);
                Handled::Yes
            }
            ScreenEvent::Activate(_) => {
                self.activate();
                fx.invalidate(crate::ui::present::Provenance::Input);
                Handled::Yes
            }
            // **This screen has no panel of its own to close first, so every BACK here is the
            // root press** — the same one Home's is, for the same reason (nothing of this app is
            // behind a first-ever sign-in). It is `LoopReq::AuthBackAtRoot`, not the plain
            // `BackAtRoot` (`screens::consent`'s first stage) reaches for: `auth::cancel` has to
            // decide FIRST whether there is a stored session to fall back into or the television's
            // own Home is the answer instead, and that decision — together with the shared
            // `webos::take_root_press` cooldown every root in this app shares — belongs to the LOOP
            // (`app::input::login_or_profiles_root_back`, performed from this request), so this
            // screen only ever ASKS for it. `AuthBackAtRoot`'s own doc has the full account of why
            // it is a distinct variant rather than a payload on `BackAtRoot`.
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. },
                ..
            }) => {
                fx.push(Fx::App(AppFx::Loop(LoopReq::AuthBackAtRoot)));
                Handled::Yes
            }
            // **The QR texture has to be freed exactly here, and `Unmount` is the one event this
            // screen is guaranteed to see before it dies.** `AppMounter::mount` builds a brand new
            // `LoginScreen` on every `Replace` onto `Route::Login` (`app/bridge.rs:275`), so every
            // sign-in retry, sign-out and "Delete all local data" round trip mints a fresh
            // instance — and, before this arm existed, orphaned the outgoing one's GL texture on
            // every one of those visits: nothing ever deleted `qr_tex` for an instance that was
            // simply going away rather than replacing its OWN code. This is the same failure the
            // legacy single-`Scene` screen already had to name once — `ui/login.rs`'s
            // `drop_a_stale_qr` doc: "zeroing the handle alone orphaned a full 400x400-ish RGBA QR
            // bitmap per sign-in retry, with nothing left holding its id to free it later" — except
            // that comment guarded a re-upload inside one long-lived `Scene` across retries, which
            // never covered a whole INSTANCE being torn down, since the legacy screen had no such
            // thing. Freeing it in `Screen::prepare` (where the struct's own field doc says GL
            // resources are otherwise built and freed) is not an option: an unmounting instance
            // gets no further `prepare` call to do it in, so the free has to run wherever teardown
            // is actually observed, which for an owned `Screen` is here. `gfx::delete_tex` no-ops
            // on 0, so a screen that never uploaded a texture (never reached `Phase::Waiting`, or
            // reached it with no QR PNG yet) frees nothing.
            ScreenEvent::Unmount => {
                crate::gfx::delete_tex(self.qr_tex);
                self.qr_tex = 0;
                self.qr_px = (0, 0);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: AppLike> Screen<H> for LoginScreen {
    fn name(&self) -> &'static str {
        word::LOGIN
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        // This is one of the family's three routes with nowhere for BACK to go INSIDE the app
        // (`ui/CLAUDE.md`'s route-family rule) — see this screen's BACK arm.
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {
        self.prepare_qr_tex();
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter;
        // The QR screen is the first thing a new user sees, before Home has any artwork to lend
        // it. `Painter::root()`, not `p`, for the ground — the ambient wash must not ride whatever
        // page-transition cascade this screen's own painter carries, exactly as
        // `screens::onboard`'s first-run mounting draws its own ground the same way.
        self.ground.draw_default(Painter::root());
        let env = Env::inert();
        let focused = f.focus.current.map(|k| k.elem) == Some(CONTROL);

        match self.phase {
            Phase::Waiting => self.draw_waiting(f, p),
            Phase::Error => self.draw_failed(f, p, &env, focused),
            Phase::Deleted => self.draw_deleted(f, p, &env, focused),
            Phase::Discovering => self.draw_working(f, p, &env, "Finding your server\u{2026}", focused),
            _ => self.draw_working(f, p, &env, "Connecting to Plex\u{2026}", focused),
        }
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    /// The QR bitmap is the one render this screen owns (§8.3 rule (c)) — its own `upload_rgba` in
    /// [`Self::prepare_qr_tex`], its own `delete_tex` on `Unmount`. Everything else here is drawn
    /// immediate-mode or comes from a shared cache. The size is whatever plex.tv's PNG decoded to,
    /// which is why it is recorded rather than assumed.
    fn render_report(&self) -> crate::ui::frame::RenderReport {
        if self.qr_tex == 0 {
            return crate::ui::frame::RenderReport::NONE;
        }
        crate::ui::frame::RenderReport::one(self.qr_px.0, self.qr_px.1)
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    // **A click only lands inside the one control's own rect, never anywhere else on the screen —
    // a deliberate departure from the pre-phase-6 behaviour, not an oversight.** The legacy loop's
    // `Route::Login` click arm (`app/run.rs`, before 957bdc4d) fired
    // `crate::ui::login::key(SDLK_RETURN, 0)` on ANY click anywhere on this route, because that
    // screen predates real per-widget hit testing and "one actionable thing on the login screen"
    // was reason enough to skip building it; `key()` itself still gated on whether a control was
    // actually offered, so the only visible effect was that a tap on the QR image, the URL, or
    // empty space fired Retry/Sign-in/the stalled-wait escape whenever one happened to be showing.
    // That is exactly the failure mode real hit testing exists to rule out everywhere else in this
    // family (a tap near a poster's shadow must not activate a card three rows away).
    // `HitSource::Engine` resolves a click against `Focusable::place`'s own rect like every other
    // owned screen, so a tap has to land on the pill (or, on the QR screen, the status sentence)
    // to do anything. Restoring click-anywhere would be a step backward, not a port.
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::consts::inside_safe;
    use crate::ui::machine::{FocusRead, InputOwner, InstanceId, MachineId, PressRead, Source, Stamped};
    use crate::ui::present::Present;

    use super::super::family::InnerHost;

    /// **A partial wipe may not be reported as a whole one.** The sweep's candidate lists span
    /// both webOS install prefixes and the jail profiles disagree about which are writable, so a
    /// survivor is ordinary — and the survivor can be the TELEMETRY decision, which is then
    /// re-read on the next launch. Saying "telemetry has been removed" over that is the one
    /// sentence on this screen that could be actively false. Ported verbatim from `ui/login.rs`.
    #[test]
    fn a_partial_wipe_does_not_claim_a_whole_one() {
        let (whole, whole_why) = deleted_readout(0);
        let (partial, partial_why) = deleted_readout(2);
        assert_ne!(whole, partial);
        assert!(whole_why.to_bytes().windows(9).any(|w| w == b"telemetry"));
        assert!(
            !partial_why.to_bytes().windows(9).any(|w| w == b"telemetry"),
            "a partial wipe must not name what it may have failed to delete"
        );
        assert!(
            partial.to_bytes().windows(10).any(|w| w == b"Signed out"),
            "…but it still states what it DID do: the session is gone either way"
        );
    }

    /// **A stalled sign-in has to be escapable, and until 2026-09-02 it was not.** The control
    /// appears on a clock, so the whole rule is a pure predicate.
    ///
    /// **Pins the DESIGN NUMBER itself, not just the `>=` operator.** Every boundary used to be
    /// phrased in terms of `ESCAPE_AFTER_MS` (`ESCAPE_AFTER_MS - 1.0`, `ESCAPE_AFTER_MS`), so a
    /// mutation that changed the constant's value — the reviewer tried `120.0` — left this test
    /// green regardless: it was grading the comparison, not the twelve seconds. The literal
    /// `assert_eq!` and the literal millisecond boundaries below close that gap.
    #[test]
    fn a_wait_that_stops_looking_normal_grows_a_way_out() {
        assert_eq!(ESCAPE_AFTER_MS, 12_000.0, "the documented twelve-second design number");
        assert!(!escape_offered(0.0), "a fresh wait offers nothing");
        assert!(
            !escape_offered(11_999.0),
            "nor does a healthy one — a button that flashes past teaches people to ignore it"
        );
        assert!(escape_offered(12_000.0));
    }

    /// **The escape belongs ONLY to the two phases that wait on a network call.** Ported verbatim.
    #[test]
    fn only_a_phase_waiting_on_the_network_can_be_stalled() {
        assert!(working_phase(Phase::Creating));
        assert!(working_phase(Phase::Discovering));
        for settled in [
            Phase::Idle,
            Phase::Waiting,
            Phase::Profiles,
            Phase::Switching,
            Phase::Ready,
            Phase::Error,
            Phase::Deleted,
        ] {
            assert!(
                !working_phase(settled),
                "{settled:?} is not a wait this screen may offer to restart"
            );
        }
    }

    /// **A code that has been replaced may not go on being drawn**, which is the login screen's
    /// half of issue #30. Ported verbatim.
    #[test]
    fn a_replaced_code_invalidates_the_cached_qr_even_without_a_phase_change() {
        assert!(
            qr_cache_stale(4, 5, Phase::Waiting),
            "a new code was published while the screen never left Waiting"
        );
        assert!(
            !qr_cache_stale(5, 5, Phase::Waiting),
            "…and the settled case must not re-upload a texture every frame"
        );
        assert!(qr_cache_stale(5, 5, Phase::Creating));
    }

    /// The one line under the code has to explain a swap the user did not ask for. Ported
    /// verbatim.
    #[test]
    fn a_swapped_code_says_so_rather_than_changing_under_the_user() {
        let says = |s: &CStr, word: &[u8]| s.to_bytes().windows(word.len()).any(|w| w == word);
        assert!(says(waiting_status(false, false), b"Waiting"));
        assert!(
            says(waiting_status(true, false), b"expired"),
            "it names what happened; a code that simply changes reads as a fault"
        );
        assert!(says(waiting_status(true, true), b"press OK"));
        assert!(says(waiting_status(false, true), b"press OK"));
    }

    /// **The QR screen's clock is not the spinner's, and it must not be.**
    ///
    /// **Pins both DESIGN NUMBERS, not just their ratio.** The relative check
    /// (`QR_ESCAPE_AFTER_MS > ESCAPE_AFTER_MS * 4.0`) survives mutating BOTH constants together —
    /// the reviewer's example, `ESCAPE_AFTER_MS = 120.0` and `QR_ESCAPE_AFTER_MS = 600.0`, still
    /// satisfies `600 > 120 * 4`. The `assert_eq!`s and the literal millisecond boundaries below
    /// pin sixty seconds and twelve seconds as VALUES, so that mutation fails here even though the
    /// ratio it preserves would not have caught it.
    #[test]
    fn the_qr_screen_offers_a_new_code_on_a_much_longer_clock_than_a_stalled_spinner() {
        assert_eq!(ESCAPE_AFTER_MS, 12_000.0);
        assert_eq!(QR_ESCAPE_AFTER_MS, 60_000.0);
        assert!(QR_ESCAPE_AFTER_MS > ESCAPE_AFTER_MS * 4.0);
        assert!(!qr_escape_offered(0.0));
        assert!(
            !qr_escape_offered(12_000.0),
            "a sign-in that is merely twelve seconds old is going fine"
        );
        assert!(
            QR_ESCAPE_AFTER_MS < 900_000.0,
            "…and it must arrive well inside a code's own fifteen-minute life, or it is not a \
             recovery from anything"
        );
        assert!(!qr_escape_offered(59_999.0));
        assert!(qr_escape_offered(60_000.0));
    }

    /// **A new code starts a new clock, even if the phase change between them was never sampled.**
    /// Ported verbatim.
    #[test]
    fn a_replaced_code_restarts_the_wait_even_when_the_phase_never_appeared_to_change() {
        let old_code: Wait = (Phase::Waiting, 7u64);
        assert!(
            !wait_restarted(old_code, (Phase::Waiting, 7)),
            "the same code in the same phase is the same wait, and the clock must keep running"
        );
        assert!(
            wait_restarted(old_code, (Phase::Waiting, 8)),
            "a new code is a new wait, whatever the phase appeared to do in between — this is \
             the case a phase-only comparison misses, and it hands a one-millisecond-old code \
             its predecessor's sixty seconds"
        );
        assert!(
            wait_restarted(old_code, (Phase::Discovering, 7)),
            "…and the original rule still holds: a step forward is a fresh wait"
        );
    }

    /// Ported verbatim.
    #[test]
    fn qr_is_vertically_centred_and_the_whole_link_stack_stays_in_the_right_column() {
        let route = RouteLayout::screen();
        let q = qr_layout(route);
        assert_eq!(q.card.cy(), Rect::FULL.cy());
        for r in [q.url, q.card, q.code, q.status] {
            assert!(r.x >= route.content.x);
            assert!(r.x + r.w <= route.content.x + route.content.w);
            assert!(inside_safe(r));
        }
    }

    /// A bare screen, built with NO read of `crate::auth` at all — for testing [`ControlKind`]'s
    /// decision and the `Focusable` geometry in complete isolation from the process-global auth
    /// controller (which `LoginScreen::new` deliberately reads, and which other tests running
    /// concurrently in this binary may have left in an arbitrary state).
    fn bare_screen(phase: Phase, phase_ms: f32) -> LoginScreen {
        LoginScreen {
            entry: EntryId(0),
            spin_ms: 0.0,
            phase_ms,
            wait: (phase, 0),
            qr_tex: 0,
            qr_tex_gen: 0,
            qr_px: (0, 0),
            qr_png_pending: None,
            phase,
            qr_gen: 0,
            qr_code: String::new(),
            qr_replaced: false,
            error: String::new(),
            delete_leftovers: 0,
            ground: RouteGround::new(),
            state: LoginState { phase: 0, qr_gen: 0, qr_replaced: false, has_control: false, delete_leftovers: 0 },
        }
    }

    /// **The one property this port exists to protect**: a control that is not drawn must not be
    /// activatable, restated for the owned screen as "a control exists only in exactly the phases
    /// `ui/login.rs`'s `key()` ladder ever acted on". `escape_ready`/`qr_escape_ready`'s old bodies
    /// are gone; this is the test that pins the same property against their replacement,
    /// `LoginScreen::control_kind`.
    ///
    /// **The boundaries below are literal milliseconds, not `ESCAPE_AFTER_MS`/`QR_ESCAPE_AFTER_MS`
    /// themselves.** Feeding a threshold its OWN constant as input proves only that `control_kind`
    /// compares two copies of the same symbol — it stays green under any value that constant takes,
    /// including the reviewer's mutated `ESCAPE_AFTER_MS = 120.0`. Literal `11_999.0`/`12_000.0` and
    /// `59_999.0`/`60_000.0` pin the twelve- and sixty-second design numbers themselves, matching
    /// the two tests above that pin the same constants directly.
    #[test]
    fn the_control_exists_only_where_legacy_ever_acted_on_a_press() {
        assert_eq!(bare_screen(Phase::Waiting, 0.0).control_kind(), None);
        assert_eq!(bare_screen(Phase::Waiting, 59_999.0).control_kind(), None);
        assert_eq!(
            bare_screen(Phase::Waiting, 60_000.0).control_kind(),
            Some(ControlKind::RestartWait)
        );
        assert_eq!(bare_screen(Phase::Creating, 0.0).control_kind(), None);
        assert_eq!(bare_screen(Phase::Creating, 11_999.0).control_kind(), None);
        assert_eq!(
            bare_screen(Phase::Creating, 12_000.0).control_kind(),
            Some(ControlKind::RestartWait)
        );
        assert_eq!(bare_screen(Phase::Discovering, 0.0).control_kind(), None);
        assert_eq!(bare_screen(Phase::Discovering, 11_999.0).control_kind(), None);
        assert_eq!(
            bare_screen(Phase::Discovering, 12_000.0).control_kind(),
            Some(ControlKind::RestartWait)
        );
        // Error and Deleted offer their control unconditionally — no clock involved.
        assert_eq!(bare_screen(Phase::Error, 0.0).control_kind(), Some(ControlKind::Retry));
        assert_eq!(bare_screen(Phase::Deleted, 0.0).control_kind(), Some(ControlKind::StartLogin));
        // The allowlist `working_phase` states explicitly: a phase the main loop is about to route
        // away from must never grow an escape that could call `auth::retry` on a flow that already
        // succeeded.
        for settled in [Phase::Idle, Phase::Profiles, Phase::Switching, Phase::Ready] {
            assert_eq!(bare_screen(settled, 1_000_000.0).control_kind(), None, "{settled:?}");
        }
    }

    fn test_cx<'a>(m: &'a crate::ui::fixture::FixtureMeasure) -> Cx<'a, InnerHost> {
        Cx {
            views: (),
            tick: crate::ui::machine::Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead { current: None , ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    /// The focus GROUP exists exactly when [`LoginScreen::has_control`] does, and its one element
    /// is `ElemKind::Bare` — the escape acts on the key-down edge with no hold and no press dip,
    /// like every read-out action in this family, never a `Control`/`Card`.
    #[test]
    fn the_control_group_exists_only_while_a_control_is_offered() {
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m);

        let quiet = bare_screen(Phase::Waiting, 0.0);
        let mut groups = Vec::new();
        Focusable::<InnerHost>::groups(&quiet, &cx, &mut groups);
        assert!(groups.is_empty(), "nothing to press while the code is fresh");
        assert!(Focusable::<InnerHost>::place(&quiet, &CONTROL, &cx, At::Drawn).is_none());
        assert!(Focusable::<InnerHost>::group_of(&quiet, &CONTROL, &cx).is_none());

        let stuck = bare_screen(Phase::Waiting, QR_ESCAPE_AFTER_MS);
        groups.clear();
        Focusable::<InnerHost>::groups(&stuck, &cx, &mut groups);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].elem, ElemKind::Bare);
        assert_eq!(groups[0].id, CONTROL_GROUP);
        assert!(Focusable::<InnerHost>::place(&stuck, &CONTROL, &cx, At::Drawn).is_some());
        assert_eq!(Focusable::<InnerHost>::group_of(&stuck, &CONTROL, &cx), Some(CONTROL_GROUP));
    }

    /// A stalled Working read-out draws its action pill LOWER than a settled one carrying the same
    /// reason — `StatusOverlay::bands`'s `Working` branch offsets the caption down from the frame
    /// centre instead of straddling it, and this is the one number that formula depends on that a
    /// host test can actually see (the caption/reason heights come from [`FixtureMeasure`], not a
    /// loaded font, so this pins the ORDERING the two `cap_y` branches produce, not literal
    /// on-device pixels).
    ///
    /// **Trimmed to the one assertion that is actually about this ORDERING.** This test used to
    /// also carry `assert_eq!(working.h, StatusOverlay::CTRL_H)` and a centering check — both
    /// restate a literal `status_action_rect` writes into its own return unconditionally
    /// (`Rect::new(frame.cx() - w * 0.5, …, w, StatusOverlay::CTRL_H)` centres on `frame.cx()` and
    /// carries `CTRL_H` for ANY `w`, by construction, whatever the surrounding logic does), so
    /// they could not fail no matter how broken the geometry got. The property they were reaching
    /// for — that this hand-reproduced Rect actually agrees with the widget it reproduces — is the
    /// real question, and it needed a real widget on the other side of the assertion:
    /// [`status_action_rect_matches_the_widget_it_is_reproducing`] below is that test.
    #[test]
    fn a_stalled_working_readout_sits_its_action_pill_lower_than_a_settled_one() {
        let m = crate::ui::fixture::FixtureMeasure;
        let working = status_action_rect(&m, ESCAPE, true, true);
        let settled = status_action_rect(&m, ESCAPE, false, true);
        assert!(
            working.y > settled.y,
            "Working straddles the centre with the spinner above it; a settled read-out centres \
             the caption alone, which sits higher"
        );
    }

    /// A reason line pushes the action pill further down still, whatever `working` is — `bands`'s
    /// `below` accumulator, ported onto `Measure`.
    #[test]
    fn a_reason_line_pushes_the_action_pill_down_further() {
        let m = crate::ui::fixture::FixtureMeasure;
        let with_reason = status_action_rect(&m, ESCAPE, false, true);
        let without = status_action_rect(&m, ESCAPE, false, false);
        assert!(with_reason.y > without.y);
    }

    /// A [`Measure`] that forwards straight to `crate::text`'s own raw, no-font-loaded fallbacks —
    /// the exact numbers `StatusOverlay::bands`/`action_frame` themselves fall back to when nothing
    /// has called `init_text`, which is true of every host test. It exists ONLY for the test below,
    /// and neither of the two `Measure`s already in this file can stand in for it:
    /// [`crate::ui::fixture::FixtureMeasure`]'s `line_h` is `sz * 1.2`, a deliberately rough
    /// stand-in for real font metrics (see its own doc) rather than the widget's actual fallback —
    /// `text_height`'s is `sz` exactly, factor 1.0, no font needed — so a `FixtureMeasure`-based
    /// comparison against the real widget could never agree numerically even when the two formulas
    /// are identical; and `TtfMeasure::width` carries a `debug_assert!` that exists to catch a REAL
    /// draw reached before boot's `init_text`, which every host test would trip on the first call.
    /// Neither omission was a bug — `FixtureMeasure` is for tests that only need INTERNAL
    /// consistency (the two tests above), and `TtfMeasure`'s assert is doing its job — but their
    /// combination is exactly why the property `status_action_rect`'s own doc claims ("mirrors …
    /// exactly") had never actually been checked by anything in this file.
    struct RawTextMeasure;
    impl Measure for RawTextMeasure {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            crate::text::text_width(s.as_ptr(), sz, bold as c_int)
        }
        fn cap_h(&self, sz: i32) -> f32 {
            crate::text::cap_h(sz, 0)
        }
        fn line_h(&self, sz: i32) -> f32 {
            crate::text::text_height(sz, 0)
        }
    }

    /// **The property `status_action_rect`'s own doc claims, actually checked.** Neither test above
    /// it compares against a real [`StatusOverlay`] at all — both only compare two calls to
    /// `status_action_rect` against EACH OTHER, which can pin an ordering but can never catch the
    /// two formulas drifting apart from each other, term for term, in lockstep. This test builds a
    /// real `StatusOverlay` with the same shape (`kind`, `reason`, `action`) `LoginScreen::draw`
    /// ever configures one with and asserts its `action_frame()` Rect is bit-for-bit the Rect
    /// `status_action_rect` computes for the equivalent inputs, across both axes
    /// (`working`/`has_reason`) this screen ever combines. `RawTextMeasure` is what makes an exact
    /// comparison possible on the host at all — see its own doc for why `FixtureMeasure` cannot do
    /// this job.
    #[test]
    fn status_action_rect_matches_the_widget_it_is_reproducing() {
        for (working, kind) in [(true, StatusKind::Working), (false, StatusKind::Failed)] {
            for (has_reason, reason) in
                [(false, None), (true, Some(c"This is taking longer than usual."))]
            {
                let mut o = StatusOverlay::new(Rect::FULL, c"caption", kind).action(ESCAPE);
                if let Some(r) = reason {
                    o = o.reason(r);
                }
                let want = o.action_frame().expect("an action was set above");
                let got = status_action_rect(&RawTextMeasure, ESCAPE, working, has_reason);
                assert_eq!(
                    (got.x, got.y, got.w, got.h),
                    (want.x, want.y, want.w, want.h),
                    "working={working} has_reason={has_reason}"
                );
            }
        }
    }

    fn step_ev(s: &mut LoginScreen, ev: &ScreenEvent<InnerHost>) -> (Handled, Vec<Stamped<InnerHost>>) {
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m);
        let mut present = Present::new();
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let handled = {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            Machine::<InnerHost>::step(s, ev, &cx, &mut fx)
        };
        (handled, buf)
    }

    fn key_back_down() -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: crate::ui::machine::Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        })
    }

    /// **This screen has no panel of its own**, so every BACK is the root press — asked of the
    /// loop (`LoopReq::AuthBackAtRoot`, performed by `app::input::login_or_profiles_root_back`)
    /// rather than performed here, exactly as `screens::consent`'s first stage asks for the plain
    /// `BackAtRoot` for the same underlying reason (that module's own
    /// `back_at_the_first_consent_stage_is_the_root_press…` test in `app/bridge.rs` is the sibling
    /// of this one, one layer up — the two variants differ because this root press needs
    /// `auth::cancel`'s answer first, which `AuthBackAtRoot`'s own doc argues at length).
    /// Deliberately built with [`bare_screen`] rather than `LoginScreen::new` — BACK's answer does
    /// not depend on the phase at all, so this test owes `crate::auth` nothing.
    #[test]
    fn back_is_always_the_root_press() {
        let mut s = bare_screen(Phase::Waiting, 0.0);
        let (handled, effs) = step_ev(&mut s, &key_back_down());
        assert_eq!(handled, Handled::Yes);
        assert!(
            effs.iter().any(|st| matches!(&st.fx, Fx::App(AppFx::Loop(LoopReq::AuthBackAtRoot)))),
            "login has no panel of its own to close first — BACK asks the loop for the root press"
        );
    }

    /// **The QR texture must not survive its screen — the leak `ScreenEvent::Unmount`'s own arm
    /// doc explains.** `999` stands in for a real GL id: a host test never uploads one for real
    /// (`prepare_qr_tex`'s own doc — GL work happens in `Screen::prepare`, never in `step`), so
    /// this pins the ONE property that matters without touching GL — that `step` zeroes `qr_tex`
    /// on `Unmount` regardless of what it held. Delete that arm, or let it fall back through to
    /// the catch-all `_ => Handled::No` the way it did before this fix, and this goes red:
    /// `qr_tex` stays `999` and `handled` reads `Handled::No`.
    #[test]
    fn unmount_frees_the_qr_texture() {
        let mut s = bare_screen(Phase::Waiting, 0.0);
        s.qr_tex = 999;
        s.qr_px = (400, 400);
        let (handled, _) = step_ev(&mut s, &ScreenEvent::Unmount);
        assert_eq!(handled, Handled::Yes);
        assert_eq!(s.qr_tex, 0, "an unmounting screen must not leak its GL texture id");
        assert_eq!(s.qr_px, (0, 0), "…nor go on claiming its bytes in the frame's render set");
    }

    /// **The QR bitmap is a render this SCREEN owns** — its own `upload_rgba`, its own
    /// `delete_tex` — so it is one of the two things in the tree that override
    /// `Screen::render_report` (§8.3 rule (c)); everything else on screen here is drawn
    /// immediate-mode or comes from a shared pool. `999` stands in for a real GL id for the same
    /// reason `unmount_frees_the_qr_texture` uses it: a host test uploads nothing.
    #[test]
    fn the_qr_bitmap_is_reported_as_this_screens_own_render() {
        use crate::ui::frame::RenderReport;
        let mut s = bare_screen(Phase::Waiting, 0.0);
        assert_eq!(
            Screen::<InnerHost>::render_report(&s),
            RenderReport::NONE,
            "no code yet: this screen holds no render of its own"
        );
        s.qr_tex = 999;
        s.qr_px = (400, 400);
        assert_eq!(
            Screen::<InnerHost>::render_report(&s),
            RenderReport::one(400, 400),
            "one texture, 400x400 RGBA8 = 640,000 bytes"
        );
    }
}
