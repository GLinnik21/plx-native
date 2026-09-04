//! Full-screen Settings modal over one frozen ambient sample of its host page.

use crate::ui::popover::Popover;
use crate::ui::route_screen::{RouteFocus, RouteGround, RouteLayout, RoutePush, RouteShape, RouteStep};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::ControlPalette;
use crate::ui::{theme, Painter, Rect};
use std::ptr::{addr_of, addr_of_mut};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    None,
    Home,
    Privacy,
    Legal,
    About,
}

/// `caching_host` since 2026-09-03, for the two RAMPS and nothing else. At rest the opaque ground
/// owns the frame and `app.rs` skips the host page entirely (`host_ground_ready`), so a settled
/// Settings never needed the snapshot — but while the modal fades IN the page under it was drawn
/// live on every frame (Home's hero and shelves plus this ground plus the scrim: the reported drop
/// on entry), and while it faded OUT the same page was re-rendered under a grey ground for as long
/// as the spring took, which on a page that costs 40 ms a frame is a fade that visibly crawls. Both
/// ramps now composite one snapshot quad. No `host::live` guard: this modal draws AFTER the page
/// closure, where the freeze is already lifted, and its own children (the Home editor, Privacy,
/// Legal) draw after it in turn. `own_motion` in [`update`] keeps its springs out of the snapshot's
/// damage test; `note_own_damage` rides its selection moves.
static mut POP: Popover = Popover::new().caching_host();
static mut TABLE: TableView = TableView::new();
static mut ROWS: Vec<Action> = Vec::new();
/// The root ↔ Home-editor/Privacy/Legal push — the family's shared [`RoutePush`], not a private
/// spring: its constants (`PUSH_K` 200, a 0.35 parent travel) were already hand-duplicated here
/// before this used the shared type, which is what made swapping them in a drop-in rather than a
/// retune.
static mut CHILD: RoutePush = RoutePush::new();
/// The root↔Home-editor push, SEPARATE from [`CHILD`] even though it is the identical
/// `RoutePush` type driven the identical way (same `update` shape, same constants). Sharing
/// `CHILD` itself was tried first and was wrong: `CHILD`'s driving input is `covered_by_child()`,
/// the OR of THREE children (Privacy, Legal, the Home editor), so ITS amount rises the instant
/// ANY of them opens — reusing it for [`home_editor_visible`]/[`child_push`] meant opening Privacy
/// or Legal ALSO satisfied "the Home editor has something to draw", and `app.rs` would call
/// `onboard::draw()` over whichever child was actually opening (Codex review, 2026-09-04, caught
/// before this shipped). Privacy and Legal need no equivalent of their own: each already owns its
/// own `Popover` for its entrance (`consent::open_settings`/`legal::open` call their own module's
/// `pop().open()`) and reads nothing from Settings for it. The Home editor is the one child with
/// no `Popover` of its own — it is a whole `Route`, drawn through `Painter::root()` like any other
/// page — so it is the one child that needs an external push at all, and it needs its OWN.
static mut HOME_PUSH: RoutePush = RoutePush::new();
/// Last frame's `onboard::settings_mode()` — the edge detector [`update`] uses to decide when to
/// [`RoutePush::sync_to`] [`HOME_PUSH`] from [`CHILD`]. **Not `HOME_PUSH.amount() == 0.0`**, which
/// was this static's first version and was wrong: `RoutePush` rides `gfx::spring`'s closed-form
/// exponential decay, which only APPROACHES its target and — as `home_editor_visible`'s own 0.001
/// threshold already documents — never reports exactly `0.0` in practice. So an equality guard was
/// true on the very FIRST frame Home ever opened (before anything had moved it) and false on every
/// later one (a settled-closed `HOME_PUSH` sits at some tiny positive residual, not exactly zero),
/// silently disabling the handoff after the Home editor's first use (Codex review, 2026-09-04,
/// round 3). A flag transition has no such asymptote to get wrong.
static mut HOME_WAS_OPENING: bool = false;
static mut GROUND_READY: bool = false;
static mut GROUND: RouteGround = RouteGround::new();

fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}
fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}
pub(crate) fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}
fn covered_by_child() -> bool {
    crate::ui::consent::is_open()
        || crate::ui::legal::is_open()
        || crate::ui::onboard::settings_mode()
}
fn root_content_visible(child: f32) -> bool {
    child < 0.995
}
fn signed_in() -> bool {
    crate::plex::session::load()
        .account(crate::plex::session::current().as_ref())
        .signed_in
}

fn rebuild(sel: i32) {
    let mut actions = Vec::new();
    let mut sections = Vec::new();
    if signed_in() {
        let n = crate::browse::pinned_count();
        sections.push(
            Section::new("Home").row(
                Row::new("Home screen")
                    .detail("Choose which libraries contribute shelves.")
                    .value(format!(
                        "{n} {}",
                        if n == 1 { "library" } else { "libraries" }
                    ))
                    .chevron(true),
            ),
        );
        actions.push(Action::Home);
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
    unsafe { *addr_of_mut!(ROWS) = actions };
    table().compact = false;
    table().header_ink = theme::TEXT_READING;
    table().set_sections(sections, sel, false);
}

pub(crate) fn open() {
    unsafe {
        GROUND_READY = false;
        (*addr_of_mut!(GROUND)).reset();
    };
    rebuild(0);
    pop().open();
    crate::ui::idle::invalidate();
}
/// BACK on the root: the modal fades out over the live host (`Popover::dismiss`). The frozen
/// ground is NOT reset here — it is what fades — and `open` resets it before the next entry.
pub(crate) fn close() {
    pop().dismiss();
    unsafe { GROUND_READY = false };
    crate::ui::idle::invalidate();
}
/// The INSTANT hide, for page teardown — the screen under this modal is being replaced (the data sweep
/// that ends on the sign-in screen), so there is nothing for a fade to fade over. Interactive exits use [`close`], which
/// runs the appear choreography backwards (`Popover::dismiss`); a teardown that used it would
/// leave `visible()` true with the old rows in the sheet, drawn over the incoming page until the
/// spring ran out (Codex review, 2026-09-02).
pub(crate) fn hide() {
    pop().close();
    unsafe { GROUND_READY = false };
    crate::ui::idle::invalidate();
}
/// Once the modal's entry fade has finished over its frozen ambient ground, the expensive host page
/// no longer needs to be redrawn underneath it.  It remains DRAWN (but never updated) during the
/// short fade so every frame composites over the same complete host rather than over swap-buffer
/// leftovers; at rest the opaque ground owns the frame by itself.
pub(crate) fn host_ground_ready() -> bool {
    is_open() && unsafe { GROUND_READY }
}
pub(crate) fn on_back() -> bool {
    if !is_open() || covered_by_child() {
        return false;
    }
    close();
    true
}
/// This route's shape, as `ui::route_screen`'s shared rules see it. **The root has no action
/// band** — every row is a destination — so rule 9 applies in full: RIGHT enters the row under
/// focus and LEFT leaves the modal, exactly as BACK does. That symmetry is what issue 6 asked for;
/// before it, this was the screen that answered neither key.
fn shape() -> RouteShape {
    let t = table();
    RouteShape {
        band: 0,
        rows: t.n_rows() > 0,
        at_last_row: t.at_last_row(),
        opens: t.row_opens(t.sel),
        // The root edits nothing — every row is a door — so BACK discards nothing and rule 9's
        // guard never engages here.
        uncommitted: false,
    }
}

pub(crate) fn on_updown(delta: i32) -> bool {
    if !is_open() || covered_by_child() {
        return false;
    }
    let mut f = RouteFocus::content();
    if let RouteStep::Scroll(d) = f.updown(shape(), delta) {
        table().move_sel(d);
    }
    crate::ui::popover::note_own_damage();
    crate::ui::idle::invalidate();
    true
}

/// LEFT/RIGHT — rules 8 and 9. Returns the [`Action`] a RIGHT-entered row opens, so the caller
/// performs it through exactly the same `perform_settings_action` an OK does.
pub(crate) fn on_left_right(delta: i32) -> Action {
    if !is_open() || covered_by_child() {
        return Action::None;
    }
    let mut f = RouteFocus::content();
    let s = shape();
    match if delta < 0 { f.left(s) } else { f.right(s) } {
        RouteStep::Enter => on_ok(),
        RouteStep::Back => {
            close();
            Action::None
        }
        _ => Action::None,
    }
}
pub(crate) fn on_ok() -> Action {
    if !is_open() || covered_by_child() {
        return Action::None;
    }
    unsafe {
        addr_of!(ROWS)
            .as_ref()
            .and_then(|r| r.get(table().sel.max(0) as usize))
            .copied()
            .unwrap_or(Action::None)
    }
}
/// **Rule 11.** It already existed, and `click` already went through it; what it never had was a
/// HOVER caller — `app.rs`'s motion ladder is keyed by `Route`, and this modal sits over `Home`,
/// so a pointer moved across the Settings root used to drive Home's own focus behind it. The
/// pointer arm in `app.rs` now asks the overlay chain first, in the same order the key ladder does.
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    // `covered_by_child` goes false the instant a child closes, which is right for a KEY (the next
    // UP belongs to this list) and wrong for a POSITIONAL hit: the root is still travelling back in
    // from `-0.35` of the screen width and its rows are not under these coordinates yet. Same for
    // the modal's own entrance. See `RoutePush::settled`.
    if !is_open() || covered_by_child() || !pop().appear_settled() {
        return false;
    }
    if !unsafe { (*addr_of!(CHILD)).settled(false) } {
        return false;
    }
    if let Some(sel) = table().hit_row(list_frame(), mx, my) {
        table().sel = sel;
        table().list_focused = true;
        crate::ui::popover::note_own_damage();
        crate::ui::idle::invalidate();
        return true;
    }
    false
}
pub(crate) fn click(mx: f32, my: f32) -> Action {
    if pointer_focus(mx, my) {
        on_ok()
    } else {
        Action::None
    }
}
pub(crate) fn refresh() {
    let sel = table().sel;
    rebuild(sel);
    crate::ui::idle::invalidate();
}
pub(crate) fn update(dt: f32) {
    // The modal's appear spring, its push and its table are the MODAL's motion, not the frozen
    // page's — `popover::own_motion`.
    let _own = crate::ui::popover::own_motion();
    pop().update(dt);
    let covered = covered_by_child();
    let opening_home = crate::ui::onboard::settings_mode();
    unsafe {
        let child = &mut *addr_of_mut!(CHILD);
        let home = &mut *addr_of_mut!(HOME_PUSH);
        // Hand off from CHILD's current spring state on the RISING EDGE of Home opening — see
        // `HOME_WAS_OPENING`'s own doc for why this has to be an edge on the FLAG rather than an
        // `amount() == 0.0` read of the spring, and `RoutePush::sync_to`'s doc for why the copy is
        // unconditional rather than gated on CHILD being away from rest: in the ordinary case (no
        // sibling was open) CHILD and HOME_PUSH have been driven identically since Home last
        // closed, so the copy is a no-op; in the interrupted case it is exactly the seed that
        // keeps the two complementary (Codex review, 2026-09-04).
        let was_opening = addr_of!(HOME_WAS_OPENING).read();
        if opening_home && !was_opening {
            home.sync_to(child);
        }
        HOME_WAS_OPENING = opening_home;
        child.update(covered, dt);
        // Driven by the Home editor ALONE, not `covered` — see `HOME_PUSH`'s own doc.
        home.update(opening_home, dt);
    }
    // The child owns every visible content pixel once mounted.  Keeping the covered root table's
    // three springs alive would be invisible work and could make them resume from a different
    // state on BACK, so freeze them with the page they belong to.
    if !covered {
        table().update(dt, list_frame().h);
    }
}

