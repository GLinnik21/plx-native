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
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Effects, EntryId, FocusKey, Fx, GroupId, Handled, InstanceId, Key,
    LogicalState, Machine, MachineId, NavOp, PresentHandle, Stamped, Tick,
};
use crate::ui::motion;
use crate::ui::present::Provenance;
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::screen::{
    At, Dir, DrawFrame, Enter, FocusSource, FocusTarget, Focusable, GroupSpec, HitSource, Mounter,
    Placed, RenderStrategy, ReturnState, Screen, ScreenEvent, Step,
};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::table_screen::{Header, TableScreen};
use crate::ui::{theme, Painter, Rect};

use super::family::{inner_cx, table_focus, InnerHost, SettingsPage};
use super::registry::{word, DirectoryLike};

/// Which ceremony the surface carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Family {
    /// Settings, over a live page: scrim + ambient ground sampled off the host.
    Settings,
    /// The first-run consent question: a route surface of its own on the hero-keyed ground.
    FirstRunConsent,
}

/// The family's shared push constants (`route_screen::RoutePush`'s, unchanged).
const PUSH_K: f32 = 200.0;
const PARENT_TRAVEL: f32 = 0.35;
const CHILD_LEAD: f32 = 0.22;
const SCRIM_A: f32 = theme::alert::SCRIM_A;

/// The `Family::Settings` scrim's ink alpha: the surface's own appear (`local_alpha`, the
/// `RouteSurface`'s `page_alpha` after its container's overwrite) composed with the ROUTE-level
/// nav dip beneath it (`nav_page_alpha`, `DrawFrame::nav_page_alpha` — spec §14 phase 8), so the
/// scrim never reads as present-but-undimmed while a Home↔Library route change is still fading
/// underneath a Settings surface that is itself already fully open.
fn settings_scrim_alpha(local_alpha: f32, nav_page_alpha: f32) -> f32 {
    SCRIM_A * local_alpha * nav_page_alpha
}

/// The `Family::Settings` entrance cascade's alpha: same composition as
/// [`settings_scrim_alpha`], undivided by `SCRIM_A` — what the ground and the pages themselves
/// draw through.
fn settings_entrance_alpha(local_alpha: f32, nav_page_alpha: f32) -> f32 {
    local_alpha * nav_page_alpha
}

/// The push spring, with the page it is carrying OUT on a pop.
struct Push {
    pos: f32,
    vel: f32,
    target: f32,
    /// A popped body, drawn in the child role until the spring settles at 0.
    leaving: Option<Instance<InnerHost>>,
}

