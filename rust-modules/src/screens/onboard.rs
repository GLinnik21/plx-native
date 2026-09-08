//! **Favorite libraries, as one screen mounted twice** (restructure spec §6.2 "Onboard ×2",
//! phase 5b; the words, the draft model and the design argument are `ui/onboard.rs`'s, moved).
//! First run mounts it as a PAGE (`Route::Onboard`, between the picker and Home, on the
//! hero-keyed ground, crumb *Who's watching?*, one action *Start watching*); Settings mounts the
//! same type as a page of the surface's own stack (crumb *Settings*, *Done* only once the draft
//! differs). The type is generic over the bundle's host, which is what lets one impl serve both.
//!
//! The draft model is unchanged: every toggle edits `draft`, nothing is written until the one
//! action commits `BrowseCmd::ApplyPins`, and BACK/Cancel is a pure discard.

use std::borrow::Cow;
use std::ffi::CStr;

use crate::stores::browse::BrowseCmd;
use crate::stores::{StoreCmd, StoreId};
use crate::ui::frame::Budget;
use crate::ui::machine::{
    Canon, Cx, Delivery, Edge, Effects, EntryId, Fx, GroupId, Handled, InputEvent, InputKind, Key, LogicalState, Machine, NavOp,
};
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::screen::{DrawFrame, Enter, FocusSource, FocusTarget, HitSource, Part, RenderStrategy, Screen, ScreenEvent};
use crate::ui::source_list::{self, Level, SrcAction, Tail};
use crate::ui::table::TableView;
use crate::ui::table_screen::{BandPart, Header, TableScreen};
use crate::ui::widgets::{CtlPop, Spinner, StatusKind, StatusOverlay};
use crate::ui::{theme, Env, Painter, View};

use super::family::{palette, table_focus, BAND_GROUP, TABLE_GROUP};
use super::registry::{band_index, word, AppFx, AppLike, LoopReq};

pub(crate) const TITLE: &str = "Which libraries do you want?";
const SETTINGS_TITLE: &str = "Favorite libraries";
const ACTION: &CStr = c"Start watching";
const DONE: &CStr = c"Done";
const RETRY: &CStr = c"Try again";
const CRUMB_SETTINGS: &str = "Settings";
const CRUMB_PROFILES: &str = crate::screens::profiles::TITLE;

/// Does first run ask this at all?
pub(crate) fn asks() -> bool {
    crate::browse::first_run_asks()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ActionKind {
    Retry,
    Start,
    Done,
}

impl ActionKind {
    fn label(self) -> &'static CStr {
        match self {
            ActionKind::Retry => RETRY,
            ActionKind::Done => DONE,
            ActionKind::Start => ACTION,
        }
    }
}

pub(crate) struct OnboardScreen {
    entry: EntryId,
    settings: bool,
    table: TableView,
    acts: Vec<SrcAction>,
    table_gen: u32,
    table_epoch: u32,
    entry_pins: Vec<(usize, bool)>,
    draft: Vec<(usize, bool)>,
    phase_ms: f32,
    pop: CtlPop<1>,
    ground: RouteGround,
    state: OnboardState,
    /// **What the one action pill MEANT at the moment its press was armed** — `ui/onboard.rs`'s
    /// `ARMED`/`ActionKind` pair, moved rather than dropped. The pill's meaning changes underneath
    /// a press that is already springing back: first run with nothing discovered shows `Try
    /// again`, and if the roster lands during the ~210 ms the tvOS press takes to bounce, the SAME
    /// focus stop (the band's one control, always element 0) has become `Start watching` — and
    /// `PressCommit` carries no memory of what was true at arm time, only the `PressId` that
    /// resolved. Focus location cannot see the change either: the band is the band whichever verb
    /// it draws. So this screen keeps its own record, written the moment an OK-down or a pointer
    /// hit on the band is about to arm the engine's press (`step`'s `Input` arms, which record and
    /// then answer `Handled::No` so the engine still does the arming), and consulted once, at
    /// `PressCommit`, against whatever `action_kind()` reads NOW: a mismatch refuses the whole
    /// press rather than performing either the verb that was pressed or the verb the pill now
    /// shows — committing the wrong one is worse than a dropped press, since one silently answers
    /// the first-run question on a `Try again` tap. See
    /// `an_action_that_changes_verb_under_an_armed_press_refuses_to_commit` below.
    armed_kind: Option<ActionKind>,
    /// **`has_band()`'s answer, cached rather than read live.** Whether the band holds a control
    /// depends on `crate::browse::section_count()` — a MUTABLE GLOBAL a roster-landing worker can
    /// change between two calls this screen never sees as one event. `Focusable::groups` reaches
    /// `has_band()` through `view()`/`labels()` on every focus query the engine makes, which can
    /// happen at any point between two of this screen's own `step`s — so a live read there means
    /// the group SET the engine is reasoning about can change mid-query, the same class of race
    /// `armed_kind`'s own doc describes on the press side (and closes the same way: record the
    /// answer at a point this screen controls, rather than re-deriving it from a global whenever
    /// asked). `rebuild` is that point — it already reacts to every input that could move this
    /// answer (a fresh mount, a toggle, and the `Tick`/`StoreChanged` arms that watch
    /// `source_list_gen`) — so caching it there costs nothing and buys two things a live read
    /// cannot: a STABLE group set for the whole gap between two `rebuild`s, and a state the
    /// recorder (`OnboardState::write`, below) can actually see and replay a frame against.
    band: bool,
}

struct OnboardState {
    settings: bool,
    band: bool,
    draft: Vec<(usize, bool)>,
}

impl LogicalState for OnboardState {
    fn write(&self, w: &mut Canon) {
        w.bool(self.settings);
        w.bool(self.band);
        w.seq(self.draft.len());
        for (s, on) in &self.draft {
            w.u32(*s as u32).bool(*on);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "onboard settings={} band={} draft={:?}",
            self.settings, self.band, self.draft
        ));
    }
}

fn snapshot_pins() -> Vec<(usize, bool)> {
    crate::browse::all_source_rows()
        .into_iter()
        .map(|r| (r.section, r.pinned))
        .collect()
}

impl OnboardScreen {
    /// First run's page.
    pub(crate) fn first_run(entry: EntryId) -> Self {
        Self::new(entry, false)
    }
    /// The Settings editor.
    pub(crate) fn settings(entry: EntryId) -> Self {
        Self::new(entry, true)
    }

    fn new(entry: EntryId, settings: bool) -> Self {
        let base = snapshot_pins();
        let mut s = Self {
            entry,
            settings,
            table: TableView::new(),
            acts: Vec::new(),
            table_gen: u32::MAX,
            table_epoch: crate::browse::table_epoch(),
            entry_pins: if settings { base.clone() } else { Vec::new() },
            draft: base,
            phase_ms: 0.0,
            pop: CtlPop::new(),
            ground: if settings { RouteGround::new() } else { super::family::pre_home_ground() },
            state: OnboardState {
                settings,
                band: false, // overwritten by `rebuild` below, before anything reads it
                draft: Vec::new(),
            },
            armed_kind: None,
            band: false, // ditto
        };
        s.rebuild(false);
        s.table.list_focused = settings;
        s
    }

    fn action_kind(&self) -> ActionKind {
        if crate::browse::section_count() == 0 {
            ActionKind::Retry
        } else if self.settings {
            ActionKind::Done
        } else {
            ActionKind::Start
        }
    }

    fn dirty(&self) -> bool {
        if !self.settings {
            return true;
        }
        self.entry_pins.iter().any(|(section, was)| {
            self.draft
                .iter()
                .find(|(s, _)| s == section)
                .is_some_and(|(_, now)| now != was)
        })
    }

    /// The band holds a control unless this is a pristine Settings editor. Reads the CACHED
    /// answer (`band`'s own doc has the reason) rather than re-deriving it from the live global
    /// `crate::browse::section_count()` on every call — `rebuild` is what keeps the cache honest.
    fn has_band(&self) -> bool {
        self.band
    }

