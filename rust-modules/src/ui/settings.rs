//! Full-screen Settings modal over one frozen ambient sample of its host page.

use crate::ui::popover::Popover;
use crate::ui::route_screen::{RouteGround, RouteLayout};
use crate::ui::table::{Row, Section, TableView};
use crate::ui::widgets::ControlPalette;
use crate::ui::{theme, Painter, Rect, Spring};
use std::ptr::{addr_of, addr_of_mut};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    None,
    Home,
    Privacy,
    Legal,
    About,
}

static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new();
static mut ROWS: Vec<Action> = Vec::new();
static mut CHILD: Spring = Spring::at(0.0);
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
pub(crate) fn close() {
    pop().close();
    unsafe {
        GROUND_READY = false;
        (*addr_of_mut!(GROUND)).reset();
    };
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
pub(crate) fn on_updown(delta: i32) -> bool {
    if !is_open() || covered_by_child() {
        return false;
    }
    table().move_sel(delta);
    crate::ui::idle::invalidate();
    true
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
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    if !is_open() || covered_by_child() {
        return false;
    }
    if let Some(sel) = table().hit_row(list_frame(), mx, my) {
        table().sel = sel;
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
    pop().update(dt);
    let covered = covered_by_child();
    unsafe {
        (*addr_of_mut!(CHILD)).step(if covered { 1.0 } else { 0.0 }, 200.0, dt);
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
pub(crate) fn draw_scrim() {
    if is_open() && !covered_by_child() {
        pop().scrim(theme::alert::SCRIM_A);
    }
}
pub(crate) fn draw() {
    if !is_open() {
        return;
    }
    let pop = pop();
    // Opaque and drawn from a frozen four-colour envelope: child screens cannot invalidate or
    // recapture it, and no title/poster edge survives as a readable low-resolution square.
    let appear = pop.appear();
    let ground = Painter::root().alpha(appear);
    unsafe { (*addr_of_mut!(GROUND)).draw_host(ground) };
    if appear >= 0.995 {
        unsafe { GROUND_READY = true };
    }
    let child = unsafe { (*addr_of!(CHILD)).pos.clamp(0.0, 1.0) };
    // Keep the one shared ground, but submit none of the fully covered root's text/table work.
    // During push/back the parent remains visible and therefore draws until the spring reaches its
    // endpoint; at rest the child is the only content tree on the frame.
    if !root_content_visible(child) {
        return;
    }
    let p = pop
        .content_painter(0.0)
        .alpha(1.0 - child)
        .translate(-0.35 * Rect::FULL.w * child, 0.0);
    // No crumb: this is the ROOT of the route family. Every child names the place BACK returns to
    // on a caption line above its title, but the root's BACK leaves the family altogether — it
    // dismisses the modal back onto Home, the way every other overlay in the app does.
    RouteLayout::screen().draw_narrative(
        p,
        None,
        "Settings",
        "Settings apply to this Plex profile on this television. You can return here from the profile menu at any time.",
        theme::size::LABEL,
    );
    table().draw(p, list_frame());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_has_only_the_product_destinations() {
        let rows = [Action::Home, Action::Privacy, Action::Legal, Action::About];
        assert_eq!(rows.len(), 4);
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
}