impl Push {
    const fn new() -> Self {
        Self {
            pos: 0.0,
            vel: 0.0,
            target: 0.0,
            leaving: None,
        }
    }
    fn amount(&self) -> f32 {
        self.pos.clamp(0.0, 1.0)
    }
    fn settled(&self) -> bool {
        (self.pos - self.target).abs() < 0.001 && self.vel.abs() < 0.02
    }
    fn parent(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(1.0 - t)
            .translate(-PARENT_TRAVEL * Rect::FULL.w * t, 0.0)
    }
    fn child(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(t)
            .translate(CHILD_LEAD * Rect::FULL.w * (1.0 - t), 0.0)
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
        s.inner.request(NavOp::Root(root), ReturnState::default());
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
    /// (`app/run.rs`'s boot-target match). A `RELEASE` build compiles `dev::read` out and always
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
            self.push.pos = 1.0;
            self.push.target = 0.0;
        } else {
            self.push.leaving = None;
            self.push.pos = 0.0;
            self.push.target = 1.0;
        }
        self.push.vel = 0.0;
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
                .unwrap_or(FocusTarget::ContainerGroup(GroupId(0))),
            _ => FocusTarget::ContainerGroup(GroupId(0)),
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
        SettingsPage::Legal => Box::new(super::legal::LegalIndex::new(entry)),
        SettingsPage::About => Box::new(super::legal::DocumentPage::about(entry)),
        SettingsPage::Document(i) => Box::new(super::legal::DocumentPage::legal(entry, i)),
        SettingsPage::Privacy => Box::new(super::consent::ConsentPage::settings(entry, cx, fx)),
        SettingsPage::Preview(i) => Box::new(super::consent::PreviewPage::new(entry, i)),
        SettingsPage::ConsentStage(i) => {
            Box::new(super::consent::ConsentPage::first_run(entry, i, cx, fx))
        }
        SettingsPage::Favourites => Box::new(super::onboard::OnboardScreen::settings(entry, cx.views)),
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
///  * `RootPage` → `RootState`: the selected row, Automatically Sign In, and trailer autoplay.
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
            motion::spring(
                &mut self.push.pos,
                &mut self.push.vel,
                self.push.target,
                PUSH_K,
                t,
                &mut ph,
            );
            if self.push.settled() {
                self.push.pos = self.push.target;
                self.push.vel = 0.0;
                if self.push.target == 0.0 {
                    self.push.leaving = None;
                }
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
        // `super::family` here matches this file's own `use super::family::{inner_cx, table_focus,
        // InnerHost, SettingsPage};` above — `family` is shared vocabulary, not a sibling screen.
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Favourites,
    Privacy,
    Legal,
    AutoSignIn,
    TrailerAutoplay,
    About,
}

/// The Settings root: a table of destinations, every row a door (no band; rule 9 in full).
pub(crate) struct RootPage {
    entry: EntryId,
    table: TableView,
    rows: Vec<Action>,
    state: RootState,
}

struct RootState {
    sel: i32,
    auto_sign_in: bool,
    trailer_autoplay: bool,
}

impl LogicalState for RootState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel as u32).bool(self.auto_sign_in).bool(self.trailer_autoplay);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "root sel={} auto_sign_in={} trailer_autoplay={}",
            self.sel, self.auto_sign_in, self.trailer_autoplay
        ));
    }
}

/// Does this television have an account? — asked by [`RootPage::rebuild`], so twice per Settings
/// open (construction, then `ScreenEvent::Enter`) and once more on every return from a child page.
///
/// **Through [`peek`](crate::plex::session::peek), never [`load`](crate::plex::session::load).**
/// The two differ in exactly one respect and it is the one that matters on a press path: `load`
/// mints a `client_id` when there is none and re-persists a plaintext session, so a READ turns
/// into `write_atomic` — a temp file, `sync_all`, a rename and a second `sync_all` on the
/// directory. That is the boot path's bargain, and `session.rs` says so in as many words ("it is
/// not one on a path a keypress can reach", "do not add a per-frame reader of this file"). This
/// call site was on the wrong side of it: on a television whose key manager is unusable — which
/// is this one — every open of the Settings modal paid two flash writes with four fsyncs on the
/// frame that mounts it, worth 150-180 ms of `navcommit` in the sessions where the flash was slow
/// (`fps:modal-ramp`, device-measured 2026-09-09;
/// `opening_settings_never_writes_the_session_file` is the account).
fn signed_in() -> bool {
    crate::plex::session::peek()
        .account(crate::plex::session::current().as_ref())
        .signed_in
}


impl RootPage {
    fn new(entry: EntryId, directory: crate::stores::browse::DirectoryView<'_>) -> Self {
        let mut s = Self {
            entry,
            table: TableView::new(),
            rows: Vec::new(),
            state: RootState {
                sel: 0,
                auto_sign_in: false,
                trailer_autoplay: true,
            },
        };
        s.rebuild(0, directory);
        s
    }