    fn rebuild(&mut self, keep: bool) {
        self.reseed_if_table_identity_changed();
        let gen = crate::browse::source_list_gen();
        let groups = crate::browse::source_groups();
        let rows = self.draft_rows();
        let (secs, acts) = source_list::sections(Level::OnHome, &groups, &rows, Tail::None);
        let sel = if keep { self.table.sel } else { 0 };
        self.table_gen = gen;
        self.acts = acts;
        self.table.compact = false;
        self.table.set_sections(secs, sel, keep);
        self.state.draft = self.draft.clone();
        // `draft_rows()` just resynced `self.draft`/`self.entry_pins` against whatever the roster
        // looks like now, so `dirty()` below reads the SAME state `has_band()` would have read
        // live — recomputing it here, once, is what lets `has_band()` become a field read instead
        // of three global/derived reads on every focus query the engine makes (`band`'s own doc).
        self.band = !self.settings || crate::browse::section_count() == 0 || self.dirty();
        self.state.band = self.band;
    }

    fn reseed_if_table_identity_changed(&mut self) {
        let epoch = crate::browse::table_epoch();
        if self.table_epoch == epoch {
            return;
        }
        self.table_epoch = epoch;
        let fresh = snapshot_pins();
        self.draft = fresh.clone();
        if self.settings {
            self.entry_pins = fresh;
        }
    }

    fn draft_rows(&mut self) -> Vec<crate::browse::SrcRow> {
        let mut rows = crate::browse::all_source_rows();
        for r in &rows {
            match self.draft.iter().position(|(s, _)| *s == r.section) {
                None => {
                    self.draft.push((r.section, r.pinned));
                    if self.settings {
                        self.entry_pins.push((r.section, r.pinned));
                    }
                }
                Some(di) if self.settings => {
                    if let Some(ei) = self.entry_pins.iter().position(|(s, _)| *s == r.section) {
                        if self.entry_pins[ei].1 == self.draft[di].1 && self.entry_pins[ei].1 != r.pinned {
                            self.entry_pins[ei].1 = r.pinned;
                            self.draft[di].1 = r.pinned;
                        }
                    }
                }
                Some(_) => {}
            }
        }
        let last = self.draft.iter().filter(|(_, on)| *on).count() == 1;
        for r in rows.iter_mut() {
            if let Some(&(_, on)) = self.draft.iter().find(|(s, _)| *s == r.section) {
                r.pinned = on;
                r.last_pinned = last && on;
            }
        }
        rows
    }

    fn body_copy(&self) -> String {
        let who: Vec<String> = crate::browse::source_groups()
            .iter()
            .filter(|g| !g.handle.is_empty())
            .map(|g| g.handle.clone())
            .collect();
        body_copy_for(&who)
    }

    fn toggle_row(&mut self, row: i32) {
        let act = usize::try_from(row).ok().and_then(|i| self.acts.get(i)).copied();
        if let Some(SrcAction::Library(section)) = act {
            let Some(idx) = self.draft.iter().position(|(s, _)| *s == section) else {
                return;
            };
            let on = self.draft[idx].1;
            let last_pinned = on && self.draft.iter().filter(|(_, v)| *v).count() == 1;
            if last_pinned {
                return;
            }
            self.draft[idx].1 = !on;
            self.rebuild(true);
        }
    }

    /// The one action pill, pressed: retry discovery (nothing found yet) or write the draft down
    /// for real. **Both leave through `fx` as an `AppFx::Store`, never by calling
    /// `crate::stores::browse::apply` directly** — that direct spelling is the LEGACY loop's own
    /// shim (`stores/mod.rs`'s module doc: "a legacy screen calls the store's `apply(cmd)` shim…
    /// steps the same machine IMMEDIATELY"), and calling it from here would let an OWNED screen
    /// reach around the one exit the restructure gives it (`ui::machine::Machine`'s doc: purity
    /// is enforced by `Effects` being the only exit). `Bridge::app_fx` (`app/bridge.rs`) turns
    /// `AppFx::Store(id, cmd)` into `Fx::Deliver(MachineId::Store(id.ord()), …)` in the SAME
    /// drain the push happens in, so from outside this screen the command still lands before the
    /// frame presents — the difference is only who is allowed to call the mutator.
    fn commit<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        if crate::browse::section_count() == 0 {
            fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::RetryDiscovery))));
            crate::log("onboard: no discovered libraries yet — retry queued");
            return;
        }
        // The count logged below is read off `self.draft`, not off `crate::browse::pinned_count()`
        // — `ApplyPins` is a QUEUED command now, not a call that has already run by the time this
        // line executes, so the live table still reflects whatever was true BEFORE this commit.
        // `self.draft` is exactly what is about to become the live state, so counting it directly
        // says the true thing regardless of when the queued command is actually drained.
        // `section_count()` is fine to read live for the total: pinning never changes how many
        // libraries exist, so that half of the sentence cannot go stale under a queued command.
        let total = crate::browse::section_count();
        let on = self.draft.iter().filter(|(_, pinned)| *pinned).count();
        fx.push(Fx::App(AppFx::Store(
            StoreId::Browse,
            StoreCmd::Browse(BrowseCmd::ApplyPins(self.draft.clone())),
        )));
        crate::log(&format!("onboard: Home selection recorded — {on} of {total} libraries on"));
        self.leave(fx);
    }

    /// Done/Start or Cancel/BACK: the Settings editor pops off the surface's stack, first run
    /// asks the loop to enter Home.
    fn leave<H: AppLike>(&mut self, fx: &mut Effects<'_, H>) {
        if self.settings {
            fx.push(Fx::Nav(NavOp::Pop));
        } else {
            fx.push(Fx::App(AppFx::Loop(LoopReq::OnboardDone)));
        }
    }

    fn labels(&self) -> Vec<&'static CStr> {
        if self.has_band() {
            vec![self.action_kind().label()]
        } else {
            Vec::new()
        }
    }

    fn view(&self) -> OnboardView<'_> {
        OnboardView {
            screen: self,
            labels: self.labels(),
        }
    }
}

fn body_copy_for(who: &[String]) -> String {
    let tail = " Pick your favorites \u{2014} they are what Home and the Library show, and you \
                can change them in Settings whenever you like.";
    match join_names(who) {
        None => "Choose the libraries this television shows. Your favorites fill Home's shelves \
                 and the Library's own tabs; Settings lists every one you have."
            .to_string(),
        Some(names) => {
            let verb = if who.len() == 1 { "has" } else { "have" };
            format!("{names} {verb} shared libraries with you.{tail}")
        }
    }
}

fn join_names(who: &[String]) -> Option<String> {
    match who {
        [] => None,
        [a] => Some(a.clone()),
        [rest @ .., last] => Some(format!("{} and {last}", rest.join(", "))),
    }
}

/// The frame's focus composition: the table beside the band.
struct OnboardView<'a> {
    screen: &'a OnboardScreen,
    labels: Vec<&'static CStr>,
}

impl<'a> OnboardView<'a> {
    fn screen(&'a self) -> TableScreen<'a> {
        let layout = RouteLayout::screen();
        TableScreen::new(Header::new(layout, None, "", ""), &self.screen.table, TABLE_GROUP, self.screen.entry)
            .uncommitted(self.screen.settings && self.screen.dirty())
            .with_band(BandPart {
                layout,
                labels: &self.labels,
                group: BAND_GROUP,
                entry: self.screen.entry,
                uncommitted: false,
                scales: [self.screen.pop.scale(0), 1.0],
                palette: palette(),
                danger: None,
            })
    }
}

impl<H: AppLike> crate::ui::screen::Focusable<H> for OnboardView<'_> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<crate::ui::screen::GroupSpec>) {
        crate::ui::screen::Focusable::<H>::groups(&self.screen(), cx, out)
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        crate::ui::screen::Focusable::<H>::group_of(&self.screen(), key, cx)
    }
    fn neighbour(&self, key: crate::ui::machine::FocusKey<u32>, dir: crate::ui::screen::Dir, cx: &Cx<'_, H>) -> crate::ui::screen::Step<u32> {
        crate::ui::screen::Focusable::<H>::neighbour(&self.screen(), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: crate::ui::screen::At) -> Option<crate::ui::screen::Placed> {
        crate::ui::screen::Focusable::<H>::place(&self.screen(), key, cx, at)
    }
    fn reconcile(&self, want: crate::ui::machine::FocusKey<u32>, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::screen::Focusable::<H>::reconcile(&self.screen(), want, cx)
    }
    fn seat(&self, g: GroupId, from: crate::ui::screen::Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::screen::Focusable::<H>::seat(&self.screen(), g, from, cx)
    }
}

