//! **The Settings family as a SURFACE with a stack of its own** (restructure spec §6.2
//! `SettingsSurface`, phase 5b): [`RouteSurface`] is one modal entry on the app's `ModalStack`
//! (Opaque, its host cached while it fades in and REPLACED once its ground has drawn) that owns
//! a `NavStack<InnerHost>` of family pages — root → Privacy | Legal | Favourites → Document —
//! and walks it on BACK before the container hears anything (`settings_back_walks_its_own_stack_
//! not_the_apps`). The same type carries the FIRST-RUN consent question: a surface over the
//! picker (or Home) whose root is the first stage and whose second stage is a push, so the two
//! ceremonies share one push spring, one ground and one focus model.
//!
//! What replaced what (spec §14): `ui/settings.rs`'s eleven statics, its `RouteFocus` ladder and
//! `SETTINGS_HOME_RETURN`/`HOME_PUSH`/`HOME_WAS_OPENING` — the two-spring choreography that
//! existed only because the Home editor was a `Route` drawn from outside the modal — are this
//! one stack and one spring. Focus lives in the engine (§7.3); the pages seat their tables on
//! `FocusMoved` and read `cx.focus` to draw.
//!
//! The surface is its OWN [`LogicalState`] (§5.4) and that state is mostly the inner stack —
//! depth, each page's argument, each mounted body's hash, and the seats a pop would restore. It
//! has to be, or the whole family is one opaque word to the recorder and `app/recorder.rs`'s
//! `state_fp` bump bought nothing; the impl carries the argument in full. **What the surface
//! cannot do is make a PAGE's own state cover that page** — `state()` is a trait object and a
//! body that writes only its identity hashes identically however it is scrolled or toggled. The
//! impl's doc carries the per-page census, including the one kind that is still identity-only;
//! read it before believing that a press inside this family is gradable.
//!
//! The push spring is the surface's own rather than the inner stack's `Transition`, because a
//! POP must keep drawing the page it retired until the slide is over, and a `NavStack` unmounts
//! at commit: `leaving` holds that body for the spring's length and draws it in the child role,
//! exactly as `legal.rs`'s index/document pair never swapped roles when BACK ran the same spring
//! backward.

use std::borrow::Cow;

use crate::ui::containers::stack::{Instance, NavStack};
use crate::ui::containers::transition::Immediate;
use crate::ui::containers::{Life, Minter};
use crate::ui::form::{Form, FormId, FormSection, FormTable, RowKey, RowKind};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InstanceId, Key,
    LogicalState, Machine, MachineId, NavOp, PresentHandle, Stamped, Tick,
};
use crate::ui::present::Provenance;
use crate::ui::route_screen::{RouteLayout, RoutePush};
use super::family::SessionGround as RouteGround;
use crate::ui::screen::{
    At, Dir, DrawFrame, Enter, FocusSource, FocusTarget, Focusable, GroupSpec, HitSource, Mounter,
    Placed, RenderStrategy, ReturnState, Screen, ScreenEvent, Step,
};
use crate::ui::table::Row;
use crate::ui::table_screen::{Header, TableScreen};
use crate::ui::{theme, Painter, Rect};

use super::family::{form_activate, form_focus, form_right_target, inner_cx, InnerHost, SettingsPage, ALERT_GROUP};
use super::plaintext_question::{self, AlertStep, PlaintextAlert};
use super::registry::{word, AppFx, DirectoryLike};

/// Which ceremony the surface carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Family {
    /// Settings, over a live page: scrim + ambient ground sampled off the host.
    Settings,
    /// The first-run consent question: a route surface of its own on the hero-keyed ground.
    FirstRunConsent,
}

/// The `Family::Settings` scrim's ink alpha: the surface's own appear (`local_alpha`, the
/// `RouteSurface`'s `page_alpha` after its container's overwrite) composed with the ROUTE-level
/// nav dip beneath it (`nav_page_alpha`, `DrawFrame::nav_page_alpha` — spec §14 phase 8), so the
/// scrim never reads as present-but-undimmed while a Home↔Library route change is still fading
/// underneath a Settings surface that is itself already fully open.
fn settings_scrim_alpha(local_alpha: f32, nav_page_alpha: f32) -> f32 {
    theme::underlay::DIM_PANEL * local_alpha * nav_page_alpha
}

/// The `Family::Settings` entrance cascade's alpha: same composition as
/// [`settings_scrim_alpha`], undivided by `theme::underlay::DIM_PANEL` — what the ground and the pages themselves
/// draw through.
fn settings_entrance_alpha(local_alpha: f32, nav_page_alpha: f32) -> f32 {
    local_alpha * nav_page_alpha
}

/// The push spring — the family's one `RoutePush`, the only one, since every Settings drill-down is a stack
/// push — with the page it is carrying OUT on a pop.
struct Push {
    route: RoutePush,
    /// Which endpoint the spring is driving to: a push runs to open, a pop back to closed.
    open: bool,
    /// A popped body, drawn in the child role until the spring settles at 0.
    leaving: Option<Instance<InnerHost>>,
}

impl Push {
    const fn new() -> Self {
        Self {
            route: RoutePush::new(),
            open: false,
            leaving: None,
        }
    }
    fn amount(&self) -> f32 {
        self.route.amount()
    }
    fn settled(&self) -> bool {
        self.route.resting(self.open)
    }
    fn parent(&self, p: Painter) -> Painter {
        self.route.parent(p)
    }
    fn child(&self, p: Painter) -> Painter {
        self.route.child(p)
    }
}

pub(crate) struct RouteSurface {
    entry: EntryId,
    id: InstanceId,
    /// Which ceremony this surface carries. It is the FIRST term of the surface's own
    /// [`LogicalState`] (the impl below), not a state object of its own: a `SurfaceState { kind }`
    /// struct lived here and hashed the ceremony ALONE under a doc claiming it covered "the
    /// ceremony, the stack's pages and each page's own", which made every press inside the whole
    /// family invisible to `Dispatcher::state_hash` — see the impl for why that mattered.
    kind: Family,
    inner: NavStack<InnerHost>,
    ids: Minter,
    push: Push,
    ground: RouteGround,
    ground_ready: bool,
    /// The focus each inner entry was left at, for the `Restored` seat on a pop.
    remembered: Vec<(EntryId, u32)>,
}

impl RouteSurface {
    /// A surface at `entry`/`id` (the dispatcher's, from the mounter) whose root is `root`.
    pub(crate) fn new(
        entry: EntryId,
        id: InstanceId,
        kind: Family,
        root: SettingsPage,
        hubs: crate::pms::HubsView<'_>,
    ) -> Self {
        let mut s = Self {
            entry,
            id,
            kind,
            inner: NavStack::new(Box::new(Immediate)),
            ids: Minter::default(),
            push: Push::new(),
            ground: if kind == Family::FirstRunConsent { super::family::pre_home_ground(hubs) } else { RouteGround::new() },
            ground_ready: false,
            remembered: Vec::new(),
        };
        let mut trail = root.boot_trail().into_iter();
        if let Some(first) = trail.next() {
            s.inner.request(NavOp::Root(first), ReturnState::default());
        }
        for page in trail {
            s.inner.request(NavOp::Push(page), ReturnState::default());
        }
        s
    }

    /// The family's top page, if the stack has a body.
    fn top(&self) -> Option<&Instance<InnerHost>> {
        self.inner.top().and_then(|e| e.inst.as_ref())
    }
    fn top_mut(&mut self) -> Option<&mut Instance<InnerHost>> {
        self.inner.top_mut().and_then(|e| e.inst.as_mut())
    }
    /// The page beneath the top — drawn in the parent role while a push is in flight.
    fn below(&mut self) -> Option<&mut Instance<InnerHost>> {
        let n = self.inner.entries.len();
        if n < 2 {
            return None;
        }
        self.inner.entries[n - 2].inst.as_mut()
    }

    /// **Is the push over?** — i.e. is there exactly ONE page on screen, the top, at `entrance`.
    ///
    /// The spring settles at BOTH ends (0 after a pop and at mount, 1 after a push), so this is a
    /// question about the SPRING and never about how deep the stack is. `draw` asked it as "is
    /// there no page below me", which is only the same question at depth 1 — see the comment at
    /// the branch, and `a_settled_pop_leaves_the_surface_at_rest_at_depth_two`, which is the state
    /// that used to draw the wrong page of the two it had.
    fn at_rest(&self) -> bool {
        self.push.leaving.is_none() && self.push.settled()
    }