fn list_frame() -> Rect {
    RouteLayout::screen().sectioned_table()
}
pub(crate) fn control_palette() -> ControlPalette {
    unsafe { (*addr_of!(GROUND)).palette() }
}
/// The CHILD side of [`HOME_PUSH`] — [`RoutePush::parent`] above (on [`CHILD`]) is the ROOT's own
/// half of a DIFFERENT push; until this existed nothing ever called the half that matters to the
/// Home editor. `ui::onboard::draw` threads its ENTIRE screen through this in Settings mode
/// instead of a bare `Painter::root()`, which is what actually rides the spring: before it did,
/// the editor drew at full opacity and full position on its very first frame regardless of what
/// was happening around it.
pub(crate) fn child_push(p: Painter) -> Painter {
    unsafe { (*addr_of!(HOME_PUSH)).child(p) }
}
/// Whether the Home editor still has anything to draw at [`HOME_PUSH`]'s CURRENT amount — the
/// "amount decides, never the flag" rule `consent.rs`'s `stage_visible` already uses for its own
/// parent/child pair, needed here for the same reason: `onboard::settings_mode()` flips false on
/// the SAME frame the reverse spring starts (`app.rs`'s Done/Cancel exit runs
/// `onboard::finish_settings` inline with the commit), so a caller gating the DRAW on that flag
/// alone would cut the exit slide before its first frame. A first-run boot never opens Settings, so
/// [`HOME_PUSH`] never leaves zero there and this is always `false`.
///
/// **`0.001`, not `0.0`** — the same threshold `stage_visible`'s `Stage::Product` arm uses, and for
/// the same reason: [`RoutePush`] rides `gfx::spring`'s closed-form exponential decay, which only
/// ever approaches its target and in practice never lands on the exact bit pattern `0.0`. Gating on
/// bare positivity would leave this `true` forever after the editor's first open, one negligible
/// but perpetual draw call short of a screen that can never idle.
pub(crate) fn home_editor_visible() -> bool {
    unsafe { (*addr_of!(HOME_PUSH)).amount() > 0.001 }
}
pub(crate) fn draw_scrim() {
    if pop().visible() && !covered_by_child() {
        pop().scrim(theme::alert::SCRIM_A);
    }
}
pub(crate) fn draw() {
    if !pop().visible() {
        return;
    }
    let pop = pop();
    // Opaque and drawn from a frozen four-colour envelope: child screens cannot invalidate or
    // recapture it, and no title/poster edge survives as a readable low-resolution square.
    let appear = pop.appear();
    let ground = Painter::root().alpha(appear);
    crate::ui::profile::phase("st.ground", || unsafe { (*addr_of_mut!(GROUND)).draw_host(ground) });
    if appear >= 0.995 {
        unsafe { GROUND_READY = true };
    }
    let child = unsafe { (*addr_of!(CHILD)).amount() };
    // Keep the one shared ground, but submit none of the fully covered root's text/table work.
    // During push/back the parent remains visible and therefore draws until the spring reaches its
    // endpoint; at rest the child is the only content tree on the frame.
    if !root_content_visible(child) {
        return;
    }
    let p = unsafe { (*addr_of!(CHILD)).parent(pop.content_painter(0.0)) };
    // No crumb: this is the ROOT of the route family. Every child names the place BACK returns to
    // on a caption line above its title, but the root's BACK leaves the family altogether — it
    // dismisses the modal back onto Home, the way every other overlay in the app does.
    crate::ui::profile::phase("st.root", || {
    RouteLayout::screen().draw_narrative(
        p,
        None,
        "Settings",
        "Settings apply to this Plex profile on this television. You can return here from the profile menu at any time.",
        theme::size::LABEL,
    );
    table().draw(p, list_frame());
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_has_only_the_product_destinations() {
        let rows = [Action::Home, Action::Privacy, Action::Legal, Action::About];
        assert_eq!(rows.len(), 4);
    }

    /// Run the modal's entrance to rest. Rule 11 refuses a POSITIONAL hit until the layer it
    /// belongs to has arrived, so every pointer test in this family has to land its screen first.
    fn settle() {
        for _ in 0..600 {
            update(1.0 / 60.0);
        }
    }

    /// **Rules 8 and 9 on the root.** It answered neither LEFT nor RIGHT before, which is half of
    /// what issue 6 reports: Legal entered on RIGHT and left on LEFT while the screen one level
    /// above it ignored both. Every row here is a destination, so RIGHT is OK's twin; the root has
    /// no action band, so LEFT is BACK's.
    #[test]
    fn right_enters_the_focused_destination_and_left_leaves_the_modal() {
        let _g = crate::testlock::serial();
        open();
        assert!(is_open());
        let by_ok = on_ok();
        assert_ne!(by_ok, Action::None, "the focused row is a real destination");
        assert_eq!(
            on_left_right(1),
            by_ok,
            "RIGHT must open exactly what OK opens, not a second reading of the row"
        );
        assert_eq!(on_left_right(-1), Action::None, "…and LEFT opens nothing");
        assert!(
            !is_open(),
            "LEFT on a screen with no action band is the way out of it (rule 9)"
        );
    }

    /// **Rule 11.** Hover parks the row under the pointer and a click in dead space does nothing —
    /// the half this screen had no CALLER for, since `app.rs`'s motion ladder is keyed by `Route`
    /// and this modal sits over `Home`.
    #[test]
    fn hover_parks_the_row_under_the_pointer_and_dead_space_parks_nothing() {
        let _g = crate::testlock::serial();
        open();
        settle();
        let f = list_frame();
        let mut parked = std::collections::BTreeSet::new();
        for i in 0..60 {
            if pointer_focus(f.x + 40.0, f.y + 10.0 + i as f32 * 16.0) {
                parked.insert(table().sel);
            }
        }
        assert!(
            parked.len() >= 2,
            "hover must park DIFFERENT rows at different heights, parked {parked:?}"
        );
        let before = table().sel;
        assert!(
            !pointer_focus(f.x + 40.0, f.y - 400.0),
            "above the list is dead space"
        );
        assert_eq!(table().sel, before, "…and dead space moves nothing");
        assert_eq!(
            click(f.x + 40.0, f.y - 400.0),
            Action::None,
            "a click that parks nothing activates nothing"
        );
        close();
    }

    #[test]
    fn a_settled_child_fully_owns_content_but_not_the_shared_ground() {
        assert!(
            root_content_visible(0.994),
            "the parent participates during motion"
        );
        assert!(
            !root_content_visible(1.0),
            "the settled child culls the root"
        );
    }

    /// `CHILD`/`HOME_PUSH` are `static mut`s several tests here touch, so two running AT ONCE is
    /// an unsynchronized write to shared state regardless of what either one asserts —
    /// `testlock::serial()` is the crate-wide lock every such test holds. It stops two tests
    /// OVERLAPPING; it says nothing about what a test that PANICS partway through leaves for
    /// whoever runs next, which is why the reset is a Drop guard and not a call at the tail end of
    /// the function (Codex review, 2026-09-04: the first version of these tests reset only on the
    /// success path).
    struct ResetPushesOnDrop;
    impl Drop for ResetPushesOnDrop {
        fn drop(&mut self) {
            unsafe {
                (*addr_of_mut!(CHILD)).jump(false);
                (*addr_of_mut!(HOME_PUSH)).jump(false);
                // Codex review, 2026-09-04, round 4: added alongside `HOME_WAS_OPENING` itself —
                // left `true` after a test that entered Settings mode, the NEXT test's very first
                // `update()` would read a false-to-true edge that never really happened (the flag
                // says "was already opening" when nothing this test did opened anything), which is
                // exactly the kind of cross-test leak this guard exists to prevent.
                HOME_WAS_OPENING = false;
            }
            // Harmless for every test that never entered Settings mode (already false); the one
            // new test below does, through the real `onboard::enter_settings()`, and must not
            // leave it set for whoever runs next.
            crate::ui::onboard::finish_settings();
        }
    }

    /// **The Home editor's own frames answer `home_editor_visible`, both directions** — the fact
    /// that used to be missing entirely: nothing ever called [`RoutePush::child`], so the editor
    /// drew at full opacity from its first frame regardless of what `CHILD` was doing, and its BACK
    /// exit had no reverse animation to be visible DURING at all. `jump` stands in for a fresh
    /// mount/full dismissal (what `open`/nothing-covering leaves `CHILD` at); `update` is what a
    /// real frame does.
    #[test]
    fn the_home_editor_is_visible_only_while_its_push_has_amount() {
        let _g = crate::testlock::serial();
        let _reset = ResetPushesOnDrop;
        unsafe { (*addr_of_mut!(HOME_PUSH)).jump(false) };
        assert!(!home_editor_visible(), "at rest, nothing to draw");

        unsafe { (*addr_of_mut!(HOME_PUSH)).update(true, 1.0 / 60.0) };
        assert!(
            home_editor_visible(),
            "a step into the push and there is something on screen"
        );

        // Settle fully open, then reverse — mirroring Done/Cancel, whose exit starts the reverse
        // spring on the very frame `onboard::settings_mode()` already reads false.
        for _ in 0..60 {
            unsafe { (*addr_of_mut!(HOME_PUSH)).update(true, 1.0 / 60.0) };
        }
        unsafe { (*addr_of_mut!(HOME_PUSH)).update(false, 1.0 / 60.0) };
        assert!(
            home_editor_visible(),
            "the reverse spring's first step still has the editor on screen"
        );

        for _ in 0..60 {
            unsafe { (*addr_of_mut!(HOME_PUSH)).update(false, 1.0 / 60.0) };
        }
        assert!(
            !home_editor_visible(),
            "settled back at rest, the exit slide is over"
        );
    }

    /// **The bug Codex caught before it shipped.** `CHILD` rises for ANY of Settings' three
    /// children (Privacy, Legal, the Home editor) — that is exactly right for the ROOT's own
    /// fade/cull, which must happen no matter which one opened. `HOME_PUSH` must NOT rise for the
    /// other two, or `app.rs`'s `if home_editor_visible() { onboard::draw() }` gate would draw the
    /// Home editor's content over Privacy or Legal the instant either one opened, even though the
    /// Home editor itself was never entered (`onboard::settings_mode()` stays false throughout).
    #[test]
    fn opening_privacy_or_legal_never_marks_the_home_editor_visible() {
        let _g = crate::testlock::serial();
        let _reset = ResetPushesOnDrop;
        unsafe { (*addr_of_mut!(CHILD)).jump(false) };
        unsafe { (*addr_of_mut!(HOME_PUSH)).jump(false) };
        // `covered_by_child()` reads `consent::is_open()`/`legal::is_open()` directly and neither
        // is reachable from a host test without opening a real popover, so this drives `CHILD` the
        // way `update()` would when EITHER is open — `covered=true` — while leaving
        // `onboard::settings_mode()` at its real, untouched value (false, nobody entered it).
        for _ in 0..60 {
            let settings_mode = crate::ui::onboard::settings_mode();
            unsafe {
                (*addr_of_mut!(CHILD)).update(true, 1.0 / 60.0);
                (*addr_of_mut!(HOME_PUSH)).update(settings_mode, 1.0 / 60.0);
            }
        }
        assert!(
            unsafe { (*addr_of!(CHILD)).amount() } > 0.9,
            "the root's own push rose, as Privacy/Legal opening requires"
        );
        assert!(
            !home_editor_visible(),
            "but the Home editor's own push never moved — nobody entered it"
        );
    }

    /// **The gap Codex's round-2 review caught, and the SECOND gap its round-3 re-review caught in
    /// the first fix.** Splitting `HOME_PUSH` out of `CHILD` fixed the Privacy/Legal bleed above,
    /// but introduced a narrower one: if Home starts opening while `CHILD` has not yet settled
    /// back to 0 from a JUST-DISMISSED sibling (Privacy/Legal reversing out), the root (driven by
    /// `CHILD`, already partway faded) and the Home editor's content (driven by a `HOME_PUSH` that
    /// would otherwise start cold at 0) stop being one complementary crossfade — for a few frames
    /// neither side is where the other expects it to be. `RoutePush::sync_to` closes it by seeding
    /// `HOME_PUSH` from `CHILD`'s current state, but the FIRST version of this fix gated the seed
    /// on `HOME_PUSH.amount() == 0.0` — which is true on Home's very first-ever open (nothing has
    /// moved it yet) and FALSE ON EVERY OPEN AFTER THAT, because `RoutePush`'s spring only
    /// approaches its target and a settled-closed push sits at some tiny positive residual, never
    /// exactly zero. So this exact test passed with `HOME_PUSH.jump(false)` in its own setup
    /// (which forces the one case that works) and would have missed the SECOND-entry case
    /// entirely. This version opens and closes Home for real FIRST, with no `jump` anywhere near
    /// the scenario under test, so `HOME_PUSH` reaches that natural non-zero residual exactly as a
    /// real second open would meet it.
    #[test]
    fn opening_home_a_second_time_mid_reversal_of_a_dismissed_sibling_still_hands_off() {
        let _g = crate::testlock::serial();
        let _reset = ResetPushesOnDrop;
        crate::browse::reset();
        unsafe { (*addr_of_mut!(CHILD)).jump(false) };
        unsafe { (*addr_of_mut!(HOME_PUSH)).jump(false) };

        // Open Home for real and let it settle fully open.
        crate::ui::onboard::enter_settings();
        for _ in 0..90 {
            update(1.0 / 60.0);
        }
        assert!(home_editor_visible(), "fixture must reach a settled-open Home first");

        // Close it for real and let it settle fully closed — WITHOUT `jump`, so `HOME_PUSH` is
        // left at whatever `gfx::spring`'s exponential decay actually reaches, not an exact 0.0.
        crate::ui::onboard::finish_settings();
        for _ in 0..90 {
            update(1.0 / 60.0);
        }
        assert!(
            !home_editor_visible(),
            "fixture must reach a settled-closed Home before the scenario starts"
        );
        let residual = unsafe { (*addr_of!(HOME_PUSH)).amount() };
        assert_ne!(
            residual, 0.0,
            "the fixture must reproduce the natural non-zero residual an exact-zero guard \
             would have missed — if this is ever exactly 0.0, the spring model changed and \
             this test needs a different fixture, not the equality guard back"
        );

        // Stand in for Legal/Privacy opening and climbing partway — real `covered_by_child()`
        // inputs are not reachable from a host test (see the test above), but `CHILD` is the same
        // push `update()` drives either way, so stepping it directly reproduces the same
        // mid-flight state a real dismissal-in-progress would leave it at.
        for _ in 0..6 {
            unsafe { (*addr_of_mut!(CHILD)).update(true, 1.0 / 60.0) };
        }
        let mid = unsafe { (*addr_of!(CHILD)).amount() };
        assert!(
            (0.05..0.95).contains(&mid),
            "fixture must land mid-flight, not settled at either end: got {mid}"
        );

        // Home starts opening a SECOND time on this exact frame, before CHILD has reversed back
        // to rest.
        crate::ui::onboard::enter_settings();
        update(1.0 / 60.0);

        let home_amt = unsafe { (*addr_of!(HOME_PUSH)).amount() };
        let child_amt = unsafe { (*addr_of!(CHILD)).amount() };
        assert!(
            (home_amt - child_amt).abs() < 0.05,
            "HOME_PUSH ({home_amt}) must hand off from CHILD's mid-flight amount ({child_amt}) \
             instead of resuming from its own settled-closed residual ({residual})"
        );
    }
}