impl<H: AppLike> crate::ui::screen::Focusable<H> for OnboardScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<crate::ui::screen::GroupSpec>) {
        crate::ui::screen::Focusable::<H>::groups(&self.view(), cx, out)
    }
    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        crate::ui::screen::Focusable::<H>::group_of(&self.view(), key, cx)
    }
    fn neighbour(&self, key: crate::ui::machine::FocusKey<u32>, dir: crate::ui::screen::Dir, cx: &Cx<'_, H>) -> crate::ui::screen::Step<u32> {
        crate::ui::screen::Focusable::<H>::neighbour(&self.view(), key, dir, cx)
    }
    fn place(&self, key: &u32, cx: &Cx<'_, H>, at: crate::ui::screen::At) -> Option<crate::ui::screen::Placed> {
        crate::ui::screen::Focusable::<H>::place(&self.view(), key, cx, at)
    }
    fn reconcile(&self, want: crate::ui::machine::FocusKey<u32>, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::screen::Focusable::<H>::reconcile(&self.view(), want, cx)
    }
    fn seat(&self, g: GroupId, from: crate::ui::screen::Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::screen::Focusable::<H>::seat(&self.view(), g, from, cx)
    }
}

impl<H: AppLike> Machine<H> for OnboardScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::Tick(t) => {
                let dt = t.dt();
                self.phase_ms += dt * 1000.0;
                // The roster half of the pump: sources and their sections land on workers that
                // only the Library screen otherwise schedules. **Called directly rather than
                // through `fx` as an `AppFx::Store`, and deliberately so** — unlike `RetryDiscovery`
                // and `ApplyPins` below, this is not a MUTATION with a `BrowseCmd` of its own; it
                // is the same per-frame LANDING PASS `stores::browse::pump()` already is (that
                // function's own doc: "The landing pass the Library screen runs once a frame while
                // it is up"), just the roster-only half of it, and every existing caller of a pump
                // — the Library screen's own `pump()` call, `viewstate::pump()` from `app/run.rs`
                // — reaches it directly rather than round-tripping it through a command a
                // `Machine::step` would have to invent a `BrowseCmd::Pump` variant to spell. A
                // command that changed nothing would still have to bump the store's generation for
                // every other reader to notice it ran, which a landing pass one screen polls for
                // itself has no need of. If a mutating mistake ever sneaks into `discover_pump`'s
                // own body, this call site would not be the fix — the fix belongs in `browse/`,
                // which decides what a pump is allowed to touch.
                crate::stores::browse::discover_pump();
                if self.table_gen != crate::browse::source_list_gen() {
                    self.rebuild(true);
                    fx.invalidate(crate::ui::present::Provenance::Landing(fx.from()));
                }
                let band = cx.focus.current.and_then(|k| band_index(k.elem));
                self.pop.step(band, dt);
                self.table.update(dt, RouteLayout::screen().sectioned_table().h);
                if self.table.n_rows() == 0 {
                    fx.note(crate::ui::present::PresentEvent::Motion); // the spinner
                }
                Handled::Yes
            }
            ScreenEvent::StoreChanged(..) => {
                if self.table_gen != crate::browse::source_list_gen() {
                    self.rebuild(true);
                }
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                table_focus(&mut self.table, to.elem);
                Handled::Yes
            }
            ScreenEvent::Activate(e) => {
                if band_index(*e).is_none() {
                    self.toggle_row(*e as i32);
                    fx.invalidate(crate::ui::present::Provenance::Input);
                }
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if cx.focus.current.and_then(|k| band_index(k.elem)).is_some() {
                    // Consume the recorded verb rather than peeking it: an arm that outlives its
                    // own commit (there should never be a second `PressCommit` for one `Input`
                    // Down, but a `.take()` costs nothing and means a bug here reads as "refuses
                    // to commit" rather than "commits the stale verb a second time").
                    match self.armed_kind.take() {
                        Some(k) if k == self.action_kind() => self.commit(fx),
                        Some(_) => crate::log(
                            "onboard: the action changed under an armed press — refusing to commit it",
                        ),
                        // `PressCommit` reached us with no recorded arm at all — should not
                        // happen given the `Input` arms below always run first, but refusing is
                        // the safe answer to an event this screen cannot explain.
                        None => {}
                    }
                }
                Handled::Yes
            }
            // The OK key-DOWN on the band: record which verb this press is being armed for
            // BEFORE answering `Handled::No`, which is what lets the engine's own `after_step`
            // still arm the press (§7.3: an unhandled OK-down on a `Control` element arms
            // `Fx::Press`) — this screen only ever WATCHES that arm, it never performs it itself.
            // See `armed_kind`'s own doc for why the verb must be captured here rather than read
            // fresh at `PressCommit`.
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Ok, edge: Edge::Down, .. },
                ..
            }) => {
                if cx.focus.current.and_then(|k| band_index(k.elem)).is_some() {
                    self.armed_kind = Some(self.action_kind());
                }
                Handled::No
            }
            // The pointer's twin of the arm above. A `Click` that lands on the band has ALREADY
            // moved focus there and armed the engine's press by the time this delivery reaches a
            // screen's `step` (both happen earlier in the same frame's ingestion, synchronously —
            // see `ui/dispatch.rs`'s `frame_with` step 2), so reading `cx.focus.current` here is
            // exactly as timely as reading it on the key path above: nothing that could change
            // `action_kind()` can run between the two within one frame.
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Click { .. },
                ..
            }) => {
                if cx.focus.current.and_then(|k| band_index(k.elem)).is_some() {
                    self.armed_kind = Some(self.action_kind());
                }
                Handled::No
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. },
                ..
            }) => {
                // BACK is navigation, never an answer: Cancel in Settings (the surface pops),
                // the picker on first run
                if self.settings {
                    Handled::No
                } else {
                    fx.push(Fx::App(AppFx::Loop(LoopReq::OnboardBack)));
                    Handled::Yes
                }
            }
            // **First run corrects its own entry point** (the identical problem
            // `ConsentPage::first_run` solves for the sign-in's own first screen — see that
            // constructor's doc for the general shape). The container's generic mount path always
            // asks for `ContainerGroup(TABLE_GROUP)` on a fresh entry (`family.rs`'s own doc, and
            // `containers/stack.rs`'s `Self::fresh`) — right for every OTHER mode this screen has
            // (the Settings editor really does want to land on its list), wrong for first run,
            // whose whole point is that the one action pill IS the interaction and the reading
            // list beside it is only what you may read FIRST.
            //
            // **This is NOT done at construction, unlike `ConsentPage::first_run`, and that is a
            // deliberate departure from the fix that screen made rather than an oversight.**
            // Consent is always hosted inside a `RouteSurface`, whose `forward` intercepts an
            // inner page's `Enter` DURING the surface's own `ScreenEvent::Mount` processing and
            // re-addresses it to the surface's own outer instance — which means the correction
            // is generated (and so queued) strictly AFTER the sibling default `Enter` for that
            // same mount has already been read out into the SAME batch. This screen has no such
            // surface in front of it on the first-run path: `Route::Onboard` mounts directly as a
            // page of the app's own OUTER stack (`app/bridge.rs`'s `AppMounter::mount`), so a
            // correction pushed from ITS constructor would land in the exact same lifecycle batch
            // as the container's own default — `stack.rs::apply`'s `[Life::Mount(new),
            // Life::Ev(new, Enter(fresh TABLE_GROUP))]` pair — and BEFORE that default, not after
            // it (`Dispatcher::mount` appends the constructor's own emissions to `post`
            // immediately after the `Mount` delivery, and the sibling `Enter` is appended right
            // behind THAT). Since the last `Enter` the engine processes for one mount is the one
            // whose seat survives (`dispatch.rs`'s `after_step` reseats unconditionally on every
            // `ScreenEvent::Enter`, with no "already seated" guard), a construction-time push here
            // would be BEFORE the default's own — and so lose to it, silently reproducing the
            // very bug this comment exists to prevent rather than fixing it.
            //
            // Reacting to the default `Enter` from inside `step` instead reproduces consent's
            // ordering the honest way: our correction is emitted DURING the processing of the
            // container's own default (inside this very call), which places it AFTER that
            // default's own re-seat outcome in the dispatcher's queue — the same relative order
            // consent gets from its surface's two-hop indirection, arrived at here without one.
            // Addressed through `fx.from()` rather than a stored `InstanceId`, because this
            // constructor is never told which instance the container minted for it and does not
            // need to be: `fx.from()` is exactly the address `execute_deliver` stamped this very
            // `Effects` sink with, which is this screen's own real instance every time — a
            // literal `InstanceId` threaded through `first_run`'s signature would not be more
            // correct, only more places for it to go stale.
            //
            // The guard on `g != BAND_GROUP` is what keeps this from re-firing on its OWN
            // correction once that comes back around: the second `Enter` this arm ever sees for
            // one mount already names the band, so it falls to the catch-all below instead of
            // looping.
            ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(g),
            }) if !self.settings && *g != BAND_GROUP => {
                let me = fx.from();
                fx.push(Fx::Deliver(
                    me,
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::ContainerGroup(BAND_GROUP),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Enter(_) => Handled::Yes,
            _ => Handled::No,
        }
    }
}