    /// Run the inner stack's lifecycle against its own bodies; the page events a push or pop
    /// produces are delivered to the bodies here, in order, and their emissions forwarded.
    fn run_inner<H: DirectoryLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let steps = self.inner.commit(&mut self.ids);
        for step in steps {
            match step {
                Life::Mount(eid) => {
                    let inst_id = self.ids.instance();
                    let entry = self.entry;
                    let icx = inner_cx(cx);
                    let mut out: Vec<Stamped<InnerHost>> = Vec::new();
                    let screen = {
                        let mut ifx = Effects::from_handle(
                            &mut out,
                            MachineId::Instance(inst_id),
                            fx.present(),
                        );
                        let arg = self
                            .inner
                            .entry(eid)
                            .map(|e| e.arg)
                            .unwrap_or(SettingsPage::Root);
                        mount_page(entry, arg, &icx, &mut ifx)
                    };
                    if let Some(e) = self.inner.entry_mut(eid) {
                        e.inst = Some(Instance {
                            id: inst_id,
                            screen,
                            inflight: Vec::new(),
                            staged: false,
                            staged_effects: Vec::new(),
                        });
                    }
                    self.forward(out, cx, fx);
                    self.deliver(eid, ScreenEvent::Mount, cx, fx);
                }
                Life::Ev(eid, ev) => {
                    self.deliver(eid, ev, cx, fx);
                }
                Life::Unmount(eid) => {
                    self.deliver(eid, ScreenEvent::Unmount, cx, fx);
                    let body = self.inner.entry_mut(eid).and_then(|e| e.inst.take());
                    // the popped page keeps drawing for the spring's length
                    if self.push.leaving.is_none() {
                        self.push.leaving = body;
                    }
                }
                // **An EVICTION is not a departure, and must not join the outgoing spring.**
                // `NavStack::evict` retires the BODY of an entry that STAYS on the stack, below
                // the top, so that a later Pop can remount it from its `ReturnState`
                // (`containers::stack`'s CAP doc, and `an_evicted_entry_keeps_its_focus_identity_
                // on_remount`). Handled as an `Unmount` — which it was until 2026-09-07 — its
                // body lands in `push.leaving` and is then drawn in the CHILD role for the length
                // of whatever push is in flight: the wrong page, sliding, over the one the user
                // asked for. Unreachable today (CAP is 16 and this family's stack is at most
                // three deep, which is exactly why it reads as safe), so the guard is the code
                // rather than a test that would have to fake a sixteen-deep Settings.
                Life::Evict(eid) => {
                    self.deliver(eid, ScreenEvent::Unmount, cx, fx);
                    if let Some(e) = self.inner.entry_mut(eid) {
                        e.inst = None;
                    }
                }
            }
        }
        let ids: Vec<InstanceId> = Vec::new();
        self.inner.prune(&ids);
    }

    /// Step one inner body under the outer context and forward what it emitted.
    fn deliver<H: DirectoryLike>(
        &mut self,
        eid: EntryId,
        ev: ScreenEvent<InnerHost>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        let mut out: Vec<Stamped<InnerHost>> = Vec::new();
        let handled = {
            let icx = inner_cx(cx);
            let Some(inst) = self.inner.entry_mut(eid).and_then(|e| e.inst.as_mut()) else {
                return Handled::No;
            };
            let mut ifx =
                Effects::from_handle(&mut out, MachineId::Instance(inst.id), fx.present());
            inst.screen.step(&ev, &icx, &mut ifx)
        };
        self.forward(out, cx, fx);
        handled
    }

    /// Step the TOP page.
    fn step_top<H: DirectoryLike>(
        &mut self,
        ev: ScreenEvent<InnerHost>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Handled {
        match self.inner.top().map(|e| e.id) {
            Some(eid) => self.deliver(eid, ev, cx, fx),
            None => Handled::No,
        }
    }

    /// An inner page's emissions, translated to the outer sink: a `Nav` op is the inner stack's
    /// (§6.2), everything else passes through unchanged (same bundle, same element type).
    fn forward<H: DirectoryLike>(
        &mut self,
        out: Vec<Stamped<InnerHost>>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) {
        for s in out {
            match s.fx {
                // an inner page's Dismiss is the SURFACE's dismissal (consent's final answer)
                Fx::Nav(NavOp::Dismiss(_)) => fx.push(Fx::Nav(NavOp::Dismiss(self.entry))),
                Fx::Nav(op) => self.request(op, cx, fx),
                // an inner page asking to be re-seated (a row that opened an alert) — the
                // engine is the outer one, so the Enter is delivered to the surface's instance
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(e))) => fx.push(Fx::Deliver(
                    MachineId::Instance(self.id),
                    Delivery::Screen(ScreenEvent::Enter(e)),
                )),
                Fx::App(a) => fx.push(Fx::App(a)),
                Fx::Log(l) => fx.push(Fx::Log(l)),
                Fx::Press(arm) => fx.push(Fx::Press(arm)),
                Fx::Remember { group, elem } => {
                    // Forwarding re-stamps the emission as this surface. Preserve inner
                    // ownership before doing so: covered/leaving pages still receive ticks.
                    if self.top().is_some_and(|top| s.from == MachineId::Instance(top.id)) {
                        fx.remember(group, elem);
                    }
                }
                Fx::Timer { id, after_ms } => fx.push(Fx::Timer { id, after_ms }),
                Fx::CancelTimer(id) => fx.push(Fx::CancelTimer(id)),
                Fx::Mount(_) | Fx::Unmount(_) | Fx::Deliver(..) => {
                    debug_assert!(false, "an inner page emitted a structural op or a delivery");
                }
            }
        }
    }

    /// An inner navigation: push or pop, with the remembered focus captured/restored and the
    /// engine re-seated through an `Enter` the surface delivers to ITSELF (§7.3 step 5).
    ///
    /// **A POP THAT WOULD EMPTY THE INNER STACK IS THE SURFACE'S OWN DISMISSAL, AND IT IS
    /// ANSWERED HERE SO THAT IT HOLDS HOWEVER THE POP ARRIVED.** `NavStack::apply`'s `Pop` arm
    /// retires the top unconditionally — there is no depth guard in the container, and there
    /// should not be, since a page stack under a tab bar legitimately has nothing beneath its
    /// root. A surface does not: it IS the bottom of its own world, so a stack with no entries
    /// leaves `top_mut()` at `None` — and once the outgoing slide settles, `at_rest()` is true
    /// again, so `draw` takes its single-page branch and finds nothing to put in it. The surface
    /// goes on drawing its scrim and its ground over an empty frame, contributes no hit stops,
    /// and answers no key: still up, still owning input, and unreachable.
    ///
    /// Two real pages emit exactly that bare `Pop` as their "I am done": `consent::band_commit`'s
    /// Settings arm (Privacy & data → Done) and `onboard::leave`'s settings arm (Favorite
    /// libraries → Done/Cancel). Both are correct at the depth the ROOT surface puts them at —
    /// pushed over the Settings root, so the pop reveals it — and both empty the stack when the
    /// surface was booted ROOTED at that page, which `/tmp/plxnative-settings=privacy|home` does
    /// (`app/run.rs`'s boot-target match). A `RELEASE` build compiles `devtrig::read` out and always
    /// roots at `SettingsPage::Root`, so this was never a shipping bug — but a Pop with nothing
    /// under it is a statement the page means ("close me"), not an accident to be guarded against
    /// at each emitter, and the surface is the only thing that knows there is nothing under it.
    ///
    /// **This is deliberately NOT what the `Key::Back` arm does at the same depth.** That one
    /// answers `Handled::No` and hands the press to the CONTAINER, because a BACK at a surface's
    /// root is a question about the whole tree and the container's answer may be more than a
    /// dismissal (`Navigation::back`'s root rule reaches `Rig::back_at_root`, the platform's
    /// Home). A page's own `Pop` carries no such question: it names this surface and nothing
    /// above it, so it becomes `Dismiss(self.entry)` — the same effect `forward` already
    /// translates an inner `Dismiss` into for consent's final answer, so both roads out of the
    /// family end at one op.
    fn request<H: DirectoryLike>(
        &mut self,
        op: NavOp<SettingsPage>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) {
        if matches!(op, NavOp::Pop) && self.inner.depth() <= 1 {
            fx.push(Fx::Nav(NavOp::Dismiss(self.entry)));
            return;
        }
        let was_top = self.inner.top().map(|e| e.id);
        if let (Some(eid), Some(k)) = (was_top, cx.focus.current) {
            self.remembered.retain(|(e, _)| *e != eid);
            self.remembered.push((eid, k.elem));
        }
        let popping = matches!(op, NavOp::Pop);
        self.inner.request(op, ReturnState::default());
        self.run_inner(cx, fx);
        // An entry the container no longer knows about (a completed Pop's own page, retired and
        // pruned inside `run_inner` above) can never be revisited under this `EntryId` — a fresh
        // visit mints a new one — so its remembered focus is garbage from here on. Without this,
        // `remembered` grows by one entry on every push AND every pop for the whole life of one
        // Settings session, since nothing else ever shrinks it. `self.inner.entry` still answers
        // for a MOUNTED or merely EVICTED entry (an evicted body keeps its cursor for exactly
        // this kind of remount, `containers::stack`'s own CAP doc), so this drops only the ids
        // that are gone for good and never the ones a later `PopTo`/`Root` could still reach.
        self.remembered
            .retain(|(e, _)| self.inner.entry(*e).is_some());
        // the spring: a push runs 0 → 1 with the new page in the child role; a pop runs 1 → 0
        // with the retired page in the child role
        if popping {
            self.push.route.jump(true);
            self.push.open = false;
        } else {
            self.push.leaving = None;
            self.push.route.jump(false);
            self.push.open = true;
        }
        // **Every page in this family shares the surface's OUTER `EntryId`, and every page's
        // table shares `GroupId(0)`** (module doc, and `FocusTarget`'s own doc on `machine.rs`).
        // That makes `(EntryId, GroupId(0))` the same `Seat::Remembered` key for Root, Legal,
        // Privacy and every other page here — harmless on a POP, where the key IS meant to name
        // whichever page is being returned to and the `remembered` list above already looked up
        // its saved row, but wrong on a PUSH: the destination has never been entered, so
        // `ContainerGroup` would hand `seat_in` the OUTGOING page's remembered row instead of the
        // new page's first row (the reported bug: OK on row 2 opened Legal already seated on
        // Legal's row 2). `FirstInGroup` is the same group with the remembered cursor ignored, and
        // it is used on every arm below EXCEPT the one that found a saved elem to restore.
        let focus = match (popping, self.inner.top().map(|e| e.id)) {
            (true, Some(eid)) => self
                .remembered
                .iter()
                .find(|(e, _)| *e == eid)
                .map(|(_, elem)| {
                    FocusTarget::Elem(FocusKey {
                        entry: self.entry,
                        elem: *elem,
                    })
                })
                .unwrap_or(FocusTarget::FirstInGroup(GroupId(0))),
            _ => FocusTarget::FirstInGroup(GroupId(0)),
        };
        fx.push(Fx::Deliver(
            MachineId::Instance(self.id),
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus })),
        ));
        fx.invalidate(Provenance::Nav);
    }

    /// The word the top page names (the heartbeat's `overlay=`).
    fn top_word(&self) -> &'static str {
        self.top().map_or(word::SETTINGS, |i| i.screen.name())
    }
}