    fn rebuild(&mut self, sel: i32, directory: crate::stores::browse::DirectoryView<'_>) {
        let sess = crate::plex::session::peek();
        let signed_in = signed_in();
        let auto_sign_in = sess.auto_sign_in();
        let trailer_autoplay = sess.trailer_autoplay();
        let multi_user = sess.home_users.len() > 1;
        self.state.auto_sign_in = auto_sign_in;
        self.state.trailer_autoplay = trailer_autoplay;

        let mut actions = Vec::new();
        let mut sections = Vec::new();
        if signed_in {
            let n = directory.pinned_count();
            // The section is Libraries and the row is Favorite libraries: the switch governs the
            // whole app — Home's shelves, the top tab strip and the Library's Sources picker.
            sections.push(
                Section::new("Libraries").row(
                    Row::new("Favorite libraries")
                        .detail("Which libraries this television shows.")
                        .value(format!(
                            "{n} {}",
                            if n == 1 { "favorite" } else { "favorites" }
                        ))
                        .chevron(true),
                ),
            );
            actions.push(Action::Favourites);
        }
        sections.push(
            Section::new("Privacy")
                .row(
                    Row::new("Privacy & data")
                        .detail("Optional reports, privacy information and local data.")
                        .chevron(true),
                )
                .row(
                    Row::new("Legal notices")
                        .detail("Privacy, licences, source code, trademarks and contact.")
                        .chevron(true),
                ),
        );
        actions.extend([Action::Privacy, Action::Legal]);
        let mut system = Section::new("System");
        // A one-person account already skips the picker; the switch only changes a multi-user boot.
        if signed_in && multi_user {
            system = system.row(
                Row::new("Automatically Sign In")
                    .detail("Skip the profile list and enter as this profile when the app starts.")
                    .toggle(auto_sign_in),
            );
            actions.push(Action::AutoSignIn);
        }
        if signed_in {
            system = system.row(
                Row::new("Play trailers automatically")
                    .detail("After a few seconds on a title, play its trailer in the background. This is also the sound control.")
                    .toggle(trailer_autoplay),
            );
            actions.push(Action::TrailerAutoplay);
        }
        system = system.row(
            Row::new("About PlxNative")
                .detail("Version, copyright and project information.")
                .chevron(true),
        );
        sections.push(system);
        actions.push(Action::About);
        self.rows = actions;
        self.table.compact = false;
        self.table.header_ink = theme::TEXT_READING;
        self.table.set_sections(sections, sel, false);
        self.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(
                RouteLayout::screen(),
                None,
                "Settings",
                "Settings apply to this Plex profile on this television. You can return here from the profile menu at any time.",
            ),
            &self.table,
            GroupId(0),
            self.entry,
        )
    }

    fn activate(&mut self, row: i32, directory: crate::stores::browse::DirectoryView<'_>,
        fx: &mut Effects<'_, InnerHost>) {
        let Some(&action) = usize::try_from(row).ok().and_then(|i| self.rows.get(i)) else {
            return;
        };
        match action {
            Action::AutoSignIn => {
                crate::plex::session::set_auto_sign_in(!self.state.auto_sign_in);
                self.rebuild(self.table.sel, directory);
            }
            Action::TrailerAutoplay => {
                crate::plex::session::set_trailer_autoplay(!self.state.trailer_autoplay);
                self.rebuild(self.table.sel, directory);
            }
            Action::Favourites => fx.push(Fx::Nav(NavOp::Push(SettingsPage::Favourites))),
            Action::Privacy => fx.push(Fx::Nav(NavOp::Push(SettingsPage::Privacy))),
            Action::Legal => fx.push(Fx::Nav(NavOp::Push(SettingsPage::Legal))),
            Action::About => fx.push(Fx::Nav(NavOp::Push(SettingsPage::About))),
        }
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
        match ev {
            ScreenEvent::Enter(_) => {
                // a return from a child: the favourite count may have changed
                let sel = self.table.sel;
                self.rebuild(sel, cx.views);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.table
                    .update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                table_focus(&mut self.table, to.elem);
                self.state.sel = self.table.sel;
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                self.activate(*e as i32, cx.views, fx);
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
                if let Some(k) = cx.focus.current {
                    if self.table.row_opens(k.elem as i32) {
                        self.activate(k.elem as i32, cx.views, fx);
                    }
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

crate::focusable_via_view!(RootPage, InnerHost, view);

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
#[path = "settings_root_navigation_tests.rs"]
mod root_navigation_tests;

#[cfg(test)]
#[path = "settings_pop_and_remember_tests.rs"]
mod pop_and_remember_tests;

#[cfg(test)]
#[path = "settings_composed_tests.rs"]
mod composed_tests;