impl<H: AppLike> Screen<H> for OnboardScreen {
    fn name(&self) -> &'static str {
        // **ONE word for both mountings, and the settings one is the reason it matters.** This is
        // "Onboard x2" (the module doc above): the SAME `Screen` impl is mounted once as a page of
        // the app's own outer stack (first run, where the heartbeat prints it as `route=onboard`)
        // and once as a page of the Settings family's INNER stack, where `RouteSurface::top_word`
        // reads it through this identical call and the heartbeat prints it as ` overlay=onboard`.
        //
        // It used to answer `word::SETTINGS` for the settings mounting, on the reasonable-looking
        // ground that the person is "in Settings" — and that made the `fps:settings-home` scene
        // print a heartbeat BYTE-IDENTICAL to `settings-root` and `settings-idle`. The harness
        // could then not tell "opened the Home-sources editor" from "opened Settings and did
        // nothing", so a boot target that silently failed to reach `SettingsPage::Favourites` — a
        // typo'd trigger value, or a future regression in `app/run.rs`'s boot-target match, which
        // falls back to the root — still produced a PASSING scene that was measuring the wrong
        // screen. The word names the SCREEN, not the ceremony it is standing in; `app/mod.rs`'s
        // `heartbeat_word_tests` carries the other half of this coupling.
        word::ONBOARD
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<Cow<'_, str>> {
        Some(Cow::Borrowed(if self.settings { CRUMB_SETTINGS } else { CRUMB_PROFILES }))
    }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, H>) {
        let p = f.painter;
        if !self.settings {
            // a first-run route is not a sheet, but it belongs to the same visual family
            self.ground.draw_home(Painter::root());
        }
        let layout = RouteLayout::screen();
        let body = self.body_copy();
        Header::new(
            layout,
            Some(if self.settings { CRUMB_SETTINGS } else { CRUMB_PROFILES }),
            if self.settings { SETTINGS_TITLE } else { TITLE },
            &body,
        )
        .paint(p);
        let labels = self.labels();
        let pal = if self.settings { palette() } else { self.ground.palette() };
        let mut band = BandPart {
            layout,
            labels: &labels,
            group: BAND_GROUP,
            entry: self.entry,
            uncommitted: false,
            scales: [self.pop.scale(0), 1.0],
            palette: pal,
            danger: None,
        };
        Part::<H>::draw(&mut band, f, layout.action);
        let lf = layout.sectioned_table();
        if self.table.n_rows() == 0 {
            let env = Env::inert();
            if crate::browse::discovery_state() == crate::browse::SecFetch::Failed {
                StatusOverlay::new(lf, c"Couldn't load libraries", StatusKind::Failed)
                    .reason(c"Check the connection, then try again.")
                    .draw(&env, p);
            } else {
                Spinner::new(lf.x + lf.w * 0.5, lf.y + StatusOverlay::CTRL_H, 22.0)
                    .phase(self.phase_ms as u32)
                    .tint(theme::TEXT_TERTIARY)
                    .draw(&env, p);
            }
            return;
        }
        let mut table = crate::ui::table_screen::TablePart {
            table: &self.table,
            frame: lf,
            group: TABLE_GROUP,
            entry: self.entry,
            uncommitted: false,
            has_band: !labels.is_empty(),
        };
        Part::<H>::draw(&mut table, f, lf);
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
    /// **Both mountings of this one screen name the same word**, which is what lets the
    /// `fps:settings-home` scene select samples that `fps:settings-root` cannot also match. The
    /// pairing this pins is stated at `Screen::name` and graded from the other side by
    /// `app/mod.rs`'s `heartbeat_word_tests`; without it, renaming either mounting's word is a
    /// silent change that only a device run would catch, and only by measuring the wrong screen.
    #[test]
    fn the_home_sources_editor_names_one_word_in_both_mountings() {
        use crate::ui::screen::Screen;
        let first = OnboardScreen::first_run(EntryId(0));
        let inside = OnboardScreen::settings(EntryId(0));
        assert_eq!(Screen::<InnerHost>::name(&first), super::word::ONBOARD);
        assert_eq!(Screen::<InnerHost>::name(&inside), super::word::ONBOARD);
        assert_ne!(Screen::<InnerHost>::name(&inside), super::word::SETTINGS);
    }

    use super::*;
    use crate::ui::machine::{FocusKey, FocusRead, InputOwner, InstanceId, MachineId, PressId, PressRead, Source, Stamped};
    use crate::ui::present::Present;

    use super::super::family::InnerHost;
    use super::super::registry::band_elem;

    /// **This screen's teardown, wrapped around the shared session guard.** Ported verbatim from
    /// `ui/onboard.rs`'s own `TempSession` — the only thing local to it now is `browse::reset()`,
    /// since there is no `SETTINGS_MODE` static left to put back: a screen instance simply goes
    /// out of scope with the test. A test using it must hold [`crate::testlock::serial`] for its
    /// whole body, exactly as the inner guard requires.
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
    impl Drop for TempSession {
        fn drop(&mut self) {
            crate::browse::reset();
            // the inner guard's own Drop runs after this and takes the redirect back
        }
    }

    /// A `Cx<InnerHost>` for driving [`OnboardScreen::step`] directly, with no dispatcher —
    /// `table_screen.rs`'s own test module builds the same shape for `FixtureHost`; this is its
    /// twin for the bundle the family's screens actually carry (`AppFx`/`AppMsg`), which is what
    /// makes `Machine::<InnerHost>::step` here the SAME code the Settings surface and the bridge
    /// both call — a screen generic over `H: AppLike` cannot be tested against a fixture that
    /// carries a different `Fx`/`Msg` pair, `FixtureHost` included.
    fn test_cx(m: &crate::ui::fixture::FixtureMeasure, focus: Option<u32>) -> Cx<'_, InnerHost> {
        Cx {
            views: (),
            tick: crate::ui::machine::Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead {
                current: focus.map(|elem| FocusKey { entry: EntryId(0), elem }),
            },
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    /// Drive one [`Machine::step`] with a fresh `Effects` sink, returning what it answered and
    /// what it emitted — the harness every test below drives the real `step` through, rather than
    /// calling `commit`/`toggle_row` as free functions the way the pre-restructure tests called
    /// `ui::onboard`'s (this screen no longer has any free function to call: everything is a
    /// method on the instance, reached exactly as the dispatcher reaches it).
    fn step_ev(s: &mut OnboardScreen, ev: &ScreenEvent<InnerHost>, focus: Option<u32>) -> (Handled, Vec<Stamped<InnerHost>>) {
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m, focus);
        let mut present = Present::new();
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        let handled = {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            Machine::<InnerHost>::step(s, ev, &cx, &mut fx)
        };
        (handled, buf)
    }

    fn key_ok_down() -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: crate::ui::machine::Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key: Key::Ok, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        })
    }

    fn key_back_down() -> ScreenEvent<InnerHost> {
        ScreenEvent::Input(InputEvent {
            at: crate::ui::machine::Tick::default(),
            source: Source::Script,
            kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
        })
    }

    /// Commit through a scratch `Effects` sink, for the tests that want to see what `commit`
    /// EMITS. **This no longer drains anything — `commit` leaves through `Fx::App(AppFx::Store)`
    /// now (see that function's own doc), so a test built on this harness alone can prove what got
    /// QUEUED and cannot prove the live table moved**: nobody here steps the store the returned
    /// `Fx::App` addresses, exactly as no test built on a scratch sink ever ran a real `Fx::Nav`
    /// either. A test that wants to see the live pin move belongs at `app/bridge.rs`'s own level,
    /// where a real `Dispatcher` drains what a screen pushes.
    fn commit_now(s: &mut OnboardScreen) -> Vec<Stamped<InnerHost>> {
        let mut present = Present::new();
        let mut buf: Vec<Stamped<InnerHost>> = Vec::new();
        {
            let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            s.commit(&mut fx);
        }
        buf
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The copy names the PEOPLE, and it has to read as a sentence at one, two and three friends —
    /// the case a hand-built "friend1, friend2, " string gets wrong at exactly one of them.
    ///
    /// **The names here are invented, and that is a rule rather than a preference.** This
    /// repository is PUBLIC and a test fixture is as public as a README; any new fixture that
    /// stands in for a real person uses a made-up name (`ui/onboard.rs`'s own copy of this test,
    /// which this is ported from, carries the fuller account of why).
    #[test]
    fn the_copy_lists_the_people_who_shared_with_you() {
        assert_eq!(join_names(&names(&[])), None);
        assert_eq!(join_names(&names(&["ada"])).as_deref(), Some("ada"));
        assert_eq!(join_names(&names(&["ada", "kate.w"])).as_deref(), Some("ada and kate.w"));
        assert_eq!(
            join_names(&names(&["ada", "kate.w", "dad"])).as_deref(),
            Some("ada, kate.w and dad")
        );
    }

    #[test]
    fn missing_owner_handles_never_invent_an_extra_server() {
        let copy = body_copy_for(&[]);
        assert_eq!(
            copy,
            "Choose the libraries this television shows. Your favorites fill Home's shelves and the Library's own tabs; Settings lists every one you have."
        );
        assert!(!copy.contains("More than one server"));
    }

    /// The band's four states (`ui::onboard`'s `bottom_actions`, moved onto `has_band`): first run
    /// always offers its commit; a clean Settings editor has nothing to; a dirty one does; and a
    /// pristine editor over an empty roster still offers `Try again`, which is a real action even
    /// though nothing has been touched.
    #[test]
    fn the_band_expresses_forward_back_and_commit_as_distinct_states() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        assert!(
            OnboardScreen::first_run(EntryId(0)).has_band(),
            "first run always offers its commit"
        );

        crate::browse::seed_pins_for_test(&[true, true]);
        assert!(
            !OnboardScreen::settings(EntryId(0)).has_band(),
            "a clean Settings editor has nothing to commit, and no longer spends the band saying \
             how to leave"
        );

        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        s.toggle_row(0);
        assert!(s.has_band(), "Done appears after an edit");

        crate::browse::reset();
        assert!(
            OnboardScreen::settings(EntryId(0)).has_band(),
            "Retry is a real action even on a pristine editor"
        );
        crate::browse::reset();
    }

    /// **Issue 9 reproduction, first run.** Before the draft model, a toggle wrote through to the
    /// live pin AND recorded it at once, so the profile's next boot never saw this screen again
    /// however BACK was then pressed. `toggle_row` mutates only `draft`; [`OnboardScreen::commit`]
    /// is the one place that ever asks for the live table to move — and, since it now leaves
    /// through `Fx::App(AppFx::Store)` rather than calling the mutator itself, this test's own
    /// commit half can only prove what got QUEUED, not that the live table moved (`commit_now`'s
    /// own doc says why); a real `Dispatcher` actually draining that command is a claim for
    /// `app/bridge.rs`'s own tests to carry, not this file's.
    #[test]
    fn toggling_never_touches_the_live_pin_or_the_recorded_answer_until_commit() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("draft");
        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::first_run(EntryId(0));

        s.toggle_row(0);
        assert!(
            !s.draft_rows()[0].pinned,
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

        // BACK/Cancel is a pure discard under this model. A fresh mount (what a real BACK leaves
        // the next entry looking at) proves the live pin came through untouched — REBINDING `s`
        // rather than dropping the fresh instance and continuing with the stale one, so the toggle
        // below starts from a draft that matches the live table again rather than from the first
        // draft's already-toggled-off state (which would net the two toggles to a no-op).
        let mut s = OnboardScreen::first_run(EntryId(0));
        assert!(crate::browse::pinned(0), "a discarded draft leaves the live pin exactly where BACK found it");

        // Toggling and THEN committing is what actually queues the write.
        s.toggle_row(0);
        let effs = commit_now(&mut s);
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::ApplyPins(edits))))
                    if edits == &vec![(0, false), (1, true)]
            )),
            "commit emits the toggled draft as an ApplyPins store command"
        );
        assert!(
            crate::browse::pinned(0),
            "…and the live pin has still not moved by this call — applying it is the STORE's job \
             once the queued command is actually drained, not this screen's"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…nor has anything been recorded — recording is also a consequence of the command \
             actually running"
        );
    }

    /// The Settings-hosted twin: Done is `commit`'s other name, and it pops the surface's OWN
    /// stack (`Fx::Nav(NavOp::Pop)`) rather than answering the outer application — the mechanism
    /// is the same draft either way.
    #[test]
    fn settings_mode_dirty_and_persistence_track_the_draft_not_the_live_table() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("settings-draft");
        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        assert!(!s.dirty(), "nothing has been touched yet");

        s.toggle_row(0);
        assert!(s.dirty(), "the draft moved, so Done has something to commit");
        assert!(
            crate::browse::pinned(0),
            "Settings mode's toggle is a draft edit too — the live table still has not moved"
        );

        let effs = commit_now(&mut s);
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::ApplyPins(edits))))
                    if edits == &vec![(0, false), (1, true)]
            )),
            "Done emits the toggled draft as an ApplyPins store command rather than applying it \
             itself"
        );
        assert!(
            crate::browse::pinned(0),
            "…and the live table has still not moved by this call — the queued command is what \
             applies it, once a real dispatcher drains it"
        );
        assert!(
            effs.iter().any(|st| matches!(st.fx, Fx::Nav(NavOp::Pop))),
            "Done leaves by popping the surface's own stack, not by answering the app"
        );
    }

    /// **Codex review finding, round 1 (2026-09-04, `ui/onboard.rs`).** `dirty` compares
    /// `entry_pins` against `draft`. An UNTOUCHED row's live pin can drift out from under
    /// `entry_pins` purely from `resolve_pins` re-deriving its still-unrecorded default as a
    /// second source lands — which must not read as "dirty" the moment the world changes around
    /// it. `draft_rows` rides such drift into `entry_pins` for any row the draft still agrees with
    /// (i.e. the user never touched).
    #[test]
    fn an_untouched_rows_live_drift_is_absorbed_into_entry_not_read_as_an_edit() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("entry-drift");
        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        assert_eq!(s.entry_pins, vec![(0, true), (1, true)]);

        // Nobody touched anything — this is `resolve_pins` re-deriving an unrecorded default as a
        // second source lands, not a user press.
        crate::browse::set_pinned_for_test(1, false);
        s.rebuild(true);

        assert_eq!(
            s.entry_pins,
            vec![(0, true), (1, false)],
            "entry_pins rode the untouched row's drift, so dirty() reads no difference from draft"
        );
        assert!(
            !s.dirty(),
            "an untouched row's drift is not something Done should offer to commit either"
        );
    }

    /// **Codex review finding, 2026-09-04.** `browse::reset()` (a profile switch, or ordinary
    /// `sync_roster` maintenance while this screen is open) makes every section INDEX mean a
    /// different library, or none — so a `draft`/`entry_pins` built from the old indices would
    /// misapply. Historically red: removing `reseed_if_table_identity_changed`'s call from
    /// `rebuild` fails the assertions below.
    #[test]
    fn a_table_reset_mid_edit_discards_the_stale_draft_instead_of_misapplying_it() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("epoch-reset");
        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        s.toggle_row(0); // draft: section 0 off — a real, in-progress user edit
        assert!(s.dirty());

        // What `sync_roster`'s `reset()` does mid-session: the table's IDENTITY changes out from
        // under the open editor. Re-seed with a DIFFERENT shape, so a surviving stale index would
        // provably be answering for the wrong library if this guard did nothing.
        crate::browse::seed_pins_for_test(&[true, true, true]);
        s.rebuild(false);

        assert!(
            !s.dirty(),
            "the stale in-progress edit was discarded, not carried forward against new indices"
        );
        let effs = commit_now(&mut s);
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::ApplyPins(edits))))
                    if edits == &vec![(0, true), (1, true), (2, true)]
            )),
            "the queued command carries the FRESH table's own state, never the pre-reset decision"
        );
    }

    /// **Codex review finding, 2026-09-04.** A freshly-landed row used to be seeded into `draft`
    /// alone, never `entry_pins`, so `dirty`'s comparison had no entry to compare a toggle on that
    /// row against — `Done` would never appear for an edit made on a library that landed after
    /// this screen opened.
    #[test]
    fn a_freshly_landed_row_can_independently_make_settings_dirty() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("late-row");
        crate::browse::seed_pins_for_test(&[true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        assert!(!s.dirty());

        // A second library lands mid-edit — an ADDITIVE append, exactly like a real second source
        // answering, never a reset.
        crate::browse::land_pin_for_test(true);
        s.rebuild(true);
        assert!(!s.dirty(), "landing alone is not an edit");

        s.toggle_row(1);
        assert!(
            s.dirty(),
            "a toggle on a freshly-landed row must make Done appear, or it can never be committed"
        );
    }

    /// The never-empty floor: the LAST pinned library on the draft refuses to turn off, judged
    /// against the draft's own count so it tracks every toggle made this session rather than only
    /// what is on disk. Not a dedicated test in `ui/onboard.rs` — it was only ever exercised in
    /// passing there — but the task this phase carries names it explicitly, so it gets one here.
    #[test]
    fn the_last_pinned_library_cannot_be_turned_off() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        crate::browse::seed_pins_for_test(&[true, false]);
        let mut s = OnboardScreen::first_run(EntryId(0));
        assert!(s.draft_rows()[0].pinned, "section 0 starts as the only pinned library");

        s.toggle_row(0); // the only pinned library refuses to turn off
        assert!(s.draft_rows()[0].pinned, "turning off the last favourite is refused");

        // Turning the second one on first frees the floor, and the first can then turn off.
        s.toggle_row(1);
        assert!(s.draft_rows()[1].pinned);
        s.toggle_row(0);
        assert!(!s.draft_rows()[0].pinned, "with a second library on, the first is free to turn off");

        crate::browse::reset();
    }

    /// **Drives the REAL BACK/Cancel path through `step`**, not a stand-in for it — first run's
    /// BACK asks the loop to leave (`LoopReq::OnboardBack`); Settings' BACK is a plain
    /// `Handled::No`, which is what lets `RouteSurface::step` pop its OWN stack rather than this
    /// screen popping anything itself (`screens/settings.rs`'s `Input` arm). Either way nothing is
    /// written: BACK/Cancel is a pure discard under the draft model, so there is nothing to
    /// restore and the live pin and the recorded answer must both be exactly where they started.
    #[test]
    fn back_through_the_real_step_path_touches_neither_the_live_pin_nor_the_record() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("real-back");

        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::first_run(EntryId(0));
        s.toggle_row(0);
        let (handled, effs) = step_ev(&mut s, &key_back_down(), None);
        assert_eq!(handled, Handled::Yes, "first run's BACK is its own answer to the key");
        assert!(effs
            .iter()
            .any(|st| matches!(st.fx, Fx::App(AppFx::Loop(LoopReq::OnboardBack)))));
        assert!(crate::browse::pinned(0), "first-run BACK left the live pin exactly as it was");
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and recorded nothing"
        );

        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        s.toggle_row(0);
        let (handled, effs) = step_ev(&mut s, &key_back_down(), None);
        assert_eq!(handled, Handled::No, "Settings BACK is the surface's own stack to pop");
        assert!(effs.is_empty(), "…and this screen asks for nothing on the way out");
        assert!(crate::browse::pinned(0), "Settings-mode BACK left the live pin exactly as it was");
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and recorded nothing — there was nothing to restore"
        );
    }

    /// **A press commits the VERB it was armed on.** `armed_kind`'s own doc has the mechanism;
    /// this is `ui::onboard`'s
    /// `an_action_that_changes_verb_under_an_armed_press_refuses_to_commit`, ported onto `step`.
    /// The OK-down on the band records `Retry`; the roster then lands, silently turning the SAME
    /// stop into `Start watching`; the commit that follows must refuse rather than perform either
    /// verb — and an unchanged verb must still commit normally.
    #[test]
    fn an_action_that_changes_verb_under_an_armed_press_refuses_to_commit() {
        let _g = crate::testlock::serial();
        // **The scratch session is load-bearing here, not boilerplate.** This test asserts that a
        // refused commit RECORDED NOTHING, and `pins_for` reads whatever `auth.json` the session
        // layer resolves — which off a developer's own machine is that household's real one. It
        // already holds a pin record for the empty profile key, so the assertion read `Some`
        // before this test had done anything at all and failed on the maintainer's Mac while
        // saying "must record nothing". Every other test in this module that names `pins_for`
        // takes one of these; this one was written without it. `plex::session::TempSession`'s own
        // doc is the full account of why the guard exists.
        let _t = TempSession::new("armed-verb");
        crate::browse::reset();
        let mut s = OnboardScreen::first_run(EntryId(0));
        assert_eq!(s.action_kind(), ActionKind::Retry, "nothing discovered yet");

        let band = band_elem(0);
        let (handled, _) = step_ev(&mut s, &key_ok_down(), Some(band));
        assert_eq!(handled, Handled::No, "the engine still arms the press itself");
        assert_eq!(s.armed_kind, Some(ActionKind::Retry));

        // …the roster lands while the press is still springing back.
        crate::browse::seed_two_source_table_for_test();
        s.rebuild(true);
        assert_eq!(s.action_kind(), ActionKind::Start, "the same stop, a different verb");

        let (_, effs) = step_ev(&mut s, &ScreenEvent::PressCommit(PressId(1)), Some(band));
        assert!(
            !effs
                .iter()
                .any(|st| matches!(st.fx, Fx::App(AppFx::Loop(LoopReq::OnboardDone)))),
            "the deferred commit must refuse rather than answer the first-run question"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and must record nothing — nothing here is an answer the user gave"
        );

        // …and an unchanged verb still commits normally: arm again at the NEW truth, then commit.
        step_ev(&mut s, &key_ok_down(), Some(band));
        assert_eq!(s.armed_kind, Some(ActionKind::Start));
        let (_, effs) = step_ev(&mut s, &ScreenEvent::PressCommit(PressId(2)), Some(band));
        assert!(effs
            .iter()
            .any(|st| matches!(st.fx, Fx::App(AppFx::Loop(LoopReq::OnboardDone)))));

        crate::browse::reset();
    }

    /// **First run corrects a mismatched default seat to the band** — the `ScreenEvent::Enter`
    /// arm's own doc has the full mechanism and why it cannot run at construction the way
    /// `ConsentPage::first_run`'s does. This drives `step` directly rather than through a real
    /// dispatcher, so it can only prove the LOCAL half: given the container's own default `Enter`
    /// (`ContainerGroup(TABLE_GROUP)`), first run answers with a corrective `Enter` naming the
    /// band, addressed to `fx.from()`; an `Enter` that already names the band is left alone
    /// (proving the guard that stops this from re-firing on its own correction); and the Settings
    /// editor — which really does want the table — never corrects at all. Whether that corrective
    /// `Enter`, once it actually reaches a live dispatcher, wins the seat over the container's own
    /// default is a claim about `ui/dispatch.rs`'s queue ordering that no unit test against `step`
    /// alone can settle; the comment above traces it, but only a `ui-sim`/device boot of first run
    /// landing focus on "Start watching" rather than the library list closes the loop for real.
    #[test]
    fn first_run_corrects_a_default_seat_on_the_table_to_the_band() {
        let _g = crate::testlock::serial();
        crate::browse::reset();
        let mut s = OnboardScreen::first_run(EntryId(0));
        let default_seat = ScreenEvent::Enter(Enter::Fresh {
            focus: FocusTarget::ContainerGroup(TABLE_GROUP),
        });
        let (handled, effs) = step_ev(&mut s, &default_seat, None);
        assert_eq!(handled, Handled::Yes);
        assert!(
            effs.iter().any(|st| matches!(
                st.fx,
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::ContainerGroup(g)
                }))) if g == BAND_GROUP
            )),
            "a default seat on the table must be answered with a corrective Enter naming the band"
        );

        // The correction must not loop: once the seat already names the band, step answers with
        // nothing further of its own.
        let already_band = ScreenEvent::Enter(Enter::Fresh {
            focus: FocusTarget::ContainerGroup(BAND_GROUP),
        });
        let (_, effs) = step_ev(&mut s, &already_band, None);
        assert!(effs.is_empty(), "an Enter that already names the band must not be re-corrected");

        // Settings mode wants exactly the default it is given — the list — so it must never
        // correct anything.
        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        let (_, effs) = step_ev(&mut s, &default_seat, None);
        assert!(effs.is_empty(), "the Settings editor's default seat on the table is the one it wants");
        crate::browse::reset();
    }

    /// **Ported from TWO old tests with byte-identical bodies** —
    /// `ui/onboard.rs`'s `start_and_back_cannot_answer_before_a_real_library_lands` and
    /// `start_cannot_answer_before_a_real_library_lands_and_back_never_answers` both asserted
    /// exactly `commit() == Action::None` and `key(SDLK_ESCAPE, 0) == Action::Back`, and neither
    /// one ever wrote `SETTINGS_MODE` — the flag whose value is the whole difference between "first
    /// run" and "the Settings editor", and the thing either name promises the test is about. What
    /// the assertions actually ran against was `SETTINGS_MODE`'s default (`false`), OR whatever a
    /// prior test in the same `testlock::serial()`-ordered run happened to leave it holding — which
    /// is precisely the cross-test global-pollution shape this repo's `Test-suite global pollution`
    /// memory warns about, and the reason two differently named tests read as one: neither actually
    /// pinned the mode its own name claims to be testing.
    ///
    /// The new architecture removes the possibility rather than fixing the test — there is no
    /// `SETTINGS_MODE` static left to leak between tests; `OnboardScreen::first_run` and
    /// `::settings` are two explicit constructors, so a test states its own mode by construction
    /// and cannot inherit one from whatever ran before it. The honest re-proof is therefore ONE
    /// test that names both modes explicitly, which is a strictly stronger claim than either half
    /// of the old pair made on its own: this asserts the "cannot answer" behaviour for BOTH
    /// flavours by construction, where the old suite asserted it for exactly one, by accident,
    /// under two different names.
    #[test]
    fn commit_and_back_both_refuse_to_answer_before_a_real_library_lands() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("no-library-yet");
        crate::browse::reset(); // no discovered libraries at all, in either flavour

        // First run: `commit`'s own doc says why a re-queue rather than a refusal is the honest
        // shape — "the skip is honest precisely because it records what the screen was showing
        // rather than deferring the question to a prompt that never comes" — and that skip is
        // BACK's alone; the pill itself must never treat an empty roster as an answered question.
        let mut s = OnboardScreen::first_run(EntryId(0));
        let effs = commit_now(&mut s);
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::RetryDiscovery)))
            )),
            "commit with no discovered libraries queues a retry, not an ApplyPins"
        );
        assert!(
            !effs.iter().any(|st| matches!(st.fx, Fx::App(AppFx::Loop(LoopReq::OnboardDone)))),
            "…and never leaves for Home — there is nothing here to have answered"
        );
        let (handled, effs) = step_ev(&mut s, &key_back_down(), None);
        assert_eq!(handled, Handled::Yes, "first run's BACK is its own answer to the key");
        assert!(
            effs.iter().any(|st| matches!(st.fx, Fx::App(AppFx::Loop(LoopReq::OnboardBack)))),
            "BACK navigates without recording an empty answer"
        );
        assert!(
            crate::plex::session::peek()
                .pins_for(&crate::plex::session::current_profile_key())
                .is_none(),
            "…and BACK recorded nothing"
        );

        // The Settings-hosted twin asks the same question of `commit` alone: its BACK/Cancel is
        // already proven to touch neither the live pin nor the record
        // (`back_through_the_real_step_path_touches_neither_the_live_pin_nor_the_record`), which is
        // a claim about LEAVING; this is the claim about the one ACTION a pristine, empty editor
        // still offers — Done must refuse to treat "nothing was ever discovered" as "the answer is
        // to keep nothing pinned".
        let mut s = OnboardScreen::settings(EntryId(0));
        let effs = commit_now(&mut s);
        assert!(
            effs.iter().any(|st| matches!(
                &st.fx,
                Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::RetryDiscovery)))
            )),
            "Settings' Done, pressed with nothing discovered, also queues a retry rather than a \
             commit"
        );
        assert!(
            !effs.iter().any(|st| matches!(st.fx, Fx::Nav(NavOp::Pop))),
            "…and never pops the surface — there is nothing to leave with"
        );
    }

    /// **Codex review finding, round 2 (2026-09-04, `ui/onboard.rs`).** The narrower race the
    /// sibling test above (`an_untouched_rows_live_drift_is_absorbed_into_entry_not_read_as_an_
    /// edit`) leaves open: a row the user HAS toggled (so `entry_pins` and `draft` genuinely
    /// disagree) that ALSO drifts independently, live, to a value nobody chose through this screen
    /// at all — `resolve_pins` re-deriving an unrecorded default is not gated on whether the user
    /// has touched that particular row. (A `bool` has only two states, so the drift necessarily
    /// lands on either `entry_pins`'s original value or `draft`'s edited one; this test picks the
    /// latter — the SAME direction the user's own toggle went — deliberately, because that is the
    /// coincidence that made the OLD `back_action`'s "restore" look plausible instead of obviously
    /// wrong: it walked a `BASE` snapshot against the live table and called `browse::toggle_pin`
    /// for every difference, and with `BASE` frozen at the pre-toggle value that would have flipped
    /// the live pin AND recorded `asked: true` — a Cancel press persisting an answer nobody gave.)
    ///
    /// The draft model survived the migration unchanged — `draft_rows`'s untouched-row sync is
    /// still gated on `entry_pins == draft` and still skips a touched row for exactly that reason —
    /// so under the new architecture BACK/Cancel has nothing to restore in the first place: nothing
    /// is ever written before `commit`. This is what keeps it that way: the drifted value (`false`)
    /// is chosen to differ from the row's `entry_pins` snapshot (`true`), the one choice under
    /// which a restore loop would misfire.
    #[test]
    fn a_toggled_rows_independent_drift_is_never_restored_or_recorded_by_cancel() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("toggle-and-drift-race");
        crate::browse::seed_pins_for_test(&[true, true]);
        let mut s = OnboardScreen::settings(EntryId(0));
        s.toggle_row(0); // a real user edit: draft[0] = false, entry_pins[0] stays true

        // A second source answers mid-edit and `resolve_pins` re-derives section 0's own
        // still-unrecorded default independently of the user's press, landing on `false` — the
        // SAME direction the user's own toggle went in. `set_pinned_for_test` stands in for that
        // live mutation, exactly as the sibling test above does for an untouched row.
        crate::browse::set_pinned_for_test(0, false);
        s.rebuild(true);

        let (handled, effs) = step_ev(&mut s, &key_back_down(), None);
        assert_eq!(handled, Handled::No, "Settings BACK is the surface's own stack to pop");
        assert!(effs.is_empty(), "…and this screen asks for nothing on the way out");
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
        crate::browse::reset();
    }

    /// **DOWN off the last library reaches the action pill.** Reported: "on screens containing a
    /// table plus a bottom action such as Done, pressing Down while focused on the last table row
    /// should move focus to the bottom button. Currently the button is generally reachable only
    /// by pressing Left." It was: the old screen's DOWN arm was `table().move_sel(1)`
    /// unconditionally, which a clamped selection turned into a no-op at the end of the list.
    ///
    /// Under the new architecture there is no per-screen DOWN arm left to have gotten wrong: the
    /// table's own `Focusable` (`ui::geom::Table`) leaves its Down edge `EdgeRule::Geometric`
    /// (`ui/geom.rs`), which the shared engine resolves at an edge with a geometric search over
    /// every OTHER group the page declares (`ui::focus::geometric`) — so the honest re-proof
    /// drives that real, shared function over this screen's own real `groups()` (the table beside
    /// `ui::table_screen::BandPart`'s band), which is the level `OnboardScreen` actually
    /// contributes to the claim.
    ///
    /// **This deliberately does not go through a live `Dispatcher`.** A first pass did — mounting
    /// the screen on a real `Dispatcher<InnerHost>` and driving DOWN/UP as real key frames — and it
    /// is worth recording exactly why that road was abandoned rather than silently dropped: a real
    /// dispatcher delivers a real `ScreenEvent::Tick` every frame, and `OnboardScreen`'s own Tick
    /// arm calls `stores::browse::discover_pump()` for real, whose `sync_roster` half retires any
    /// source not present in `crate::plex::server_ids()`'s LIVE roster (`browse::mod.rs`'s own
    /// words: "Roster removal is an identity boundary, not a failed fetch"). `seed_pins_for_test`
    /// stamps its fabricated source with `crate::plex::current_server()` — a real-looking id
    /// nothing has actually registered — so the very first real Tick wiped the seeded roster back
    /// to zero rows, which read as "there is nowhere left to focus" and not as a focus-engine
    /// question at all. Registering a fake-but-live server to satisfy `sync_roster` shifted the
    /// failure rather than removing it: `sync_roster`'s OWN comparison of the fixture's hard-coded
    /// `client_addr`/`token_gen` (`0`/`0`) against the real client it had just registered read as a
    /// second mismatch and cleared `sections_done`/`counts_done` right back out from under the
    /// fixture. Driving the shared `geometric()` function directly, over this screen's own real
    /// `groups()`, proves the exact same claim — the table and the band find each other — without
    /// a live Tick pump in the loop to fight with a fixture never built to survive one.
    #[test]
    fn down_off_the_last_row_reaches_the_bottom_action() {
        let _g = crate::testlock::serial();
        crate::browse::seed_pins_for_test(&[true, true]);
        let s = OnboardScreen::first_run(EntryId(0));
        let m = crate::ui::fixture::FixtureMeasure;
        let cx = test_cx(&m, None);
        let mut groups: Vec<crate::ui::screen::GroupSpec> = Vec::new();
        crate::ui::screen::Focusable::<InnerHost>::groups(&s, &cx, &mut groups);
        let table = groups.iter().find(|g| g.id == TABLE_GROUP).expect("the table has a group");
        assert_eq!(table.len, 2, "two seeded rows");
        assert_eq!(
            table.edge[1],
            crate::ui::screen::EdgeRule::Geometric,
            "the table's DOWN edge is a geometric search, not a wall or a fixed link"
        );

        // The last row has no in-group neighbour below it: this is what makes it an EDGE case
        // (rule 2) rather than an ordinary `move_sel`.
        let last_row = FocusKey { entry: EntryId(0), elem: 1u32 };
        assert!(
            matches!(
                crate::ui::screen::Focusable::<InnerHost>::neighbour(&s, last_row, crate::ui::screen::Dir::Down, &cx),
                crate::ui::screen::Step::Edge
            ),
            "the last row has no row below it inside the table's own group"
        );

        // The real edge-rule resolution: the shared `geometric` search, over this screen's own
        // real `groups()`, from the last row's own real placement.
        let from = crate::ui::screen::Focusable::<InnerHost>::place(&s, &last_row.elem, &cx, crate::ui::screen::At::SpringTarget)
            .expect("the last row places");
        let dest = crate::ui::focus::geometric(&groups, TABLE_GROUP, from.rect, crate::ui::screen::Dir::Down);
        assert_eq!(
            dest.map(|g| g.id),
            Some(BAND_GROUP),
            "DOWN off the last library must reach the action, not sit on a clamped selection"
        );

        // …and UP comes straight back, so the two are one vertical walk (rule 3 — the same rule
        // `ui::route_screen`'s module doc states for the whole family): from the band, Up must
        // find the table again.
        let band_key = FocusKey { entry: EntryId(0), elem: band_elem(0) };
        let band_from = crate::ui::screen::Focusable::<InnerHost>::place(&s, &band_key.elem, &cx, crate::ui::screen::At::SpringTarget)
            .expect("the band places");
        let back = crate::ui::focus::geometric(&groups, BAND_GROUP, band_from.rect, crate::ui::screen::Dir::Up);
        assert_eq!(back.map(|g| g.id), Some(TABLE_GROUP), "UP off the band returns to the table");
        crate::browse::reset();
    }

    /// **A press the user walked away from must not swallow the next one.** Codex review,
    /// 2026-09-04, `ui/onboard.rs`: `press::cancel` clears the commit but leaves the press ACTIVE
    /// for the ~200ms of its spring-back, so under the OLD architecture an arm expired against
    /// `is_active` outlived the press that owned it — press the action, press UP into the list,
    /// press OK: nothing happened, because the row's OWN immediate activation was judged against
    /// a pill the user had already left, through ONE function (`on_ok`) that read a single shared
    /// `ARMED`/`is_active()` pair for both the pill and the row.
    ///
    /// **That whole judgement is STRUCTURALLY ELIMINATED, not merely fixed, and the elimination
    /// lives one layer below this screen.** `ui/dispatch.rs`'s `after_step`, the `Key::Ok`/
    /// `Edge::Down` arm, decides what an OK-down does purely from `kind_of`'s answer for
    /// whichever element is CURRENTLY focused: `Some((k, ElemKind::Bare)) => … ScreenEvent::
    /// Activate`, `Some((k, ek)) => … Fx::Press(PressArm{…})` — two disjoint match arms, and
    /// NEITHER reads `self.input.press`/`self.input.arm`, which is the only place a still-bouncing
    /// press lives. A table row here is `ElemKind::Bare` (`ui/table_screen.rs`'s `TablePart::
    /// groups`: "a row commits on the DOWN edge, as every row in the app does — no press dip"), so
    /// its activation is resolved by the DISPATCHER, off CURRENT focus, before this screen's own
    /// `step` ever has an opinion — never off what a DIFFERENT, still-bouncing press elsewhere is
    /// doing. There is no shared ambient state left for a row's own OK to be judged against a pill
    /// the user already left, because there is no code path connecting the two at all.
    ///
    /// What is left for THIS screen to keep true — and what would still fail if the coupling were
    /// reintroduced by a different road — is that its OWN `Activate` arm never grows a new
    /// dependency on `armed_kind` (the record of what the PILL, a different element entirely, was
    /// armed to do). `armed_kind` is set here to a value a real still-bouncing pill press would
    /// leave behind, before the row's own `Activate` arrives; the row must toggle exactly as if
    /// nothing were armed, and `armed_kind` itself must be untouched by an event that was never
    /// about the band at all.
    #[test]
    fn a_press_abandoned_for_the_list_does_not_swallow_the_row_s_own_ok() {
        let _g = crate::testlock::serial();
        crate::browse::seed_two_source_table_for_test();
        let mut s = OnboardScreen::first_run(EntryId(0));
        assert_eq!(s.action_kind(), ActionKind::Start, "two sources are seeded");
        s.armed_kind = Some(ActionKind::Start); // stands in for the pill's still-bouncing arm

        let section = crate::browse::all_source_rows()[0].section;
        let before = s.draft.iter().find(|(sec, _)| *sec == section).map(|(_, on)| *on);

        let (handled, effs) = step_ev(&mut s, &ScreenEvent::Activate(0), None);
        assert_eq!(handled, Handled::Yes);
        assert!(effs.is_empty(), "a row's own activation asks for nothing beyond the invalidate");

        let after = s.draft.iter().find(|(sec, _)| *sec == section).map(|(_, on)| *on);
        assert_ne!(
            before, after,
            "the row's own Activate toggled its draft — an armed pill on a DIFFERENT element never \
             gates it"
        );
        assert_eq!(
            s.armed_kind,
            Some(ActionKind::Start),
            "the row's activation does not read, let alone consume, the pill's own arm record"
        );
        crate::browse::reset();
    }
}