/// The mounter's one `match` for the family (§6.1).
fn mount_page(
    entry: EntryId,
    arg: SettingsPage,
    cx: &Cx<'_, InnerHost>,
    fx: &mut Effects<'_, InnerHost>,
) -> Box<dyn Screen<InnerHost>> {
    match arg {
        SettingsPage::Root => Box::new(RootPage::new(entry, cx.views)),
        SettingsPage::Language => Box::new(LanguagePage::new(entry)),
        SettingsPage::Contribute => Box::new(super::legal::DocumentPage::contribute(entry)),
        SettingsPage::Playback => Box::new(super::preferences::PreferencesPage::new(entry, super::preferences::Kind::Playback)),
        SettingsPage::AudioSubtitles => Box::new(super::preferences::PreferencesPage::new(entry, super::preferences::Kind::AudioSubtitles)),
        SettingsPage::Legal => Box::new(super::legal::LegalIndex::new(entry)),
        SettingsPage::About => Box::new(super::legal::DocumentPage::about(entry)),
        SettingsPage::Document(i) => Box::new(super::legal::DocumentPage::legal(entry, i)),
        SettingsPage::Privacy => Box::new(super::consent::ConsentPage::settings(entry, cx, fx)),
        SettingsPage::Preview(i) => Box::new(super::consent::PreviewPage::new(entry, i)),
        SettingsPage::ConsentStage(i) => {
            Box::new(super::consent::ConsentPage::first_run(entry, i, cx, fx))
        }
        SettingsPage::Favourites => Box::new(super::onboard::OnboardScreen::settings(entry, cx.views)),
        SettingsPage::Picker(kind) => Box::new(super::preferences::PickerPage::new(entry, kind)),
    }
}

/// **The surface IS its own logical state, and the inner stack is most of it** (§5.4).
///
/// `app/recorder.rs` re-pinned `state_fp` for this phase — invalidating every committed replay
/// fixture — on the stated grounds that "without folding `Dispatcher::state_hash` in, a replay
/// would have graded every press inside Settings, Privacy, Legal and first-run Favourites as
/// identical". That fold reaches a surface through `containers::Navigation::write`, which hashes
/// exactly `i.screen.state().hash()` per entry — so if this answers the CEREMONY alone, the pin
/// bought nothing and the fixtures were invalidated for no gain. It did answer the ceremony alone
/// until 2026-09-07. The concrete miss: in Privacy, OK on *Share crash reports* flips the page's
/// draft with focus left on the same row, and the route word, the overlay word and the focus
/// fingerprint are all byte-identical either side of it — so a replay of a run that FAILED to
/// toggle still ended `verdict=SAME`.
///
/// The shape mirrors `containers::Navigation::write` deliberately, because a surface's inner
/// stack is the same object one level down: depth, then per entry its `EntryId`, its argument
/// (`SettingsPage`'s own encoding — `screens::family`, where a new variant is forced through a
/// census `match`) and, when it has a body, that body's `InstanceId` and its own state hash.
/// After the stack comes `remembered`, the per-entry focus a pop restores.
///
/// **WHAT THIS COVERS IS EXACTLY TWO THINGS, AND THE SECOND IS SOMEBODY ELSE'S CODE.** The
/// surface itself contributes the ceremony, the SHAPE of the stack (how deep, which page at each
/// level, which entry ids and instance ids) and the remembered seats. Everything FINER — what a
/// page holds — is a claim about each body's own `LogicalState`, and this file cannot enforce a
/// word of it: `state()` is a trait object, `hash()` folds in whatever that impl chose to write,
/// and an impl that writes nothing hashes identically forever without failing anything. The
/// census, as of 2026-09-07, one line per page kind, so the next reader can check it instead of
/// trusting it:
///
///  * `RootPage` → `RootState`: the selected row's `RowKey` (identity, not position; `remembered` holds
///    the same keys for the root), Automatically Sign In, and trailer autoplay.
///  * `PreferencesPage` (Playback, Audio & Subtitles): the selected field's `RowKey`, confirmed
///    values and pending/error presentation. See `screens::preferences::SHAPE`.
///  * `PickerPage` (one preference's choice list, `SettingsPage::Picker`): which field, the
///    selected option's position, the checked option, pending/error presentation and the Force
///    acknowledgement state. See `screens::preferences::PICKER_SHAPE`.
///  * `ConsentPage` (Privacy & data, and each first-run stage) → `ConsentState`: the mode, both
///    halves of the draft decision, and whether the delete alert is up.
///  * `OnboardScreen` (Favorite libraries) → `OnboardState`: whether it is the Settings or the
///    first-run instance, whether it currently HAS an action band (the group set the engine is
///    reasoning about, cached at `rebuild` for the reason that field's own doc gives), and the
///    whole draft pin list.
///  * `DocumentPage` (the six Legal documents and About) → `DocState`: WHICH document, and its
///    reading position in whole `document_reader::STEP`s.
///  * `PreviewPage` (the five Privacy previews) → `PreviewState`: which preview. **Its reading
///    position is NOT in the hash**, so two frames of one preview scrolled to different places
///    are one word to a replay — the same gap `DocState::pos` closed for the Legal documents.
///    The fix belongs in `screens::consent`, beside the reader that owns the position, not here.
///
/// That last bullet is why this doc used to be worth distrusting and is worth reading now. It
/// said the finer half "rides in through each body's own `state().hash()`" and stopped there,
/// which reads as a guarantee when it is only a mechanism. The mechanism was always in place,
/// and TWELVE of the family's roughly sixteen pages — the six Legal documents, About and the five
/// Privacy previews, i.e. every READER — wrote their identity alone through it. A verifier
/// refuted the guarantee by scrolling a document; the Legal half was fixed in the same pass as
/// this sentence and the previews' half was not. **If you add a page to this family, the
/// hash gains its identity for free and NOTHING of its contents: adding the row here is the work,
/// and a page whose bullet would read "identity only" is a page a replay cannot grade.**
///
/// **The push spring is deliberately absent**, as is `ground_ready` and everything else the draw
/// reads: those are RENDER state, sampled from a wall clock, so folding them in would make every
/// mid-animation frame diverge from a recording of the same presses and turn the divergence
/// report into noise. What the recorder grades is which pages are open and what each one holds.
/// `push.leaving` is absent for the same reason and is worth naming separately, because it is a
/// whole PAGE rather than a float: a surface mid-pop hashes as though the pop had already
/// finished, which is right — the pop is committed on the stack and only the slide is still
/// running.
impl LogicalState for RouteSurface {
    fn write(&self, w: &mut Canon) {
        w.discriminant(self.kind as u32);
        w.seq(self.inner.entries.len());
        for e in &self.inner.entries {
            w.u32(e.id.0);
            e.arg.write(w);
            w.option(e.inst.as_ref(), |c, i| {
                c.u32(i.id.0);
                c.u64(i.screen.state().hash());
            });
        }
        // **`remembered` is logical state, not bookkeeping: it decides WHERE FOCUS LANDS on the
        // next pop.** Two builds that agree about every page and disagree about this map put the
        // cursor on different rows the moment BACK is pressed, and until 2026-09-07 that
        // difference was invisible to `Dispatcher::state_hash` — a replay would report the seat
        // itself as a divergence one frame later, at the `FocusMoved`, with nothing in the record
        // saying why.
        //
        // **Sorted by `EntryId` rather than written in `Vec` order, and that is a decision about
        // NOISE.** The insertion order is deterministic for one press sequence, so hashing it
        // would also work for a straight replay — but it carries no behaviour: `request` retires
        // an entry's old seat before pushing the new one, so the ids are unique, and every read
        // is a `find` by id. Hashing the order would let two states that behave identically in
        // every possible future report `DIVERGED`, which is the same argument that keeps the
        // spring out of this impl. Sorting is cheap because this list is bounded by the stack's
        // depth (three pages in this family today) and shrinks with it.
        let mut seats: Vec<(u32, u32)> = self.remembered.iter().map(|(e, k)| (e.0, *k)).collect();
        seats.sort_unstable();
        w.seq(seats.len());
        for (e, k) in seats {
            w.u32(e).u32(k);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(match self.kind {
            Family::Settings => "settings",
            Family::FirstRunConsent => "consent",
        });
        // one segment per page, bottom of the stack first, so a divergence report reads as the
        // path the surface is standing on rather than as a single opaque word
        for e in &self.inner.entries {
            out.push('/');
            e.arg.probe(out);
            match e.inst.as_ref() {
                Some(i) => {
                    out.push(':');
                    i.screen.state().probe(out);
                }
                // an entry whose body was evicted at CAP: the page is still on the stack and will
                // remount, which is a different thing from it not being there at all
                None => out.push_str(":-"),
            }
        }
        // …then the remembered seats, in the order `write` hashes them, so a `replay: diverge`
        // line that moved on this half NAMES the entry and the element rather than leaving the
        // reader to infer a focus difference from a hash that changed with no visible page move.
        let mut seats: Vec<(u32, u32)> = self.remembered.iter().map(|(e, k)| (e.0, *k)).collect();
        seats.sort_unstable();
        out.push_str(" seats=");
        for (i, (e, k)) in seats.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("{e}:{k}"));
        }
    }
}

