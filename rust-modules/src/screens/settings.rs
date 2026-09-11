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
    At, Dir, DrawFrame, Enter, Focusable, FocusSource, FocusTarget, GroupSpec, HitSource, Mounter,
    Placed, RenderStrategy, ReturnState, Screen, ScreenEvent, Step,
};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::table_screen::{Header, TableScreen};
use crate::ui::{theme, Painter, Rect};

use super::family::{inner_cx, table_focus, InnerHost, SettingsPage};
use super::registry::{word, AppLike};

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
        p.alpha(1.0 - t).translate(-PARENT_TRAVEL * Rect::FULL.w * t, 0.0)
    }
    fn child(&self, p: Painter) -> Painter {
        let t = self.amount();
        p.alpha(t).translate(CHILD_LEAD * Rect::FULL.w * (1.0 - t), 0.0)
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
    pub(crate) fn new(entry: EntryId, id: InstanceId, kind: Family, root: SettingsPage) -> Self {
        let mut s = Self {
            entry,
            id,
            kind,
            inner: NavStack::new(Box::new(Immediate)),
            ids: Minter::default(),
            push: Push::new(),
            ground: if kind == Family::FirstRunConsent { super::family::pre_home_ground() } else { RouteGround::new() },
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
    fn run_inner<H: AppLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let steps = self.inner.commit(&mut self.ids);
        for step in steps {
            match step {
                Life::Mount(eid) => {
                    let inst_id = self.ids.instance();
                    let entry = self.entry;
                    let icx = inner_cx(cx);
                    let mut out: Vec<Stamped<InnerHost>> = Vec::new();
                    let screen = {
                        let mut ifx = Effects::from_handle(&mut out, MachineId::Instance(inst_id), fx.present());
                        let arg = self.inner.entry(eid).map(|e| e.arg).unwrap_or(SettingsPage::Root);
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
    fn deliver<H: AppLike>(&mut self, eid: EntryId, ev: ScreenEvent<InnerHost>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let mut out: Vec<Stamped<InnerHost>> = Vec::new();
        let handled = {
            let icx = inner_cx(cx);
            let Some(inst) = self.inner.entry_mut(eid).and_then(|e| e.inst.as_mut()) else {
                return Handled::No;
            };
            let mut ifx = Effects::from_handle(&mut out, MachineId::Instance(inst.id), fx.present());
            inst.screen.step(&ev, &icx, &mut ifx)
        };
        self.forward(out, cx, fx);
        handled
    }

    /// Step the TOP page.
    fn step_top<H: AppLike>(&mut self, ev: ScreenEvent<InnerHost>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match self.inner.top().map(|e| e.id) {
            Some(eid) => self.deliver(eid, ev, cx, fx),
            None => Handled::No,
        }
    }

    /// An inner page's emissions, translated to the outer sink: a `Nav` op is the inner stack's
    /// (§6.2), everything else passes through unchanged (same bundle, same element type).
    fn forward<H: AppLike>(&mut self, out: Vec<Stamped<InnerHost>>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
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
    fn request<H: AppLike>(&mut self, op: NavOp<SettingsPage>, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
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
        self.remembered.retain(|(e, _)| self.inner.entry(*e).is_some());
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
                .map(|(_, elem)| FocusTarget::Elem(FocusKey { entry: self.entry, elem: *elem }))
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
fn mount_page(entry: EntryId, arg: SettingsPage, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Box<dyn Screen<InnerHost>> {
    match arg {
        SettingsPage::Root => Box::new(RootPage::new(entry)),
        SettingsPage::Legal => Box::new(super::legal::LegalIndex::new(entry)),
        SettingsPage::About => Box::new(super::legal::DocumentPage::about(entry)),
        SettingsPage::Document(i) => Box::new(super::legal::DocumentPage::legal(entry, i)),
        SettingsPage::Privacy => Box::new(super::consent::ConsentPage::settings(entry, cx, fx)),
        SettingsPage::Preview(i) => Box::new(super::consent::PreviewPage::new(entry, i)),
        SettingsPage::ConsentStage(i) => Box::new(super::consent::ConsentPage::first_run(entry, i, cx, fx)),
        SettingsPage::Favourites => Box::new(super::onboard::OnboardScreen::settings(entry)),
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
///  * `RootPage` → `RootState`: the selected row.
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

impl<H: AppLike> Machine<H> for RouteSurface {
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
                self.step_top(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GroupId(0)) }), cx, fx);
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
                let back = matches!(iev.kind, crate::ui::machine::InputKind::Key { key: Key::Back, edge: crate::ui::machine::Edge::Down, .. });
                if back && self.inner.depth() > 1 {
                    self.request(NavOp::Pop, cx, fx);
                    return Handled::Yes;
                }
                Handled::No
            }
            ScreenEvent::FocusMoved { from, to, by } => {
                self.step_top(ScreenEvent::FocusMoved { from: *from, to: *to, by: *by }, cx, fx);
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
    fn tick<H: AppLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if !self.push.settled() {
            let mut ph: PresentHandle<'_> = fx.present();
            motion::spring(&mut self.push.pos, &mut self.push.vel, self.push.target, PUSH_K, t, &mut ph);
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
                let mut ifx = Effects::from_handle(&mut out, MachineId::Instance(inst.id), fx.present());
                inst.screen.step(&ScreenEvent::Tick(t), &icx, &mut ifx);
            }
        }
        self.forward(out, cx, fx);
    }
}

impl<H: AppLike> Focusable<H> for RouteSurface {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if let Some(top) = self.top() {
            top.screen.groups(&inner_cx(cx), out);
        }
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        self.top().and_then(|t| t.screen.group_of(key, &inner_cx(cx)))
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        self.top().map_or(Step::Edge, |t| t.screen.neighbour(key, dir, &inner_cx(cx)))
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        self.top().and_then(|t| t.screen.place(key, &inner_cx(cx), at))
    }
    fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, H>) -> FocusKey<u32> {
        self.top().map_or(want, |t| t.screen.reconcile(want, &inner_cx(cx)))
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

impl<H: AppLike> Screen<H> for RouteSurface {
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
    fn draw_pages<H: AppLike>(&mut self, f: &mut DrawFrame<'_, '_, H>, entrance: Painter) {
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
                let child = if popping { self.push.leaving.as_mut() } else { self.top_mut() };
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
    fn mount(&mut self, _id: InstanceId, arg: &SettingsPage, _ret: &ReturnState<u32>, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Box<dyn Screen<InnerHost>> {
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
}

impl LogicalState for RootState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel as u32);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("root sel={}", self.sel));
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
    fn new(entry: EntryId) -> Self {
        let mut s = Self {
            entry,
            table: TableView::new(),
            rows: Vec::new(),
            state: RootState { sel: 0 },
        };
        s.rebuild(0);
        s
    }

    fn rebuild(&mut self, sel: i32) {
        let mut actions = Vec::new();
        let mut sections = Vec::new();
        if signed_in() {
            let n = crate::browse::pinned_count();
            // The section is Libraries and the row is Favorite libraries: the switch governs the
            // whole app — Home's shelves, the top tab strip and the Library's Sources picker.
            sections.push(
                Section::new("Libraries").row(
                    Row::new("Favorite libraries")
                        .detail("Which libraries this television shows.")
                        .value(format!("{n} {}", if n == 1 { "favorite" } else { "favorites" }))
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
        sections.push(
            Section::new("System").row(
                Row::new("About PlxNative")
                    .detail("Version, copyright and project information.")
                    .chevron(true),
            ),
        );
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

    fn open(&self, row: i32, fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = usize::try_from(row).ok().and_then(|i| self.rows.get(i)) else {
            return;
        };
        let page = match action {
            Action::Favourites => SettingsPage::Favourites,
            Action::Privacy => SettingsPage::Privacy,
            Action::Legal => SettingsPage::Legal,
            Action::About => SettingsPage::About,
        };
        fx.push(Fx::Nav(NavOp::Push(page)));
    }
}

impl Machine<InnerHost> for RootPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Enter(_) => {
                // a return from a child: the favourite count may have changed
                let sel = self.table.sel;
                self.rebuild(sel);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.table.update(t.dt(), RouteLayout::screen().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                table_focus(&mut self.table, to.elem);
                self.state.sel = self.table.sel;
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                self.open(*e as i32, fx);
                Handled::Yes
            }
            ScreenEvent::Input(crate::ui::machine::InputEvent {
                kind: crate::ui::machine::InputKind::Key { key: Key::Right, at_edge: true, .. },
                ..
            }) => {
                // rule 8: RIGHT on a row that opens nested content enters it, exactly as OK does
                if let Some(k) = cx.focus.current {
                    if self.table.row_opens(k.elem as i32) {
                        self.open(k.elem as i32, fx);
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
mod tests {
    //! **No SDL, no GL, no `Dispatcher`.** `RouteSurface` is generic over any `H: AppLike`
    //! (`registry::AppLike`'s blanket impl), and `family::InnerHost` already satisfies that bound
    //! — the exact fact that lets the Settings family mount its own pages a second time inside
    //! the surface (§6.2). So a test here drives `RouteSurface` as `Machine<InnerHost>` directly,
    //! with a hand-built `Cx`/`Effects` standing in for the outer dispatcher, and reads the
    //! effects it emits — the same shape `ui/fixture.rs` and `containers/tests.rs` use, minus the
    //! `Dispatcher` itself, which only `app/bridge.rs`'s `AppHost` can stand up (its `Arg` carries
    //! the legacy `Route`, which this layer may not name). What this style CANNOT see is
    //! anything the real focus ENGINE would do — a raw `Key::Down` here moves nothing, because
    //! there is no engine in this harness to turn it into a `FocusMoved`; the tests below drive
    //! the engine's own primitives (`FocusMoved`, `Activate`) directly instead, which is what
    //! `bridge.rs`'s own `the_settings_surface_owns_input_and_walks_its_own_stack` cannot do from
    //! outside `app/`, since it drives real keys through the real engine and never inspects a
    //! `FocusKey` at all.

    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    // `By` is the odd one out and the split is deliberate rather than untidy: the other seven
    // names really are `ui::machine`'s, but `By` — how a focus move was CAUSED (a direction key,
    // a pointer, a restore) — belongs to `ui::screen` beside `ScreenEvent::FocusMoved`, the only
    // thing that carries one. Writing it as `ui::machine::By` compiles nowhere and is invisible
    // to every non-test gate, since this module is `cfg(test)`.
    use crate::ui::machine::{Edge, FocusRead, InputEvent, InputKind, InputOwner, PressRead, Source};
    use crate::ui::screen::By;
    use crate::ui::present::Present;

    /// Spec §14 phase 8: `Family::Settings`'s scrim/entrance composition reads
    /// `DrawFrame::nav_page_alpha` rather than the `ui::nav` statics — these two pin the
    /// arithmetic itself (the wiring at the two call sites is a straight field read, checked by
    /// the compiler and by every existing draw test in this module staying green). A route dip
    /// in flight (a non-1.0 `nav_page_alpha`) must dim the scrim and the entrance exactly as
    /// much as the surface's own appear does — before this field existed, both call sites read
    /// the live global instead of whatever a host test's `DrawFrame` carried, so a test built on
    /// the OLD shape could not have told a wired composition from an ignored parameter; a
    /// process-wide static is either at rest (1.0, indistinguishable from the identity) or being
    /// driven by a second test racing this one (`testlock::serial()`'s whole reason for existing
    /// — see `docs/../test-suite-global-pollution.md`), never a controlled non-1.0 value a test
    /// can set.
    #[test]
    fn settings_scrim_and_entrance_alpha_compose_local_and_nav_page_alpha() {
        assert_eq!(settings_scrim_alpha(1.0, 1.0), SCRIM_A);
        assert_eq!(settings_entrance_alpha(1.0, 1.0), 1.0);
        // the surface is fully open (local 1.0) but the route beneath it is mid-dip (0.5): both
        // the scrim and the entrance must read the dip, not just the surface's own appear.
        assert_eq!(settings_scrim_alpha(1.0, 0.5), SCRIM_A * 0.5);
        assert_eq!(settings_entrance_alpha(1.0, 0.5), 0.5);
        // the surface is itself still appearing (local 0.5) over a route at rest (1.0).
        assert_eq!(settings_scrim_alpha(0.5, 1.0), SCRIM_A * 0.5);
        assert_eq!(settings_entrance_alpha(0.5, 1.0), 0.5);
        // both in flight at once multiply, never clamp or pick a max.
        assert_eq!(settings_scrim_alpha(0.5, 0.4), SCRIM_A * 0.2);
        assert_eq!(settings_entrance_alpha(0.5, 0.4), 0.2);
    }

    // A `static`, not a `const`: `Cx::measure` needs a genuine `&'static dyn Measure`, and a
    // `static` gives one outright rather than leaning on constant-promotion rules at the borrow
    // site inside `cx` below.
    static MEASURE: FixtureMeasure = FixtureMeasure;

    fn cx(focus: Option<FocusKey<u32>>) -> Cx<'static, InnerHost> {
        Cx {
            views: (),
            tick: Tick::default(),
            measure: &MEASURE,
            press: PressRead::default(),
            focus: FocusRead { current: focus , ..Default::default() },
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    /// Step the surface once and return what it emitted, the way `RouteSurface::forward` would
    /// hand effects up to whatever mounted it.
    fn step(s: &mut RouteSurface, ev: ScreenEvent<InnerHost>, focus: Option<FocusKey<u32>>) -> Vec<Stamped<InnerHost>> {
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        let c = cx(focus);
        let _ = <RouteSurface as Machine<InnerHost>>::step(s, &ev, &c, &mut fx);
        out
    }

    fn name(s: &RouteSurface) -> &'static str {
        <RouteSurface as Screen<InnerHost>>::name(s)
    }

    struct DrawProbe {
        id: u32,
        seen: std::rc::Rc<std::cell::RefCell<Vec<(u32, crate::ui::screen::NavPresentation)>>>,
    }

    impl Machine<InnerHost> for DrawProbe {
        type Ev = ScreenEvent<InnerHost>;
        fn step(&mut self, _: &Self::Ev, _: &Cx<'_, InnerHost>, _: &mut Effects<'_, InnerHost>) -> Handled {
            Handled::No
        }
    }

    impl Focusable<InnerHost> for DrawProbe {
        fn groups(&self, _: &Cx<'_, InnerHost>, _: &mut Vec<GroupSpec>) {}
        fn group_of(&self, _: &u32, _: &Cx<'_, InnerHost>) -> Option<GroupId> { None }
        fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, InnerHost>) -> Step<u32> { Step::Edge }
        fn place(&self, _: &u32, _: &Cx<'_, InnerHost>, _: At) -> Option<Placed> { None }
        fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, InnerHost>) -> FocusKey<u32> { want }
        fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, InnerHost>) -> FocusKey<u32> {
            panic!("draw-only probe cannot be seated")
        }
    }

    impl Screen<InnerHost> for DrawProbe {
        fn name(&self) -> &'static str { "draw-probe" }
        fn state(&self) -> &dyn LogicalState { &() }
        fn crumb(&self, _: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { None }
        fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, InnerHost>) {}
        fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
            self.seen.borrow_mut().push((self.id, crate::ui::screen::NavPresentation {
                page_alpha: f.page_alpha,
                chrome_alpha: f.chrome_alpha,
                view_tab: f.view_tab,
                blur_amount: f.blur_amount,
            }));
        }
        fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    }

    #[test]
    fn nested_draw_preserves_navigation_at_rest_and_through_push_and_pop() {
        use crate::ui::containers::stack::Entry;
        use crate::ui::screen::NavPresentation;
        use std::{cell::RefCell, rc::Rc};

        let navigation = NavPresentation {
            page_alpha: 0.21, chrome_alpha: 0.37, view_tab: Some(2), blur_amount: 0.63,
        };
        let cases: &[(&str, f32, f32, bool, &[u32])] = &[
            ("rest after pop", 0.0, 0.0, false, &[2]),
            ("rest after push", 1.0, 1.0, false, &[2]),
            ("mid-push", 0.5, 1.0, false, &[1, 2]),
            ("mid-pop", 0.5, 0.0, true, &[2, 3]),
        ];
        let mut actual = Vec::new();
        let mut expected = Vec::new();
        for &(label, pos, target, popping, order) in cases {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let instance = |id| Instance {
                id: InstanceId(id),
                screen: Box::new(DrawProbe { id, seen: Rc::clone(&seen) }) as Box<dyn Screen<InnerHost>>,
                inflight: Vec::new(),
            };
            // Install inert bodies directly: no real page construction, auth/session reads or
            // lifecycle side effects. Both resting cases retain a page underneath the top.
            let mut inner = NavStack::new(Box::new(Immediate));
            for id in [1, 2] {
                inner.entries.push(Entry {
                    id: EntryId(id), arg: SettingsPage::Root, ret: ReturnState::default(),
                    inst: Some(instance(id)), evicted: false,
                });
            }
            let mut surface = RouteSurface {
                entry: EntryId(0), id: InstanceId(0), kind: Family::Settings,
                inner, ids: Minter::default(),
                push: Push { pos, vel: 0.0, target, leaving: popping.then(|| instance(3)) },
                ground: RouteGround::new(), ground_ready: false, remembered: Vec::new(),
            };
            let outer = cx(None);
            let c = inner_cx(&outer);
            let mut f = DrawFrame::with_navigation(&c, Painter::root(), navigation);
            let before = LogicalState::hash(&surface);
            surface.draw_pages(&mut f, Painter::root());
            assert_eq!(LogicalState::hash(&surface), before, "{label}: draw changed logical state");
            actual.push((label, seen.borrow().clone()));
            expected.push((label, order.iter().map(|&id| (id, navigation)).collect::<Vec<_>>()));
        }
        assert_eq!(actual, expected, "every nested draw path must inherit the outer snapshot");
    }

    /// **Tests mounting a real root page need a scratch session, because `RootPage::rebuild` asks
    /// whether this television is signed in and the row set DEPENDS ON THE ANSWER** — signed in, a `Libraries`
    /// section with *Favorite libraries* is prepended, so row 1 stops being *Legal notices* and
    /// becomes *Privacy & data*. Without the redirect that question is answered by whatever
    /// `auth.json` happens to be on the machine running `make check`, so the two tests below that
    /// press row 1 passed on a runner and failed on the maintainer's own Mac — a real signal that
    /// reads exactly like flakiness. `session::TempSession` writes a file with no account token
    /// and no dialable server, i.e. deterministically SIGNED OUT, which is the state the row
    /// comments here already assume. The caller must hold `testlock::serial()` for its whole body
    /// (the redirected path is a crate global); every test below takes it first.
    fn scratch_session(tag: &str) -> crate::plex::session::TempSession {
        crate::plex::session::TempSession::new(tag)
    }

    /// **OPENING SETTINGS MUST NOT WRITE THE SESSION FILE.** Device-measured, 2026-09-09:
    /// `fps:modal-ramp` (open and dismiss the Settings modal every 1500 ms) read a `worstframe`
    /// of 188-210 ms against a 75 ms ceiling, with `FRAMEDROP` putting 150-181 ms of it in
    /// `navcommit=` — the dispatcher's POST-COMMIT DRAIN, which is where this surface's mount and
    /// its root page's `ScreenEvent::Enter` run.
    ///
    /// [`RootPage::rebuild`] asks [`signed_in`] whether this television has an account, once at
    /// construction and again on `Enter`, so TWICE per open. That question used to go through
    /// [`crate::plex::session::load`] — the read-modify-WRITE door, whose own doc says a read that
    /// can turn into a save "is not [an acceptable trade] on a path a keypress can reach", and
    /// "do not add a per-frame reader of this file". On this television the key manager is
    /// unusable ("session protection: no usable key manager; using the 0600 file fallback"), so
    /// every `load` takes the plaintext branch and re-persists: `write_atomic`, i.e. a temp file,
    /// `sync_all`, a rename and a second `sync_all` on the directory. Two flash writes with four
    /// fsyncs, synchronously, on the frame that opens the modal. Instrumented on the set the same
    /// day: 23 `load`s in one 18 s run, `saves=1 plaintext=1` on every one, 6-15 ms each in that
    /// session and ~75 ms each in the sessions that failed — which is also why the symptom is
    /// BIMODAL, and why it bisected to a range containing no functional change at all.
    ///
    /// The assertion is the file's INODE, not its mtime: `write_atomic` renames a fresh temp file
    /// into place, so a write always moves it, whatever a filesystem's timestamp resolution.
    /// Watched red against `session::load()` — the inode changed on the mount.
    #[test]
    fn opening_settings_never_writes_the_session_file() {
        use std::os::unix::fs::MetadataExt;
        let _g = crate::testlock::serial();
        let sess = scratch_session("surface-no-session-write");
        let file = sess.path();
        let before = std::fs::metadata(&file).expect("the scratch session exists");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let after = std::fs::metadata(&file).expect("the scratch session still exists");
        assert_eq!(
            before.ino(),
            after.ino(),
            "opening Settings rewrote the session file: a flash write with two fsyncs on the \
             frame the modal mounts (fps:modal-ramp, 150 ms of navcommit)"
        );
        assert_eq!(before.len(), after.len(), "and nothing about its contents moved either");
    }

    /// Mounting the surface at its `Root` page runs the inner stack's own lifecycle (§3.4) and
    /// names the root — the heartbeat's `overlay=` before anything has been pressed.
    #[test]
    fn mounting_the_surface_names_its_root_page() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-mount");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        assert_eq!(name(&s), word::SETTINGS);
        assert_eq!(s.inner.depth(), 1);
        assert_eq!(s.kind, Family::Settings);
    }

    /// **BACK at the surface's own root does not touch the inner stack, and it is not swallowed
    /// either** — `Handled::No` is exactly what tells the CONTAINER (the outer `ModalStack`) it
    /// may dismiss the surface. `bridge.rs`'s own test asserts the CONSEQUENCE of this one level
    /// up (the surface's phase goes to `Closing`); this is the return value that consequence is
    /// built on.
    #[test]
    fn back_at_the_surface_s_own_root_is_not_handled() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-back-root");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Sdl,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        });
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        let handled = <RouteSurface as Machine<InnerHost>>::step(&mut s, &back, &cx(None), &mut fx);
        assert_eq!(handled, Handled::No, "the surface's own stack has nothing to pop at depth 1");
        assert_eq!(s.inner.depth(), 1, "…and nothing about the stack moved while deciding that");
    }

    /// **The remembered-focus round trip (spec §7.3 step 4).** Signed out, the root's rows are
    /// Privacy & data / Legal notices / About PlxNative (`bridge.rs`'s own comment on the same
    /// fixture: "Favourites is absent signed out"), so row 1 is Legal notices. The engine seats
    /// focus there, OK pushes the index, and a BACK must hand focus back to THAT row — not row 0
    /// — which is the one thing `bridge.rs`'s word-only assertions cannot see from outside `app/`.
    #[test]
    fn a_pop_from_legal_restores_focus_to_the_row_that_opened_it() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-pop-focus");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);

        let legal_row = FocusKey { entry: EntryId(0), elem: 1 };
        step(
            &mut s,
            ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir },
            Some(legal_row),
        );
        step(&mut s, ScreenEvent::Activate(legal_row.elem), Some(legal_row));
        assert_eq!(name(&s), word::LEGAL, "OK on Legal notices pushed the index");
        assert_eq!(s.inner.depth(), 2);

        let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Sdl,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        });
        let out = step(&mut s, back, None);
        assert_eq!(name(&s), word::SETTINGS, "BACK popped the inner stack, not the surface");
        assert_eq!(s.inner.depth(), 1);

        let reseat = out.iter().find_map(|st| match &st.fx {
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) }))) => Some(*k),
            _ => None,
        });
        assert_eq!(
            reseat,
            Some(legal_row),
            "BACK from Legal must ask the engine to re-seat the row that opened it, not the first row"
        );
    }

    /// The complementary case: a PUSH always asks for a fresh seat on the new page's own first
    /// group, never the remembered list — a remembered entry belongs to the page being LEFT, and
    /// reusing it for the page being ENTERED would seat the Legal index on whatever numeric row
    /// happened to be focused on the root.
    #[test]
    fn a_push_seats_the_new_page_fresh_rather_than_from_the_remembered_list() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-push-seat");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let root_row = FocusKey { entry: EntryId(0), elem: 1 };
        step(&mut s, ScreenEvent::FocusMoved { from: None, to: root_row, by: By::Dir }, Some(root_row));
        let out = step(&mut s, ScreenEvent::Activate(root_row.elem), Some(root_row));
        let seat = out.iter().find_map(|st| match &st.fx {
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) => Some(*focus),
            _ => None,
        });
        assert!(
            matches!(seat, Some(FocusTarget::ContainerGroup(GroupId(0)))),
            "a push seats the destination's own group 0, not a remembered element: {seat:?}"
        );
    }

    /// **`remembered` must not grow for the whole life of a Settings session.** Every push and
    /// every pop records one entry; without the retire-time cleanup in `request`, opening and
    /// closing Legal a few times would leave stale rows behind for entries the container has
    /// already dropped for good, because a popped page's `EntryId` is never minted again.
    #[test]
    fn remembered_does_not_grow_across_repeated_visits_to_the_same_page() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-remembered");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let legal_row = FocusKey { entry: EntryId(0), elem: 1 };
        for _ in 0..5 {
            step(&mut s, ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir }, Some(legal_row));
            step(&mut s, ScreenEvent::Activate(legal_row.elem), Some(legal_row));
            assert_eq!(name(&s), word::LEGAL);
            let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
                at: Tick::default(),
                source: Source::Sdl,
                kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
            });
            step(&mut s, back, None);
            assert_eq!(name(&s), word::SETTINGS);
        }
        assert!(
            s.remembered.len() <= 1,
            "five open/close cycles through the same page left {} remembered entries, want at most the live root",
            s.remembered.len()
        );
    }

    /// Run the push spring to rest on 16 ms frames — bounded, so a spring that never settles
    /// fails the test rather than hanging the suite.
    fn settle(s: &mut RouteSurface) {
        for i in 1..600u32 {
            step(s, ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_000 }), None);
            if s.at_rest() {
                return;
            }
        }
        panic!("the push spring never settled");
    }

    /// **A pop that has finished leaves the surface AT REST at depth two — which is the state the
    /// draw used to get wrong.** Settings root → Legal notices → a legal document → BACK: the
    /// reverse push runs 1 → 0, and once it lands the ONE page on screen is the top, the Legal
    /// index. `draw` decided that case as "there is no page below me", which is only the same
    /// question at depth 1 — so with the Settings root still under the index it took the PARENT
    /// branch instead and drew the root at full strength while the index the user had just
    /// returned to was never drawn at all, hit map included.
    ///
    /// The assertion is on `at_rest()` because `draw` cannot be reached from a host test: it
    /// paints, and painting measures text through SDL2_ttf, which this build does not link. This
    /// is the predicate the branch is now keyed on, and the one that used to have no equivalent.
    #[test]
    fn a_settled_pop_leaves_the_surface_at_rest_at_depth_two() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-at-rest");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        assert!(s.at_rest(), "a freshly mounted surface has no push in flight");

        // root → Legal notices → About-style document: two pushes, so the pop below lands on a
        // stack that still has something UNDER its top
        let legal_row = FocusKey { entry: EntryId(0), elem: 1 };
        step(&mut s, ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir }, Some(legal_row));
        step(&mut s, ScreenEvent::Activate(legal_row.elem), Some(legal_row));
        assert!(!s.at_rest(), "the push is in flight the frame it is requested");
        settle(&mut s);
        let doc_row = FocusKey { entry: EntryId(0), elem: 0 };
        step(&mut s, ScreenEvent::FocusMoved { from: None, to: doc_row, by: By::Dir }, Some(doc_row));
        step(&mut s, ScreenEvent::Activate(doc_row.elem), Some(doc_row));
        settle(&mut s);
        assert_eq!(s.inner.depth(), 3, "root → Legal index → one Legal document");

        let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Sdl,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        });
        step(&mut s, back, None);
        assert_eq!(s.inner.depth(), 2, "BACK popped the document");
        assert!(!s.at_rest(), "…and the reverse push is carrying it out");
        settle(&mut s);
        assert!(
            s.at_rest(),
            "with the spring settled and nothing leaving, the Legal index is the only page on \
             screen — even though `below()` still answers the Settings root"
        );
        assert!(s.push.leaving.is_none(), "the outgoing body is released when the spring lands");
    }

    /// **The surface's logical state covers its inner stack — the whole reason phase 5b re-pinned
    /// `state_fp` and invalidated every committed replay fixture.** `Dispatcher::state_hash` folds
    /// in exactly `screen.state().hash()` per surface, so with the ceremony alone in there (which
    /// is what `SurfaceState` wrote until 2026-09-07) every press anywhere inside Settings,
    /// Privacy, Legal and first-run Favourites hashed identically and a replay could never report
    /// `DIVERGED`.
    ///
    /// **What this test grades is the STACK half and nothing else, and the distinction is the one
    /// the old wording lost.** It opens a page and watches the hash move, which proves the fold
    /// reaches the bodies at all. It says nothing whatever about whether a given body's own
    /// `state()` writes enough to tell two of ITS frames apart — the sentence here used to add
    /// that the finer half "rides in through each body's own `state().hash()`", which is a
    /// mechanism dressed as a guarantee, and a verifier refuted it by scrolling a Legal document
    /// with the hash standing still. That half is each page's test to write, in each page's own
    /// file; the `LogicalState` impl above carries the census of who currently does.
    #[test]
    fn the_logical_state_follows_the_inner_stack() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-state-hash");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let at_root = <RouteSurface as Screen<InnerHost>>::state(&s).hash();

        let legal_row = FocusKey { entry: EntryId(0), elem: 1 };
        step(&mut s, ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir }, Some(legal_row));
        step(&mut s, ScreenEvent::Activate(legal_row.elem), Some(legal_row));
        let at_legal = <RouteSurface as Screen<InnerHost>>::state(&s).hash();
        assert_ne!(
            at_root, at_legal,
            "pushing the Legal index must move the surface's logical state, or a replay grades \
             every press inside the family as identical"
        );

        let mut probe = String::new();
        <RouteSurface as Screen<InnerHost>>::state(&s).probe(&mut probe);
        assert!(
            probe.starts_with("settings/root:") && probe.contains("/legal:"),
            "the probe names the path the surface is standing on, got {probe:?}"
        );
    }

    /// A BACK press as the dispatcher delivers one. `at_edge` is `false` because this harness
    /// has no engine to have produced an edge rule; the surface's arm is deliberately blind to
    /// the flag (see its comment), so the two roads are one event here.
    fn back_key() -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Sdl,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        })
    }

    /// Hand the surface an effect the way an inner page's own `step` does — through `forward`,
    /// the one seam every page emission crosses on its way up — and return what the surface
    /// emitted outward. Driving `request` directly would test the guard and skip the road, and
    /// the road is half the claim: the fix has to hold for a Pop that arrives from a page, not
    /// only for one this file spells out.
    fn forwarded(s: &mut RouteSurface, fx: Fx<InnerHost>) -> Vec<Stamped<InnerHost>> {
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut sink = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        let c = cx(None);
        s.forward(
            vec![Stamped {
                from: MachineId::Instance(InstanceId(1)),
                fx,
            }],
            &c,
            &mut sink,
        );
        out
    }

    #[test]
    fn only_current_inner_instance_can_project_remembered_selection() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("inner-remember-owner");
        let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let covered = s.inner.top().unwrap().inst.as_ref().unwrap().id;
        forwarded(&mut s, Fx::Nav(NavOp::Push(SettingsPage::Legal)));
        let active = s.inner.top().unwrap().inst.as_ref().unwrap().id;
        assert_ne!(covered, active);
        for (source, accepted) in [(covered, false), (active, true)] {
            let mut out = Vec::new();
            let mut present = Present::new();
            let mut sink = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
            s.forward(
                vec![Stamped {
                    from: MachineId::Instance(source),
                    fx: Fx::Remember { group: GroupId(0), elem: 0 },
                }],
                &cx(None),
                &mut sink,
            );
            assert_eq!(out.iter().any(|s| matches!(s.fx, Fx::Remember { .. })), accepted);
        }
    }

    /// **A Pop that would empty the inner stack is the surface's dismissal, not a surface left up
    /// with no page in it.** The configuration is the real one:
    /// `/tmp/plxnative-settings=privacy` roots the surface AT Privacy & data, and that page's
    /// Done (`consent::band_commit`'s Settings arm) emits a bare `Fx::Nav(NavOp::Pop)` — correct
    /// when Privacy sits over the Settings root, and one entry too many here. `NavStack` has no
    /// depth guard of its own, so before the fix this retired the only entry and left `top_mut()`
    /// at `None` while `at_rest()` stayed true: scrim and ground still drawn, input still owned,
    /// no page, no hit stops, and no key that could reach it. `onboard::leave`'s settings arm and
    /// `/tmp/plxnative-settings=home` are the same pair.
    #[test]
    fn a_pop_that_would_empty_the_stack_dismisses_the_surface_instead() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-pop-empty");
        let mut s = RouteSurface::new(EntryId(7), InstanceId(0), Family::Settings, SettingsPage::Privacy);
        step(&mut s, ScreenEvent::Mount, None);
        assert_eq!(s.inner.depth(), 1, "rooted at Privacy, there is nothing under it");

        let out = forwarded(&mut s, Fx::Nav(NavOp::Pop));
        assert!(
            out.iter().any(|st| matches!(&st.fx, Fx::Nav(NavOp::Dismiss(e)) if *e == EntryId(7))),
            "the Pop must become this surface's own dismissal, emitted against its entry"
        );
        assert_eq!(s.inner.depth(), 1, "…and the stack must NOT have been emptied on the way");
        assert!(
            s.top().is_some(),
            "a surface with no top page draws its ground over nothing and cannot be left"
        );
    }

    /// The complementary half, and the one that must not regress: with a page UNDER it, the same
    /// forwarded Pop is an ordinary inner pop and the surface stays up. Without this, "dismiss on
    /// Pop" would be indistinguishable from "dismiss on every Pop", which would take Privacy's
    /// Done straight out of Settings instead of back to its root.
    #[test]
    fn a_pop_with_a_page_under_it_pops_the_inner_stack_and_stays_up() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-pop-inner");
        let mut s = RouteSurface::new(EntryId(7), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut s, ScreenEvent::Mount, None);
        let legal_row = FocusKey { entry: EntryId(7), elem: 1 };
        step(&mut s, ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir }, Some(legal_row));
        step(&mut s, ScreenEvent::Activate(legal_row.elem), Some(legal_row));
        assert_eq!(s.inner.depth(), 2, "Legal is up");

        let out = forwarded(&mut s, Fx::Nav(NavOp::Pop));
        assert!(
            !out.iter().any(|st| matches!(&st.fx, Fx::Nav(NavOp::Dismiss(_)))),
            "a Pop with something under it is the INNER stack's, never the surface's"
        );
        assert_eq!(s.inner.depth(), 1);
        assert_eq!(name(&s), word::SETTINGS, "it landed back on the Settings root");
    }

    /// **`remembered` is in the hash, and this is the arrangement that isolates it.** Two
    /// surfaces are driven through byte-identical page states — same root selection (`FocusMoved`
    /// writes `RootState::sel` from the event, not from `Cx`), same push, same entry and instance
    /// ids from the same `Minter` sequence — and differ in ONE respect: the first pushes with the
    /// engine reporting a current focus, so `request` records the seat, and the second pushes
    /// with none, so it records nothing. Before the seats were folded in, those two surfaces
    /// hashed identically and then behaved differently the moment BACK was pressed: the second
    /// assertion is that divergence, arriving one frame later at the re-seat, with nothing in the
    /// record able to say why.
    #[test]
    fn the_remembered_seats_are_part_of_the_hash() {
        let _g = crate::testlock::serial();
        let _sess = scratch_session("surface-seat-hash");
        let legal_row = FocusKey { entry: EntryId(0), elem: 1 };

        let mut seated = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut seated, ScreenEvent::Mount, None);
        step(&mut seated, ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir }, Some(legal_row));
        step(&mut seated, ScreenEvent::Activate(legal_row.elem), Some(legal_row));

        let mut unseated = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
        step(&mut unseated, ScreenEvent::Mount, None);
        step(&mut unseated, ScreenEvent::FocusMoved { from: None, to: legal_row, by: By::Dir }, Some(legal_row));
        step(&mut unseated, ScreenEvent::Activate(legal_row.elem), None);

        assert_eq!(seated.inner.depth(), unseated.inner.depth(), "the same stack, by construction");
        assert!(!seated.remembered.is_empty(), "the seated push recorded where focus was");
        assert!(unseated.remembered.is_empty(), "the unseated one had nothing to record");
        assert_ne!(
            <RouteSurface as Screen<InnerHost>>::state(&seated).hash(),
            <RouteSurface as Screen<InnerHost>>::state(&unseated).hash(),
            "two surfaces that will seat focus differently on the next BACK must not hash alike"
        );

        // …and here is the behaviour that difference predicts, so the hash is grading something
        // a replay can actually see go wrong rather than an incidental field. `ScreenEvent` is
        // not `Clone` (it carries a host's own types), so the press is built once per surface.
        let a = step(&mut seated, back_key(), None);
        let b = step(&mut unseated, back_key(), None);
        let seat_of = |out: &[Stamped<InnerHost>]| {
            out.iter().find_map(|st| match &st.fx {
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) => Some(*focus),
                _ => None,
            })
        };
        assert!(matches!(seat_of(&a), Some(FocusTarget::Elem(k)) if k == legal_row));
        assert!(matches!(seat_of(&b), Some(FocusTarget::ContainerGroup(GroupId(0)))));
    }

    /// **THE LEFT ROAD, EXECUTED** (§7.3 step 2 + rule 9) — the one road into this surface that
    /// no test ran end to end.
    ///
    /// The BACK road is executed: `app/bridge.rs`'s
    /// `the_settings_surface_owns_input_and_walks_its_own_stack` drives a real `Dispatcher`, the
    /// real surface and real `Key::Back` presses, root → Legal → back → root → dismissed. LEFT is
    /// supposed to mean the same thing — "LEFT returns to the index, and only a second LEFT
    /// leaves the family", which `ui/legal.rs`'s
    /// `right_enters_a_document_and_left_walks_all_the_way_back_out` pinned for the legacy screens
    /// and which went out of the tree WITH that module in phase 5b — and after the port it was
    /// true only as the COMPOSITION of three links, each tested somewhere else and never together:
    ///
    ///  1. the table/document groups declare `EdgeRule::Nav(NavOpKind::Back)` on their LEFT edge
    ///     (`ui::table_screen`'s own tests, and `screens::legal`'s, assert the edge array),
    ///  2. `dispatch::after_step` turns that outcome into a synthetic `Key::Back` down with
    ///     `at_edge: true` delivered to the input OWNER (`dispatch::edge_back_tests`, against a
    ///     hand-built screen that counts BACKs and decrements an integer "depth"),
    ///  3. `RouteSurface` walks its own `NavStack` on a BACK and declines one at its root (the
    ///     tests above, which hand-feed `Key::Back` with `at_edge: false` and have no engine at
    ///     all).
    ///
    /// Every link can stay green while the chain is broken, because no link's test contains the
    /// next one's subject: link 2's owner is not this surface, and link 3's harness cannot
    /// produce an edge. That is not hypothetical — link 2 IS a repair, of an engine shortcut that
    /// sent the edge straight to `Navigation::back` and so dismissed the whole surface where a
    /// BACK press had walked its stack (`after_step`'s own comment). This module runs the chain:
    /// a real `Dispatcher`, a real `RouteSurface` presented into it as an `Opaque` modal, a real
    /// LEFT through the real engine, and the two ends of the rule asserted — at depth the inner
    /// stack pops and the surface stays up, at the root the surface is dismissed.
    ///
    /// **The link still not executed here is the DOCUMENT's own LEFT.** Reaching a Legal document
    /// takes a second press, and at that depth the edge rule is `DocumentFocus`'s rather than the
    /// table's — a different link 1, with links 2 and 3 identical (the surface sees a BACK and
    /// pops, whatever declared the edge). `screens::legal`'s `a_document_s_left_edge_is_back_to_
    /// the_index` grades that declaration on its own. What is proven below is the index level:
    /// one LEFT back to the Settings root, a second one out of the family.
    ///
    /// The bundle is the family's own inner host, which is what makes this possible from this
    /// file at all: `InnerHost` is a complete `Host` (`screens::family`) AND satisfies `AppLike`,
    /// so `Dispatcher<InnerHost>` mounts the same `RouteSurface` the bridge does. The app's real
    /// bundle (`app/bridge.rs`'s `AppHost`) cannot be named here — its `Arg` carries the legacy
    /// `Route`, which is `app`-private — so what this cannot see is the app's own mounter and
    /// nothing else; the dispatcher, the engine, the containers and the surface are the shipping
    /// ones.
    mod composed {
        use super::*;
        use crate::screens::registry::{self, AppFx, AppMsg};
        use crate::ui::containers::modal::{Phase, Style};
        use crate::ui::dispatch::{CxParts, Dispatcher, NoTap, Rig, Split};
        use crate::ui::fixture::{key, tick};
        use crate::ui::machine::TimerId;

        /// The dispatcher's own mounter: the surface for anything but [`SettingsPage::About`],
        /// which stands in for whatever page the application has UNDER Settings. The root stack
        /// needs a body and this test is not about which one; a document is the family's most
        /// inert page (its `prepare` is empty and it draws nothing without a `DrawFrame`).
        struct SurfaceMounter;

        impl Mounter<InnerHost> for SurfaceMounter {
            fn mount(
                &mut self,
                id: InstanceId,
                arg: &SettingsPage,
                _ret: &ReturnState<u32>,
                cx: &Cx<'_, InnerHost>,
                fx: &mut Effects<'_, InnerHost>,
            ) -> Box<dyn Screen<InnerHost>> {
                // `mount` is handed the mounting body's OWN entry as `cx.owner`
                // (`Dispatcher::mount`), and the surface must be built with it: every `FocusKey`
                // its pages mint carries that entry, and the engine matches on it.
                let entry = match cx.owner {
                    InputOwner::Entry(e) => e,
                    _ => EntryId(0),
                };
                match arg {
                    SettingsPage::About => mount_page(entry, SettingsPage::About, cx, fx),
                    SettingsPage::ConsentStage(stage) => Box::new(RouteSurface::new(entry, id,
                        Family::FirstRunConsent, SettingsPage::ConsentStage(*stage))),
                    other => Box::new(RouteSurface::new(entry, id, Family::Settings, *other)),
                }
            }
        }

        struct SurfaceRig {
            mounter: SurfaceMounter,
            measure: FixtureMeasure,
            /// How many times BACK reached the root of the ROOT stack (the platform's Home).
            roots: u32,
        }

        impl Rig<InnerHost> for SurfaceRig {
            fn split(&mut self) -> Split<'_, InnerHost> {
                Split {
                    mounter: &mut self.mounter,
                    views: (),
                    measure: &self.measure,
                }
            }
            fn deliver(&mut self, _to: MachineId, _msg: &AppMsg, _parts: &CxParts<u32>, _fx: &mut Effects<'_, InnerHost>) -> Handled {
                Handled::No
            }
            fn timer(&mut self, _owner: MachineId, _id: TimerId, _parts: &CxParts<u32>, _fx: &mut Effects<'_, InnerHost>) {}
            fn app_fx(&mut self, _from: MachineId, _fx: AppFx, _parts: &CxParts<u32>, _out: &mut Effects<'_, InnerHost>) {}
            fn log(&mut self, _line: &str) {}
            fn prepare(&mut self, _b: &mut Budget, _present: &mut Present) {}
            fn ls2_pump(&mut self) {}
            fn opaque_route(&mut self, _bound: bool) {}
            fn clear_opaque_region(&mut self) {}
            fn now_us(&self) -> u64 {
                0
            }
            fn back_at_root(&mut self) {
                self.roots += 1;
            }
        }

        /// **`draw: false` on every frame, and it is not an optimisation.** `RouteSurface::draw`
        /// paints — a scrim, the ground, then each body — and painting measures text through
        /// SDL2_ttf and issues GL calls, neither of which a host unit build has. Steps 1–9 are
        /// what this module grades (ingest, the engine, the drain, the nav commit), and step 10
        /// is the only one it cannot run. The prepare pass still runs, which is safe: every
        /// `prepare` in this family is empty.
        fn frame(d: &mut Dispatcher<InnerHost>, rig: &mut SurfaceRig, ms: u32, inputs: Vec<crate::ui::machine::InputEvent<u32>>) {
            d.frame_with(rig, tick(ms), inputs, vec![], &mut NoTap, false);
        }

        /// A booted dispatcher with an `Opaque` Settings surface presented over the root page,
        /// and the surface's entry id.
        fn opened() -> (Dispatcher<InnerHost>, SurfaceRig, EntryId) {
            let mut d: Dispatcher<InnerHost> = Dispatcher::new();
            let mut rig = SurfaceRig {
                mounter: SurfaceMounter,
                measure: FixtureMeasure,
                roots: 0,
            };
            d.request(MachineId::Nav, NavOp::Root(SettingsPage::About));
            frame(&mut d, &mut rig, 0, vec![]);
            d.nav.next_style = Style::Opaque { snapshot: true };
            d.request(MachineId::Nav, NavOp::Present(SettingsPage::Root));
            frame(&mut d, &mut rig, 16, vec![]);
            let id = d.nav.modals.top().expect("the surface is presented").entry.id;
            (d, rig, id)
        }

        /// The surface's own `LogicalState::probe` — the path it is standing on, which is the
        /// only view of the inner stack a caller outside this file has (the container hands out
        /// `&dyn Screen`, so there is no downcast to a `RouteSurface`).
        fn path(d: &Dispatcher<InnerHost>, id: EntryId) -> String {
            let mut s = String::new();
            d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
            s
        }

        /// Seat focus on the surface's Nth element as the test's PREMISE. Without it the first
        /// direction key is spent by `move_dir`'s no-focus fallback, which seats and returns
        /// `Moved` and never reaches an edge rule — the assertion would then be about seating
        /// rather than about the rule under test.
        fn seat(d: &mut Dispatcher<InnerHost>, id: EntryId, elem: u32) {
            d.set_focus(Some(FocusKey { entry: id, elem }));
        }

        fn consent_opened(page: SettingsPage) -> (Dispatcher<InnerHost>, SurfaceRig, EntryId) {
            let mut d = Dispatcher::new();
            let mut rig = SurfaceRig { mounter: SurfaceMounter, measure: FixtureMeasure, roots: 0 };
            d.request(MachineId::Nav, NavOp::Root(SettingsPage::About));
            frame(&mut d, &mut rig, 0, vec![]);
            d.nav.next_style = Style::Opaque { snapshot: true };
            d.request(MachineId::Nav, NavOp::Present(page));
            frame(&mut d, &mut rig, 16, vec![]);
            let id = d.nav.modals.top().unwrap().entry.id;
            (d, rig, id)
        }

        #[test]
        fn composed_owner_first_run_answers_survive_full_frames() {
            let _g = crate::testlock::serial();
            for stage in [0, 1, 17] {
                let (mut d, mut rig, id) = consent_opened(SettingsPage::ConsentStage(stage));
                assert_eq!(d.focus(), Some(FocusKey { entry: id, elem: registry::BAND }), "stage {stage} mount");
                frame(&mut d, &mut rig, 32, vec![]);
                assert_eq!(d.focus().unwrap().elem, registry::BAND);
                for (round, direction) in [Key::Left, Key::Down].into_iter().enumerate() {
                    let ms = 48 + round as u32 * 64;
                    seat(&mut d, id, 1); // Privacy policy, not a guessed geometric starting point.
                    frame(&mut d, &mut rig, ms, vec![key(direction, tick(ms))]);
                    assert!(registry::band_index(d.focus().unwrap().elem).is_some(), "stage {stage}, {direction:?}");
                    seat(&mut d, id, registry::BAND);
                    frame(&mut d, &mut rig, ms + 16, vec![key(Key::Right, tick(ms + 16))]);
                    assert_eq!(d.focus().unwrap().elem, registry::BAND + 1);
                    frame(&mut d, &mut rig, ms + 32, vec![]);
                    assert_eq!(d.focus().unwrap().elem, registry::BAND + 1);
                    frame(&mut d, &mut rig, ms + 48, vec![key(Key::Left, tick(ms + 48))]);
                    assert_eq!(d.focus().unwrap().elem, registry::BAND);
                }
                assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
                assert_eq!(rig.roots, 0);
            }
        }

        #[test]
        fn composed_owner_settings_done_survives_then_disappears_with_reverted_draft() {
            let _g = crate::testlock::serial();
            let (mut d, mut rig, id) = consent_opened(SettingsPage::Privacy);
            seat(&mut d, id, 0);
            frame(&mut d, &mut rig, 32, vec![key(Key::Ok, tick(32))]); // draft only
            frame(&mut d, &mut rig, 48, vec![key(Key::Left, tick(48))]);
            assert_eq!(d.focus().unwrap().elem, registry::BAND);
            frame(&mut d, &mut rig, 64, vec![]);
            assert_eq!(d.focus().unwrap().elem, registry::BAND);
            seat(&mut d, id, 0);
            frame(&mut d, &mut rig, 80, vec![key(Key::Ok, tick(80))]); // reverse draft
            seat(&mut d, id, registry::BAND); // a retained key for a removed control
            frame(&mut d, &mut rig, 96, vec![]);
            assert!(registry::band_index(d.focus().unwrap().elem).is_none());
            assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
        }

        #[test]
        fn composed_owner_pointer_seats_and_validates_answer_press_identity_without_committing() {
            use crate::ui::machine::{InputEvent, InputKind, PressId, Source};
            use crate::ui::screen::{Activate, Hover, Stop};
            let _g = crate::testlock::serial();
            for stage in [0, 1] {
                let (mut d, mut rig, id) = consent_opened(SettingsPage::ConsentStage(stage));
                for (round, elem) in [registry::BAND, registry::BAND + 1].into_iter().enumerate() {
                    let ms = 32 + round as u32 * 32;
                    let unchanged_path = path(&d, id);
                    let focused = FocusKey { entry: id, elem };
                    let c = cx(None);
                    let screen = &d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen;
                    let placed = screen.place(&elem, &c, At::Drawn).expect("real Consent button geometry");
                    // No GL draw on the host: publish the real queried geometry to the real hit map.
                    d.input.hit.fill(vec![Stop { key: focused, rect: placed.rect, rest_rect: placed.rest_rect,
                        clip: placed.clip, hover: Hover::Focus, activate: Activate::Press }]);
                    d.input.hit.swap();
                    d.input.hit.dpad_mode = false;
                    frame(&mut d, &mut rig, ms, vec![InputEvent { at: tick(ms), source: Source::Script,
                        kind: InputKind::Pointer { x: placed.rect.cx(), y: placed.rect.cy(), hit: None } }]);
                    assert_eq!(d.focus(), Some(focused));
                    let instance = d.nav.instance_of(id).unwrap();
                    // The production typed press validator, but Hold rather than Commit: this
                    // page does not answer on hold, so neither consent choice is made or saved.
                    d.emit(MachineId::Input, Fx::Deliver(MachineId::Instance(instance),
                        Delivery::Press { id: PressId(1), key: focused, held: true }));
                    let report = d.frame_with(&mut rig, tick(ms + 16), vec![], vec![], &mut NoTap, false);
                    assert_eq!(report.dropped_deliveries, 0, "a valid button was refused by press identity validation");
                    assert_eq!(d.focus(), Some(focused));
                    assert_eq!(path(&d, id), unchanged_path, "holding must not answer or advance the stage");
                    assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
                }
            }
        }

        #[test]
        fn composed_owner_favourites_footer_survives_left_down_and_idle_frames() {
            let _g = crate::testlock::serial();
            let _session = scratch_session("composed-owner-favourites");
            struct ResetSources;
            impl Drop for ResetSources {
                fn drop(&mut self) {
                    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
                    crate::plex::reset_servers_for_test();
                }
            }
            let _reset = ResetSources;
            crate::plex::reset_servers_for_test();
            let a = crate::plex::register_for_test("focus-a", "127.0.0.1", 9, "synthetic", "focus-test");
            let b = crate::plex::register_for_test("focus-b", "127.0.0.1", 9, "synthetic", "focus-test");
            // The existing fixture pins client identities and marks sections/counts complete:
            // the real Onboard Tick can poll discovery without spawning network work.
            crate::browse::seed_registered_table_for_test([a, b]);
            let pins = crate::browse::favorite_sections();
            let (mut d, mut rig, id) = consent_opened(SettingsPage::Favourites);
            seat(&mut d, id, 0);
            frame(&mut d, &mut rig, 32, vec![key(Key::Ok, tick(32))]); // local draft only
            for (round, direction) in [Key::Left, Key::Down].into_iter().enumerate() {
                let ms = 48 + round as u32 * 32;
                let screen = &d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen;
                let mut groups = Vec::new();
                screen.groups(&cx(None), &mut groups);
                let table = groups.iter().find(|g| g.id == GroupId(0)).unwrap();
                assert!(table.len > 0, "actual Favourites rows must exist");
                assert!(groups.iter().any(|g| g.id == GroupId(1) && g.len == 1), "draft offers Done");
                seat(&mut d, id, table.len as u32 - 1);
                frame(&mut d, &mut rig, ms, vec![key(direction, tick(ms))]);
                assert_eq!(d.focus().unwrap().elem, registry::BAND, "{direction:?} reaches Done after reconciliation");
                frame(&mut d, &mut rig, ms + 16, vec![]);
                assert_eq!(d.focus().unwrap().elem, registry::BAND);
            }
            assert_eq!(crate::browse::favorite_sections(), pins, "draft navigation cannot persist pins");
            assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)));
        }

        /// **The composition, at depth.** Signed out, the Settings root's rows are Privacy & data
        /// / Legal notices / About, so OK on row 1 pushes the Legal index; LEFT off that index's
        /// column then runs the whole chain — edge rule, synthetic BACK, the surface's own pop —
        /// and lands back on the Settings root with the surface still up and still owning input.
        #[test]
        fn left_inside_the_family_pops_the_inner_stack_and_never_dismisses_the_surface() {
            let _g = crate::testlock::serial();
            let _sess = scratch_session("composed-left-inner");
            let (mut d, mut rig, id) = opened();
            assert!(path(&d, id).starts_with("settings/root:"), "{}", path(&d, id));

            seat(&mut d, id, 1);
            frame(&mut d, &mut rig, 32, vec![key(Key::Ok, tick(32))]);
            assert!(path(&d, id).contains("/legal:"), "OK on Legal notices pushed the index: {}", path(&d, id));

            seat(&mut d, id, 0);
            frame(&mut d, &mut rig, 48, vec![key(Key::Left, tick(48))]);
            let p = path(&d, id);
            assert!(!p.contains("/legal:"), "LEFT popped the index off the surface's stack: {p}");
            assert!(p.starts_with("settings/root:"), "…and landed on the Settings root: {p}");
            assert_ne!(
                d.nav.modals.top().unwrap().phase,
                Phase::Closing,
                "a LEFT the surface answered is not the container's BACK"
            );
            assert_eq!(
                d.nav.input_owner(),
                Some(InputOwner::Entry(id)),
                "…and the surface still owns input"
            );
            assert_eq!(rig.roots, 0, "nothing reached the platform");
        }

        /// **The composition, at the surface's own root — the second LEFT.** The same press, one
        /// level shallower, is declined by the surface (`Handled::No`), becomes the dispatcher's
        /// `pending_back` and is resolved at the same frame's nav commit by dismissing the whole
        /// surface. This is the half that must NOT change: `request`'s new "a Pop that would
        /// empty the stack dismisses" guard deliberately does not cover BACK, because the
        /// container's answer to a root BACK can be more than a dismissal.
        #[test]
        fn left_at_the_surfaces_own_root_dismisses_it() {
            let _g = crate::testlock::serial();
            let _sess = scratch_session("composed-left-root");
            let (mut d, mut rig, id) = opened();
            seat(&mut d, id, 0);
            frame(&mut d, &mut rig, 32, vec![key(Key::Left, tick(32))]);
            assert!(path(&d, id).starts_with("settings/root:"), "the stack never moved: {}", path(&d, id));
            assert_eq!(
                d.nav.modals.top().unwrap().phase,
                Phase::Closing,
                "the refusal became the container's BACK, in the same frame"
            );
            assert_ne!(d.nav.input_owner(), Some(InputOwner::Entry(id)), "input left with it");
            assert_eq!(rig.roots, 0, "a surface dismissal is not the platform's BACK");
        }
    }
}
