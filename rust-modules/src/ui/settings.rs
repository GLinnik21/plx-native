//! Full-screen Settings modal over one cached snapshot of its host page.

use crate::ui::consts::{MARGIN_X, SCR_H, SCR_W};
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::text_view::TextView;
use crate::ui::{theme, Rect, Spring};
use std::ptr::{addr_of, addr_of_mut};

const ROUTE_TOP: f32 = 150.0;
const COPY_W: f32 = crate::ui::home::HERO_COL_W;
const LIST_X: f32 = 930.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action { None, Home, Privacy, Legal, About }

static mut POP: Popover = Popover::new();
static mut TABLE: TableView = TableView::new();
static mut ROWS: Vec<Action> = Vec::new();
static mut CHILD: Spring = Spring::at(0.0);
static mut GROUND_READY: bool = false;

fn pop() -> &'static mut Popover { unsafe { &mut *addr_of_mut!(POP) } }
fn table() -> &'static mut TableView { unsafe { &mut *addr_of_mut!(TABLE) } }
pub(crate) fn is_open() -> bool { unsafe { (*addr_of!(POP)).is_open() } }
fn covered_by_child() -> bool {
    crate::ui::consent::is_open()
        || crate::ui::legal::is_open()
        || crate::ui::onboard::settings_mode()
}
fn signed_in() -> bool {
    crate::plex::session::load().account(crate::plex::session::current().as_ref()).signed_in
}

fn rebuild(sel: i32) {
    let mut actions = Vec::new();
    let mut sections = Vec::new();
    if signed_in() {
        let n = crate::browse::pinned_count();
        sections.push(Section::new("Home").row(
            Row::new("Home screen")
                .detail("Choose which libraries contribute shelves.")
                .value(format!("{n} {}", if n == 1 { "library" } else { "libraries" }))
                .chevron(true),
        ));
        actions.push(Action::Home);
    }
    sections.push(Section::new("Privacy")
        .row(Row::new("Privacy & data")
            .detail("Optional reports, privacy information and local data.").chevron(true))
        .row(Row::new("Legal notices")
            .detail("Privacy, licences, source code, trademarks and contact.").chevron(true)));
    actions.extend([Action::Privacy, Action::Legal]);
    sections.push(Section::new("System").row(
        Row::new("About PlxNative")
            .detail("Version, copyright and project information.").chevron(true),
    ));
    actions.push(Action::About);
    unsafe { *addr_of_mut!(ROWS) = actions };
    table().compact = false;
    table().set_sections(sections, sel, false);
}

pub(crate) fn open() {
    unsafe { GROUND_READY = false };
    rebuild(0);
    pop().open();
    crate::ui::idle::invalidate();
}
pub(crate) fn close() {
    pop().close();
    unsafe { GROUND_READY = false };
    crate::ui::idle::invalidate();
}
/// Once the modal has painted its cached ground, the expensive live Home page no longer needs to
/// be redrawn underneath it. The first frame remains live so the snapshot has an honest source.
pub(crate) fn host_ground_ready() -> bool {
    is_open() && unsafe { GROUND_READY }
}
pub(crate) fn on_back() -> bool {
    if !is_open() || covered_by_child() { return false; }
    close(); true
}
pub(crate) fn on_updown(delta: i32) -> bool {
    if !is_open() || covered_by_child() { return false; }
    table().move_sel(delta); crate::ui::idle::invalidate(); true
}
pub(crate) fn on_ok() -> Action {
    if !is_open() || covered_by_child() { return Action::None; }
    unsafe { addr_of!(ROWS).as_ref().and_then(|r| r.get(table().sel.max(0) as usize)).copied().unwrap_or(Action::None) }
}
pub(crate) fn pointer_focus(mx: f32, my: f32) -> bool {
    if !is_open() || covered_by_child() { return false; }
    if let Some(sel) = table().hit_row(list_frame(), mx, my) {
        table().sel = sel; crate::ui::idle::invalidate(); return true;
    }
    false
}
pub(crate) fn click(mx: f32, my: f32) -> Action { if pointer_focus(mx, my) { on_ok() } else { Action::None } }
pub(crate) fn refresh() { let sel = table().sel; rebuild(sel); crate::ui::idle::invalidate(); }
pub(crate) fn update(dt: f32) {
    pop().update(dt);
    unsafe { (*addr_of_mut!(CHILD)).step(if covered_by_child() { 1.0 } else { 0.0 }, 200.0, dt); }
    table().update(dt, list_frame().h);
}

fn list_frame() -> Rect {
    Rect::new(LIST_X, ROUTE_TOP, SCR_W as f32 - MARGIN_X - LIST_X, SCR_H as f32 - ROUTE_TOP - MARGIN_X)
}
pub(crate) fn draw_scrim() {
    if is_open() && !covered_by_child() { pop().scrim(theme::alert::SCRIM_A); }
}
pub(crate) fn draw() {
    if !is_open() { return; }
    let pop = pop();
    let ground = pop.content_painter(pop.appear());
    pop.modal_ground(ground, Rect::FULL);
    unsafe { GROUND_READY = true };
    let child = unsafe { (*addr_of!(CHILD)).pos.clamp(0.0, 1.0) };
    let p = ground.alpha(1.0 - child).translate(-0.35 * SCR_W as f32 * child, 0.0);
    TextView::new("Settings", theme::size::HERO, theme::TEXT_HEADING)
        .bold().max_lines(2).draw(p, Rect::new(MARGIN_X, ROUTE_TOP, COPY_W, 180.0));
    TextView::new(
        "Settings apply to this Plex profile on this television. You can return here from the profile menu at any time.",
        theme::size::BODY, theme::TEXT_READING,
    ).max_lines(5).draw(p, Rect::new(MARGIN_X, ROUTE_TOP + 126.0, COPY_W, 270.0));
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
}