impl<H: DirectoryLike> Machine<H> for RouteSurface {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.run_inner(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if self.ground.refresh() {
                    fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
                }
                self.tick(*t, cx, fx);
                Handled::Yes
            }
            ScreenEvent::Enter(_) => {
                // the engine seats on this (after_step); the top page hears it too
                self.step_top(
                    ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::ContainerGroup(GroupId(0)),
                    }),
                    cx,
                    fx,
                );
                Handled::Yes
            }
            ScreenEvent::Input(iev) => {
                let handled = self.step_top(ScreenEvent::Input(iev.clone()), cx, fx);
                if handled == Handled::Yes {
                    return Handled::Yes;
                }
                // **This arm answers LEFT as well as BACK, and that is the whole of rule 9 here.**
                // The family's table and document groups declare `EdgeRule::Nav(NavOpKind::Back)`
                // on their LEFT edge (`table_screen::TablePart::groups` / `DocumentFocus`), and
                // `dispatch::after_step` re-delivers such an edge rule to the input OWNER as a
                // synthetic `Key::Back` down with `at_edge: true` before it becomes the
                // dispatcher's `pending_back`. So the test below is deliberately blind to
                // `at_edge` and to the wcode: whichever key produced it, the surface pops its own
                // stack first, and only a BACK at the surface's own ROOT falls through as
                // `Handled::No` for the container to answer by dismissing the whole surface —
                // one LEFT back to the index and the second one out of the family. (That
                // sentence was `ui/legal.rs`'s `right_enters_a_document_and_left_walks_all_the_
                // way_back_out`, which left the tree with the module it lived in; the composed
                // tests at the bottom of this file are what execute it now.)
                //
                // **The `depth() > 1` guard stays even though `request` now turns an emptying Pop
                // into a dismissal, because the two are different answers and only one of them is
                // right here.** `request`'s dismissal is for a PAGE saying "close me", which
                // names this surface and nothing above it. A BACK at the surface's own root is a
                // question about the whole tree, and `Handled::No` is what lets the container
                // answer it — which for a modal is the dismissal, but for the stack underneath is
                // `Rig::back_at_root` and the television's own Home. Routing BACK through
                // `request` instead would swallow the press as `Handled::Yes` and hand the
                // container a `Dismiss` it never got first refusal on; `back_at_the_surface_s_own_
                // root_is_not_handled` and `left_at_the_surfaces_own_root_dismisses_it` are the
                // two ends of that.
                let back = matches!(
                    iev.kind,
                    crate::ui::machine::InputKind::Key {
                        key: Key::Back,
                        edge: crate::ui::machine::Edge::Down,
                        ..
                    }
                );
                if back && self.inner.depth() > 1 {
                    self.request(NavOp::Pop, cx, fx);
                    return Handled::Yes;
                }
                Handled::No
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                self.step_top(
                    ScreenEvent::FocusMoved {
                        from: *from,
                        to: *to,
                        by: *by,
                    },
                    cx,
                    fx,
                );
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => self.step_top(ScreenEvent::Activate(*e), cx, fx),
            ScreenEvent::PressHold(id) => self.step_top(ScreenEvent::PressHold(*id), cx, fx),
            ScreenEvent::PressCommit(id) => self.step_top(ScreenEvent::PressCommit(*id), cx, fx),
            ScreenEvent::Timer(id) => self.step_top(ScreenEvent::Timer(*id), cx, fx),
            ScreenEvent::StoreChanged(o, g) => {
                for eid in self.inner.entries.iter().map(|e| e.id).collect::<Vec<_>>() {
                    self.deliver(eid, ScreenEvent::StoreChanged(*o, *g), cx, fx);
                }
                Handled::Yes
            }
            // **The two halves of teardown are two events, and merging them delivered `Unmount`
            // TWICE to every page.** `ModalStack::prune` emits `WillLeave(ForGood)` and then
            // `Unmount` for the same surface, so one arm answering both fired the second event
            // for both — and a page whose `Unmount` releases something (a reader's cached flow, a
            // draft it declines to commit) had to be idempotent by luck rather than by contract.
            // Each is forwarded once, verbatim: `Leave` is passed through rather than assumed,
            // because `Deeper` and `ForGood` mean different things to a page and only the
            // container knows which this is.
            ScreenEvent::WillLeave(l) => {
                for eid in self.inner.entries.iter().map(|e| e.id).collect::<Vec<_>>() {
                    self.deliver(eid, ScreenEvent::WillLeave(*l), cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Unmount => {
                for eid in self.inner.entries.iter().map(|e| e.id).collect::<Vec<_>>() {
                    self.deliver(eid, ScreenEvent::Unmount, cx, fx);
                }
                Handled::Yes
            }
            // **The app-switch pair (0x103/0x106) has to reach the pages too.** `Navigation::
            // suspend`/`resume` deliver these to every BODY the tree owns, but the tree's view of
            // this surface is one body — so falling through to `_ => Handled::No` left the whole
            // family running while the app was backgrounded: springs integrating, readers
            // updating, timers still armed on a screen the television is not showing. Two arms
            // rather than one because a `ScreenEvent<H>` cannot be re-used as a
            // `ScreenEvent<InnerHost>` (different host, so the event is rebuilt, which is why
            // every forward in this impl names its variant).
            ScreenEvent::Suspend => {
                for eid in self.inner.entries.iter().map(|e| e.id).collect::<Vec<_>>() {
                    self.deliver(eid, ScreenEvent::Suspend, cx, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Resume => {
                for eid in self.inner.entries.iter().map(|e| e.id).collect::<Vec<_>>() {
                    self.deliver(eid, ScreenEvent::Resume, cx, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl RouteSurface {
    fn tick<H: DirectoryLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if !self.push.settled() {
            let mut ph: PresentHandle<'_> = fx.present();
            self.push.route.tick(self.push.open, t, &mut ph);
            if self.push.settled() && !self.push.open {
                self.push.leaving = None;
            }
        }
        // every body ticks (both levels stay warm through a push, as the legacy pair did)
        for eid in self.inner.entries.iter().map(|e| e.id).collect::<Vec<_>>() {
            self.deliver(eid, ScreenEvent::Tick(t), cx, fx);
        }
        // **…AND SO DOES THE PAGE ON ITS WAY OUT.** `push.leaving` is drawn in the child role for
        // the whole length of the reverse push, but it is no longer reachable through
        // `self.inner`: `run_inner` took its body out of the entry and `NavStack::prune` then
        // dropped the retired entry outright, so the loop above cannot see it. Without this it
        // spent those ~200 ms FROZEN on screen — `DocumentReader::update` and `TableView::update`
        // stopped mid-scroll while the page was still visibly sliding — which reads as a
        // stutter in the pop rather than as a page that stopped animating.
        //
        // Its emissions go up the same seam every other page's do (`forward`) rather than being
        // dropped: a `Tick` in this family produces none today (each page's `Tick` arm only steps
        // its own springs), and silently swallowing whatever a future one emits would be a bug
        // that no test could see.
        let mut out: Vec<Stamped<InnerHost>> = Vec::new();
        {
            let icx = inner_cx(cx);
            if let Some(inst) = self.push.leaving.as_mut() {
                let mut ifx =
                    Effects::from_handle(&mut out, MachineId::Instance(inst.id), fx.present());
                inst.screen.step(&ScreenEvent::Tick(t), &icx, &mut ifx);
            }
        }
        self.forward(out, cx, fx);
    }
}

impl<H: DirectoryLike> Focusable<H> for RouteSurface {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if let Some(top) = self.top() {
            top.screen.groups(&inner_cx(cx), out);
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        self.top()
            .and_then(|t| t.screen.group_of(key, &inner_cx(cx)))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        self.top()
            .map_or(Step::Edge, |t| t.screen.neighbour(key, dir, &inner_cx(cx)))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        self.top()
            .and_then(|t| t.screen.place(key, &inner_cx(cx), at))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        self.top()
            .map_or(want, |t| t.screen.reconcile(want, &inner_cx(cx)))
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<u32> {
        self.top().map_or(
            FocusKey {
                entry: self.entry,
                elem: 0,
            },
            |t| t.screen.seat(g, from, &inner_cx(cx)),
        )
    }
}

impl<H: DirectoryLike> Screen<H> for RouteSurface {
    fn name(&self) -> &'static str {
        self.top_word()
    }
    fn state(&self) -> &dyn LogicalState {
        // the surface itself, so the hash covers the inner stack — see the `LogicalState` impl
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>) {
        let icx = inner_cx(cx);
        for e in self.inner.entries.iter_mut() {
            if let Some(i) = e.inst.as_mut() {
                i.screen.prepare(b, &icx);
            }
        }
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let a = f.page_alpha;
        let root = Painter::root();
        // `family` is shared vocabulary, not a sibling screen (check-deps sibling rule)
        super::family::set_palette(self.ground.palette());
        match self.kind {
            Family::Settings => {
                // the scrim over the live host while the modal fades in; invisible under the
                // opaque ground at rest and what fades out over the host on dismissal
                let dim = theme::scrim_black(settings_scrim_alpha(a, f.nav_page_alpha));
                root.rect(Rect::FULL, 0.0, dim, dim, 0.0);
                crate::ui::profile::phase("st.ground", || self.ground.draw_host(root.alpha(a)));
            }
            Family::FirstRunConsent => {
                self.ground.draw_home(root);
            }
        }
        self.ground_ready = a >= 0.995;
        let entrance = match self.kind {
            Family::Settings => root.alpha(settings_entrance_alpha(a, f.nav_page_alpha)),
            Family::FirstRunConsent => root.alpha(a).translate(Rect::FULL.w * (1.0 - a), 0.0),
        };
        self.draw_pages(f, entrance);
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
    fn ground_ready(&self) -> bool {
        self.ground_ready
    }
}

impl RouteSurface {
    /// Draw the nested pages through the surface's entrance cascade. Kept separate from the
    /// ground paint so the real page selection and frame propagation can be tested without GL.
    fn draw_pages<H: DirectoryLike>(&mut self, f: &mut DrawFrame<'_, '_, H>, entrance: Painter) {
        let navigation = f.navigation();
        let t = self.push.amount();
        let icx = inner_cx(f.cx);
        let mut stops = Vec::new();
        // the parent role: the page beneath the top on a push, the top itself on a pop
        let parent_p = self.push.parent(entrance);
        let child_p = self.push.child(entrance);
        let popping = self.push.leaving.is_some();
        // **AT REST THERE IS EXACTLY ONE PAGE ON SCREEN — THE TOP — AND THE SPRING IS WHAT SAYS
        // SO.** It settles at BOTH ends (0 after a pop and at mount, 1 after a push), and at
        // either end the surviving page belongs at `entrance`: untranslated, undimmed, and the
        // only one contributing stops. This was written as the `else` of "there is no page below
        // me", which is a different question and answers the same way only at depth 1 — so
        // Settings root → Legal notices → a document → BACK left the spring parked at 0 with
        // `below()` still answering the ROOT, and the surface drew the Settings root at full
        // strength while the Legal index it had just returned to was invisible. The hit map
        // followed the draw, so the only rows a click could reach were the wrong page's.
        if self.at_rest() {
            if let Some(inst) = self.top_mut() {
                let mut inner = DrawFrame::with_navigation(&icx, entrance, navigation);
                inst.screen.draw(&mut inner);
                stops.extend(inner.into_stops());
            }
        } else {
            if t < 0.999 {
                if let Some(inst) = if popping { self.top_mut() } else { self.below() } {
                    let mut inner = DrawFrame::with_navigation(&icx, parent_p, navigation);
                    inst.screen.draw(&mut inner);
                    stops.extend(inner.into_stops());
                }
            }
            if t > 0.01 {
                let child = if popping {
                    self.push.leaving.as_mut()
                } else {
                    self.top_mut()
                };
                if let Some(inst) = child {
                    let mut inner = DrawFrame::with_navigation(&icx, child_p, navigation);
                    inst.screen.draw(&mut inner);
                    // a leaving page takes no input: its stops are not registered
                    if !popping {
                        stops.extend(inner.into_stops());
                    }
                }
            }
        }
        // the inner frames folded their own cascades; re-register through the identity
        for s in stops {
            f.stop(Painter::root(), s);
        }
    }
}

/// The surface's mounter is itself — `mount_page` — but the CONTAINER mounts the surface
/// through the app's mounter; this impl exists so a test bundle can mount family pages alone.
impl Mounter<InnerHost> for RouteSurface {
    fn mount(
        &mut self,
        _id: InstanceId,
        arg: &SettingsPage,
        _ret: &ReturnState<u32>,
        cx: &Cx<'_, InnerHost>,
        fx: &mut Effects<'_, InnerHost>,
    ) -> Box<dyn Screen<InnerHost>> {
        mount_page(self.entry, *arg, cx, fx)
    }
}

// ---------------------------------------------------------------------------------------------
// the root page
// ---------------------------------------------------------------------------------------------

/// A server's machine id — the identity of its "connect without encryption" row. Not
/// [`MachineId`], which names a UI machine (`ui/machine.rs`).
#[derive(Clone, PartialEq, Eq, Debug)]
struct ServerMachineId(String);

/// A root row's identity: what selection survives a rebuild by, and what a test addresses a row
/// by. Its focus key ([`FormId::key`]) is hand-assigned below and unrelated to the row's position,
/// so reordering the form moves no key; every key stays below `registry::BAND`.
#[derive(Clone, PartialEq, Eq, Debug)]
enum RootId {
    Favourites,
    Playback,
    AudioSubtitles,
    Language,
    AutoSignIn,
    TrailerAutoplay,
    /// One server's switch. Its key is [`PLAINTEXT_KEY_BASE`] plus its position in the section
    /// ([`root_form`] assigns it); its identity is the machine, so a rebuild keeps the row.
    Plaintext(ServerMachineId),
    Privacy,
    Legal,
    About,
}

/// The first key of the per-server switches: past every fixed [`RootId`] key.
const PLAINTEXT_KEY_BASE: u32 = 1000;

impl FormId for RootId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            RootId::Playback => 1,
            RootId::AudioSubtitles => 2,
            RootId::Favourites => 3,
            RootId::Language => 4,
            RootId::AutoSignIn => 5,
            RootId::TrailerAutoplay => 6,
            RootId::Privacy => 7,
            RootId::Legal => 8,
            RootId::About => 9,
            // the base; `root_form` adds the position (`item_keyed`)
            RootId::Plaintext(_) => PLAINTEXT_KEY_BASE,
        })
    }
}

/// What a root row does beyond opening a page (a `Nav` row never reaches it — see
/// [`FormTable::activate`]).
#[derive(Clone, PartialEq, Eq, Debug)]
enum Action {
    /// A `Nav` row's slot: opening its page is [`RowKind::Nav`]'s job, so nothing dispatches this.
    Door,
    AutoSignIn,
    TrailerAutoplay,
    /// A server's "connect without encryption" switch.
    Plaintext(ServerMachineId),
}

/// The Settings root: a table of destinations, every row a door (no band; rule 9 in full).
pub(crate) struct RootPage {
    entry: EntryId,
    form: FormTable<RootId, Action, SettingsPage>,
    state: RootState,
    session_watch: crate::plex::session::VisibleSessionWatch,
    session_snapshot: std::sync::Arc<crate::plex::session::Session>,
    pending_auto: Option<(bool, crate::storage_worker::TypedTicket<bool>)>,
    pending_trailer: Option<(bool, crate::storage_worker::TypedTicket<bool>)>,
    /// The servers the signed-in account answered crate::i18n::msg::settings_plaintext_question() for, then the
    /// ones discovery offers it for and nobody has answered, by row — the `(machine_id, allowed)`
    /// each switch shows.
    plaintext_rows: Vec<(String, bool)>,
    /// A switch flipped here and not yet reflected by the answers it reads (`grant::choices`):
    /// shown optimistically until they agree.
    pending_plaintext: Option<(String, bool)>,
    /// The shared question (`screens::plaintext_question`), asked when a switch is turned ON.
    alert: PlaintextAlert,
    /// `plex::grant::revision` as last read — an offer or an answer landing rebuilds the rows.
    grant_seen: u64,
}

struct RootState {
    /// The selected row's focus key — an identity, not a position.
    sel: RowKey,
    auto_sign_in: bool,
    trailer_autoplay: bool,
    language: crate::i18n::Preference,
    /// Each unencrypted-connection switch, in row order. Written to the canon only when there is
    /// one, so every other root's digest is unchanged.
    plaintext: Vec<bool>,
}

impl LogicalState for RootState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).bool(self.auto_sign_in).bool(self.trailer_autoplay).str(self.language.tag());
        if !self.plaintext.is_empty() {
            w.u32(self.plaintext.len() as u32);
            for &on in &self.plaintext {
                w.bool(on);
            }
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "root sel={} auto_sign_in={} trailer_autoplay={} language={}",
            self.sel.0, self.auto_sign_in, self.trailer_autoplay, self.language.tag()
        ));
        if !self.plaintext.is_empty() {
            out.push_str(&format!(" plaintext={:?}", self.plaintext));
        }
    }
}

/// One unencrypted-connection switch's plain data — see [`RootPage::plaintext_inputs`] for how
/// this is gathered and [`root_form`] for how it becomes a row.
struct PlaintextRowInput {
    machine: ServerMachineId,
    /// The server's own name, or the app's fallback ("Plex server") when none is known.
    name: String,
    /// `name` is a real machine name (Server text, may elide); false when it is the app's own
    /// fallback string, which is App text and must never elide.
    named: bool,
    on: bool,
    /// Whether a grant carries the server right now, stated quietly in the detail line.
    connected: bool,
}

/// Every argument [`root_form`] builds the Settings root's rows from — what [`RootPage::
/// rebuild`] gathers (mostly from `crate::plex::session::peek_settled()` and its own pending-write
/// state) before calling the pure builder, so the builder itself never reads a global and a test
/// can drive it directly with a synthesized combination no real session may currently be in.
struct RootInputs {
    signed_in: bool,
    /// A one-person account already skips the profile picker; only a multi-user account is ever
    /// shown the Automatically Sign In switch.
    multi_user: bool,
    library_count: i64,
    auto_sign_in: bool,
    trailer_autoplay: bool,
    language: crate::i18n::Preference,
    /// Unencrypted-connection switches, in row order — empty when signed out or when nobody has
    /// an answered/offered plaintext question.
    plaintext: Vec<PlaintextRowInput>,
}

/// The Settings root as a [`Form`], built from plain arguments rather than `&self` — see
/// [`RootInputs`]. `RootPage::rebuild` gathers the inputs and calls this; the text-fit tests
/// (`settings_text_fit_tests.rs`) call it directly, over every `RootInputs` combination worth
/// checking, without a real signed-in session. Reordering the page is moving a line here.
fn root_form(inputs: &RootInputs) -> Form<RootId, Action, SettingsPage> {
    let signed_in = inputs.signed_in;
    // Order: Libraries, Playback (player experience only), System (Language, Automatically Sign
    // In, Trailer autoplay), Unencrypted connections, Privacy, then About alone at the very end.
    //
    // The Libraries section is Libraries and the row is Favorite libraries: the switch governs
    // the whole app — Home's shelves, the top tab strip and the Library's Sources picker.
    let libraries = FormSection::new(crate::i18n::msg::settings_libraries_section())
        .visible(signed_in)
        .item(
            RootId::Favourites,
            RowKind::Nav(SettingsPage::Favourites),
            Action::Door,
            Row::new(crate::i18n::msg::settings_libraries_title())
                .detail(crate::i18n::msg::settings_libraries_detail())
                .value(crate::i18n::msg::settings_libraries_count(inputs.library_count))
                .chevron(true),
        );
    let playback = FormSection::new(crate::i18n::msg::settings_playback_section())
        .item(
            RootId::Playback,
            RowKind::Nav(SettingsPage::Playback),
            Action::Door,
            Row::new(crate::i18n::msg::settings_playback_title())
                .detail(crate::i18n::msg::settings_playback_detail())
                .chevron(true),
        )
        .item_if(
            signed_in,
            RootId::AudioSubtitles,
            RowKind::Nav(SettingsPage::AudioSubtitles),
            Action::Door,
            Row::new(crate::i18n::msg::settings_audio_title())
                .detail(crate::i18n::msg::settings_audio_detail())
                .chevron(true),
        );
    let system = FormSection::new(crate::i18n::msg::settings_system_section())
        .item(
            RootId::Language,
            RowKind::Nav(SettingsPage::Language),
            Action::Door,
            Row::new(crate::i18n::msg::settings_language_title())
                .detail(crate::i18n::msg::settings_language_detail())
                .value(preference_name(inputs.language))
                .chevron(true),
        )
        .item_if(
            signed_in && inputs.multi_user,
            RootId::AutoSignIn,
            RowKind::Toggle,
            Action::AutoSignIn,
            Row::new(crate::i18n::msg::settings_auto_sign_in_title())
                .detail(crate::i18n::msg::settings_auto_sign_in_detail())
                .toggle(inputs.auto_sign_in),
        )
        .item_if(
            signed_in,
            RootId::TrailerAutoplay,
            RowKind::Toggle,
            Action::TrailerAutoplay,
            Row::new(crate::i18n::msg::settings_trailers_title())
                .detail(crate::i18n::msg::settings_trailers_detail())
                .toggle(inputs.trailer_autoplay),
        );
    let has_plaintext = signed_in && !inputs.plaintext.is_empty();
    let mut plaintext = FormSection::new(crate::i18n::msg::settings_plaintext_section()).visible(has_plaintext);
    if has_plaintext {
        for (i, row) in inputs.plaintext.iter().enumerate() {
            let label = Row::new(&row.name);
            let label = if row.named { label.server_label() } else { label };
            plaintext = plaintext.item_keyed(
                RootId::Plaintext(row.machine.clone()),
                RowKey(PLAINTEXT_KEY_BASE + i as u32),
                RowKind::Toggle,
                Action::Plaintext(row.machine.clone()),
                label
                    .detail(plaintext_question::settings_detail(row.on, row.connected))
                    .toggle(row.on),
            );
        }
    }
    let privacy = FormSection::new(crate::i18n::msg::settings_privacy_section())
        .item(
            RootId::Privacy,
            RowKind::Nav(SettingsPage::Privacy),
            Action::Door,
            Row::new(crate::i18n::msg::settings_privacy_title())
                .detail(crate::i18n::msg::settings_privacy_detail())
                .chevron(true),
        )
        .item(
            RootId::Legal,
            RowKind::Nav(SettingsPage::Legal),
            Action::Door,
            Row::new(crate::i18n::msg::settings_legal_title())
                .detail(crate::i18n::msg::settings_legal_detail())
                .chevron(true),
        );
    let about = FormSection::new(crate::i18n::msg::settings_about_section()).item(
        RootId::About,
        RowKind::Nav(SettingsPage::About),
        Action::Door,
        Row::new(crate::i18n::msg::settings_about_title())
            .detail(crate::i18n::msg::settings_about_detail())
            .chevron(true),
    );
    Form::new()
        .section(libraries)
        .section(playback)
        .section(system)
        .section(plaintext)
        .section(privacy)
        .section(about)
}

impl RootPage {
    fn new(entry: EntryId, directory: crate::stores::browse::DirectoryView<'_>) -> Self {
        let mut s = Self {
            entry,
            form: FormTable::new(super::registry::BAND),
            session_watch: Default::default(),
            session_snapshot: Default::default(),
            pending_auto: None, pending_trailer: None,
            plaintext_rows: Vec::new(),
            pending_plaintext: None,
            alert: PlaintextAlert::new(ALERT_GROUP, super::registry::ALERT, super::registry::ALERT + 1),
            grant_seen: crate::plex::grant::revision(),
            state: RootState {
                sel: RowKey(0),
                auto_sign_in: false,
                trailer_autoplay: true,
                language: crate::i18n::Preference::System,
                plaintext: Vec::new(),
            },
        };
        s.rebuild(directory);
        s
    }

    /// Re-derive the rows from the session, keeping focus on the row it is on BY IDENTITY (a
    /// vanished row falls to its next, else previous, neighbour — `FormTable::set`).
    fn rebuild(&mut self, directory: crate::stores::browse::DirectoryView<'_>) {
        if let Some(snapshot) = crate::plex::session::peek_settled() {
            self.session_snapshot = snapshot;
        }
        let sess = &self.session_snapshot;
        let signed_in = sess.account(crate::plex::session::current().as_ref()).signed_in;
        let auto_sign_in = self.pending_auto.as_ref().map_or_else(|| sess.auto_sign_in(), |(value, _)| *value);
        let trailer_autoplay = self.pending_trailer.as_ref().map_or_else(|| sess.trailer_autoplay(), |(value, _)| *value);
        let multi_user = sess.home_users.len() > 1;
        self.state.auto_sign_in = auto_sign_in;
        self.state.trailer_autoplay = trailer_autoplay;
        self.state.language = crate::i18n::saved_preference();
        let plaintext = if signed_in { self.plaintext_inputs() } else { Vec::new() };
        let form = root_form(&RootInputs {
            signed_in, multi_user, library_count: directory.pinned_count() as i64,
            auto_sign_in, trailer_autoplay, language: self.state.language, plaintext,
        });
        let keep = self.form.selected_id().cloned();
        self.form.table.compact = false;
        self.form.table.header_ink = theme::TEXT_READING;
        self.form.set(form, keep.as_ref());
        self.form.table.list_focused = true;
        if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
            self.state.sel = key;
        }
    }

    /// **Unencrypted connections**: one input per server the signed-in account answered
    /// crate::i18n::msg::settings_plaintext_question() for (`grant::choices` — this session's answers over the
    /// session file's, for THIS account only), so an allowed one is here to turn off again — and,
    /// switched off, one per server discovery offers the question for that nobody has answered
    /// (`grant::offers`), so a signed-in person whose server went plaintext-only has a place to
    /// say yes. Reads `self.session_snapshot`/`self.pending_plaintext` (the account and the
    /// switch's own optimistic-write state), and records what it found back onto
    /// `self.plaintext_rows`/`self.state.plaintext` for [`RootPage::activate`]/[`LogicalState`] —
    /// what it returns is the plain [`PlaintextRowInput`]s [`root_form`] (a pure builder) turns
    /// into rows and [`Action::Plaintext`] entries.
    fn plaintext_inputs(&mut self) -> Vec<PlaintextRowInput> {
        use crate::plex::session::PlaintextChoice;
        let sess = &self.session_snapshot;
        let answered = crate::plex::grant::choices(&sess.plaintext_consent, &sess.account_token);
        let mut rows: Vec<(String, bool)> = answered
            .iter()
            .filter(|(_, choice)| *choice != PlaintextChoice::Undecided)
            .map(|(machine, choice)| (machine.clone(), choice.allows()))
            .collect();
        for offer in crate::plex::grant::offers() {
            if plaintext_question::asks(Some(&offer)) && !rows.iter().any(|(m, _)| *m == offer.machine_id) {
                rows.push((offer.machine_id, false));
            }
        }
        // The optimistic value holds until the answers read agree with it.
        if let Some((machine, on)) = &self.pending_plaintext {
            match rows.iter_mut().find(|(m, _)| m == machine) {
                Some(row) if row.1 == *on => self.pending_plaintext = None,
                Some(row) => row.1 = *on,
                None => {}
            }
        }
        self.plaintext_rows = rows.clone();
        self.state.plaintext = rows.iter().map(|(_, on)| *on).collect();
        let offers = crate::plex::grant::offers();
        rows.into_iter()
            .map(|(machine, on)| {
                // An offered server was never reached, so the session file does not know it yet:
                // the name discovery settled with comes first.
                let real_name = offers
                    .iter()
                    .find(|o| o.machine_id == machine && !o.name.is_empty())
                    .map(|o| o.name.clone())
                    .or_else(|| sess.sources.iter()
                        .find(|s| s.machine_id == machine && !s.name.is_empty())
                        .map(|s| s.name.clone()));
                let named = real_name.is_some();
                let name = real_name.unwrap_or_else(|| crate::i18n::msg::settings_plaintext_server().to_string());
                let connected = on && crate::plex::grant::granted_origin(&machine).is_some();
                PlaintextRowInput { machine: ServerMachineId(machine), name, named, on, connected }
            })
            .collect()
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(
                RouteLayout::screen(),
                None,
                crate::i18n::msg::settings_title(),
                crate::i18n::msg::settings_root_copy(),
            ),
            &self.form.table,
            GroupId(0),
            self.entry,
        )
        .keyed(&self.form)
    }

    /// Activate the row whose focus key is `key` (an `Activate` element or the RIGHT rule's).
    fn activate(&mut self, key: u32, directory: crate::stores::browse::DirectoryView<'_>,
        fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = form_activate(&self.form, key, fx) else {
            return;
        };
        match action {
            Action::Door => {}
            Action::AutoSignIn => {
                let on = !self.state.auto_sign_in;
                if let Ok(ticket) = crate::plex::session::queue_update_ticket(move |current|
                    (current.auto_sign_in() != on).then(|| current.with_auto_sign_in(on))) {
                    self.pending_auto = Some((on, ticket));
                }
                self.rebuild(directory);
            }
            Action::TrailerAutoplay => {
                let on = !self.state.trailer_autoplay;
                if let Ok(ticket) = crate::plex::session::queue_update_ticket(move |current|
                    (current.trailer_autoplay() != on).then(|| current.with_trailer_autoplay(on))) {
                    self.pending_trailer = Some((on, ticket));
                }
                self.rebuild(directory);
            }
            Action::Plaintext(ServerMachineId(machine)) => {
                use crate::plex::session::PlaintextChoice;
                let Some(on) = self.plaintext_rows.iter().find(|(m, _)| *m == machine).map(|(_, on)| *on) else {
                    return;
                };
                if !on {
                    // ON asks first — the same question the sign-in and the failure read-outs
                    // put, seated on *Not now*; only its *Connect* allows (`alert_answer`).
                    let sid = crate::plex::id_of_machine(&machine);
                    self.alert.open(&machine, sid, MachineId::Instance(InstanceId(0)), fx);
                    return;
                }
                // OFF is immediate: `grant::record` withdraws the grant NOW, before the
                // preferences write lands, and records the revocation for this account.
                crate::eventlog::log("settings: unencrypted connections turned off for one server");
                let account = crate::plex::grant::account_key(&self.session_snapshot.account_token);
                if crate::plex::grant::record(&account, &machine, PlaintextChoice::Revoked).is_ok() {
                    self.pending_plaintext = Some((machine, false));
                }
                self.rebuild(directory);
            }
        }
    }
}

impl RootPage {
    /// The question was answered: send the one command it became (a *Connect* shows the switch
    /// on at once; Session records it and re-finds the server), and hand focus back to the table.
    fn alert_answer(&mut self, cmd: Option<crate::auth::SessionCmd>, directory: crate::stores::browse::DirectoryView<'_>,
        fx: &mut Effects<'_, InnerHost>) {
        if let Some(cmd) = cmd {
            if let crate::auth::SessionCmd::AnswerPlaintext { machine_id, choice, .. } = &cmd {
                if choice.allows() {
                    self.pending_plaintext = Some((machine_id.clone(), true));
                }
            }
            fx.push(Fx::App(super::registry::AppFx::Session(cmd)));
        }
        plaintext_question::enter_group(fx, MachineId::Instance(InstanceId(0)), GroupId(0));
        self.rebuild(directory);
        fx.invalidate(Provenance::Input);
    }
}

impl Machine<InnerHost> for RootPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        cx: &Cx<'_, InnerHost>,
        fx: &mut Effects<'_, InnerHost>,
    ) -> Handled {
        match self.alert.step(ev, cx) {
            AlertStep::Pass => {}
            AlertStep::Done(handled) => return handled,
            AlertStep::Answer(cmd) => {
                self.alert_answer(cmd, cx.views, fx);
                return Handled::Yes;
            }
        }
        match ev {
            ScreenEvent::Enter(_) => {
                // a return from a child: the favourite count may have changed
                self.rebuild(cx.views);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                let mut landed = self.session_watch.changed();
                for pending in [&mut self.pending_auto, &mut self.pending_trailer] {
                    if pending.as_ref().is_some_and(|(_, ticket)| !matches!(ticket.try_recv(),
                        Err(std::sync::mpsc::TryRecvError::Empty))) {
                        // Read AFTER the receipt: the worker may have installed Locked/Blocked
                        // while this Tick was polling. Retain a consumed receipt's local value
                        // until authority settles; subsequent polls see Disconnected.
                        if let Some(snapshot) = crate::plex::session::peek_settled() {
                            self.session_snapshot = snapshot;
                            *pending = None;
                            landed = true;
                        }
                    }
                }
                // An offer or an answer landed (`grant::revision`): the switches re-read.
                let revision = crate::plex::grant::revision();
                if revision != self.grant_seen {
                    self.grant_seen = revision;
                    landed = true;
                }
                if self.alert.is_open() && !self.alert.subject().is_some_and(|m|
                    self.plaintext_rows.iter().any(|(row, on)| row == m && !on)) {
                    // the server the question is about left the list, or is on already
                    self.alert.withdraw();
                }
                self.alert.update(t.dt());
                if landed { self.rebuild(cx.views); fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session)); }

                self.form.table
                    .update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                self.activate(*e, cx.views, fx);
                Handled::Yes
            }
            ScreenEvent::Input(crate::ui::machine::InputEvent {
                kind:
                    crate::ui::machine::InputKind::Key {
                        key: Key::Right,
                        at_edge: true,
                        ..
                    },
                ..
            }) => {
                // rule 8: RIGHT on a row that opens nested content enters it, exactly as OK does
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, cx.views, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

/// The table's focus, unless the question is open — then its two answers ALONE (a modal traps
/// focus, §7.3 step 7), exactly as Privacy & data's delete alert.
impl Focusable<InnerHost> for RootPage {
    fn groups(&self, cx: &Cx<'_, InnerHost>, out: &mut Vec<GroupSpec>) {
        if !self.alert.groups(out) {
            Focusable::<InnerHost>::groups(&self.view(), cx, out)
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, InnerHost>) -> Option<GroupId> {
        match self.alert.group_of(*key) {
            Some(answer) => answer,
            None => Focusable::<InnerHost>::group_of(&self.view(), key, cx),
        }
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, InnerHost>) -> Step<u32> {
        match self.alert.neighbour(key, dir) {
            Some(step) => step,
            None => Focusable::<InnerHost>::neighbour(&self.view(), key, dir, cx),
        }
    }
    fn place(&self, key: &u32, cx: &Cx<'_, InnerHost>, at: At) -> Option<Placed> {
        match self.alert.place(*key) {
            Some(placed) => placed,
            None => Focusable::<InnerHost>::place(&self.view(), key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        if let Some(key) = self.alert.reconcile(want) {
            return key;
        }
        if self.alert.owns(want.elem) {
            return FocusKey { entry: self.entry, elem: self.state.sel.0 };
        }
        Focusable::<InnerHost>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, InnerHost>) -> FocusKey<u32> {
        match self.alert.seat(g, self.entry) {
            Some(key) => key,
            None => Focusable::<InnerHost>::seat(&self.view(), g, from, cx),
        }
    }
}

impl Screen<InnerHost> for RootPage {
    fn name(&self) -> &'static str {
        word::SETTINGS
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let mut v = self.view();
        crate::ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
        self.alert.draw(f, self.entry);
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
}

#[cfg(test)]
#[path = "settings_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "settings_draw_and_lifecycle_tests.rs"]
mod draw_and_lifecycle_tests;

#[cfg(test)]
#[path = "settings_text_fit_tests.rs"]
mod text_fit_tests;

#[cfg(test)]
#[path = "settings_root_navigation_tests.rs"]
mod root_navigation_tests;

#[cfg(test)]
#[path = "settings_pop_and_remember_tests.rs"]
mod pop_and_remember_tests;

#[cfg(test)]
#[path = "settings_composed_tests.rs"]
mod composed_tests;

#[cfg(test)]
#[path = "settings_nav_structure_tests.rs"]
mod nav_structure_tests;

// The picker persists an installation preference, while the immutable LocaleContext continues
// to render the current session. Choosing a language never remounts a screen or resets playback.
fn preference_name(preference: crate::i18n::Preference) -> &'static str {
    if preference == crate::i18n::Preference::System {
        crate::i18n::msg::settings_language_system()
    } else {
        preference.native_name()
    }
}

const LANGUAGES: [crate::i18n::Preference; 4] = [
    crate::i18n::Preference::System, crate::i18n::Preference::En,
    crate::i18n::Preference::Es, crate::i18n::Preference::Be,
];

/// A Language row's identity: the preference a `Choice` row saves, or the contribution guide.
#[derive(Clone, PartialEq, Eq, Debug)]
enum LangId {
    Choice(crate::i18n::Preference),
    Contribute,
}

impl FormId for LangId {
    fn key(&self) -> RowKey {
        use crate::i18n::Preference as P;
        RowKey(match self {
            LangId::Choice(P::System) => 0,
            LangId::Choice(P::En) => 1,
            LangId::Choice(P::Es) => 2,
            LangId::Choice(P::Be) => 3,
            LangId::Contribute => 4,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum LangAction {
    /// Save this preference (a `Choice` row).
    Pick(crate::i18n::Preference),
    /// The contribution row's slot: opening its page is [`RowKind::Nav`]'s job, so nothing
    /// dispatches this.
    Open,
}

/// The Language page's rows from plain inputs: one checked `Choice` per language, then the
/// contribution guide as a `Nav` row in its own section.
fn language_form(selected: crate::i18n::Preference, busy: bool) -> Form<LangId, LangAction, SettingsPage> {
    let mut choices = FormSection::new("");
    for language in LANGUAGES {
        choices = choices.item(
            LangId::Choice(language),
            RowKind::Choice,
            LangAction::Pick(language),
            Row::new(preference_name(language)).checked(language == selected).dim(busy),
        );
    }
    let contribution = FormSection::new("").item(
        LangId::Contribute,
        RowKind::Nav(SettingsPage::Contribute),
        LangAction::Open,
        Row::new(crate::i18n::msg::settings_language_contribute())
            .detail(crate::i18n::msg::settings_language_help())
            .chevron(true),
    );
    Form::new().section(choices).section(contribution)
}

struct LanguagePage {
    entry: EntryId,
    form: FormTable<LangId, LangAction, SettingsPage>,
    state: LanguageState,
    save: Option<(crate::i18n::Preference, std::sync::mpsc::Receiver<bool>)>,
}

struct LanguageState {
    selected: crate::i18n::Preference,
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
    failed: bool,
    busy: bool,
}

impl LogicalState for LanguageState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).u8(LANGUAGES.iter().position(|p| *p == self.selected).unwrap_or(0) as u8).bool(self.failed).bool(self.busy);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("language selected={} sel={} failed={} busy={}", self.selected.tag(), self.sel.0, self.failed, self.busy));
    }
}

impl LanguagePage {
    fn new(entry: EntryId) -> Self {
        let selected = crate::i18n::saved_preference();
        let sel = LangId::Choice(selected).key();
        let mut page = Self { entry, form: FormTable::new(super::registry::BAND), state: LanguageState { selected, sel, failed: false, busy: false }, save: None };
        page.rebuild();
        page
    }

    fn pending(&self) -> bool {
        self.state.selected != crate::i18n::current().preference()
    }

    /// Re-derive the rows, keeping the cursor on its row by identity (the saved language on the
    /// first build).
    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(LangId::Choice(self.state.selected));
        self.form.table.compact = false;
        self.form.set(language_form(self.state.selected, self.state.busy), Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        let copy = if self.state.busy {
            crate::i18n::msg::settings_language_saving()
        } else if self.state.failed {
            crate::i18n::msg::settings_language_save_failed()
        } else if self.pending() {
            crate::i18n::msg::settings_language_pending()
        } else {
            crate::i18n::msg::settings_language_copy()
        };
        TableScreen::new(Header::new(RouteLayout::screen(), Some(crate::i18n::msg::settings_title()),
            crate::i18n::msg::settings_language_title(), copy), &self.form.table, GroupId(0), self.entry).keyed(&self.form)
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        if let Some(LangAction::Pick(preference)) = form_activate(&self.form, key, fx) {
            if self.state.busy || preference == self.state.selected { return; }
            let (reply, receipt) = std::sync::mpsc::channel();
            self.save = Some((preference, receipt));
            self.state.busy = true;
            self.state.failed = false;
            fx.push(Fx::App(AppFx::Preferences(super::registry::PreferenceCmd::Language {
                language: preference, reply,
            })));
            self.rebuild();
        }
    }

    fn poll_save(&mut self) -> bool {
        let Some((preference, receipt)) = &self.save else { return false; };
        let success = match receipt.try_recv() {
            Ok(success) => success,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
        };
        if success { self.state.selected = *preference; }
        self.state.failed = !success;
        self.state.busy = false;
        self.save = None;
        self.rebuild();
        true
    }
}

impl Machine<InnerHost> for LanguagePage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                // The generic modal Enter runs before queued mount effects. Remembering a
                // row afterward cannot move its already-seated cursor, so request an explicit
                // Enter just as first-run consent does. A child return does not remount this
                // page and still restores the surface's remembered contribution row.
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey {
                            entry: self.entry,
                            elem: self.state.sel.0,
                        }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                if self.poll_save() { fx.invalidate(crate::ui::present::Provenance::Input); }
                self.form.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) { self.state.sel = key; }
                Handled::Yes
            }
            ScreenEvent::Activate(key) => { self.activate(*key, fx); Handled::Yes }
            ScreenEvent::Input(crate::ui::machine::InputEvent { kind: crate::ui::machine::InputKind::Key {
                key: Key::Right, at_edge: true, .. }, .. }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

crate::focusable_via_view!(LanguagePage, InnerHost, view);

impl Screen<InnerHost> for LanguagePage {
    fn name(&self) -> &'static str { "language" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(crate::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        crate::ui::screen::Part::<InnerHost>::draw(&mut self.view(), f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

#[cfg(test)]
#[path = "settings_language_tests.rs"]
mod language_tests;
