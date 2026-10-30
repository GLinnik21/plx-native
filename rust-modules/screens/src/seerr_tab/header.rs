//! The Seerr tab's header: the Library's one row (`plx_ui::page_header::PageHeader`). The two
//! sections stand on the left, "Requests" and "Discover", the one on view wearing the selected plate
//! (siblings, as a library's names are); OK on the other switches to it in place. The right cluster
//! is the section's own: on Requests a read-out saying whose requests the list holds
//! ("Everyone's requests", or "Your requests" for a user the server shows only their own), on
//! Discover the control that hides or shows the titles already on the server.
//!
//! This file DECLARES the row ([`Row::new`]); its layout, paint, motion (the plate and the focus
//! capsule that travel) and focus group are the ui's, as on every page that wears the row.
use plx_machine::machine::{EntryId, FocusKey, GroupId, Measure};
use plx_platform::i18n::msg;
use plx_ui::page_header::{HeaderFocus, HeaderItem, HeaderRow, PageHeader, PageHeaderSpec, Slot};
use plx_ui::widgets::{CtlPop, TabStrip};
use plx_ui::{Painter, Rect};

use super::{Section, AVAILABLE_ELEM, DISCOVER_ELEM, REQUESTS_ELEM};

/// The row laid out, with its stops.
pub struct Row {
    row: HeaderRow,
}

impl Row {
    /// The row for `section` on view, whether Discover shows the titles already on the server, and
    /// whether the list holds everybody's requests.
    pub fn new(measure: &dyn Measure, section: Section, show_available: bool, sees_all: bool) -> Self {
        let pill = |label: &str, on: bool| HeaderItem {
            label: label.to_owned(),
            selected_plate: on,
            focusable: true,
            ..Default::default()
        };
        let left = vec![
            pill(msg::seerr_section_requests(), section == Section::Requests),
            pill(msg::seerr_section_discover(), section == Section::Discover),
        ];
        let right = match section {
            Section::Requests => vec![HeaderItem {
                label: if sees_all { msg::seerr_requests_all() } else { msg::seerr_requests_own() }.to_owned(),
                readout: true,
                ..Default::default()
            }],
            Section::Discover => vec![HeaderItem {
                label: msg::seerr_discover_available().to_owned(),
                value: Some(if show_available { msg::seerr_discover_shown() } else { msg::seerr_discover_hidden() }.to_owned()),
                focusable: true,
                ..Default::default()
            }],
        };
        let page = PageHeader::new(PageHeaderSpec { left, right, ..Default::default() }, &[], measure);
        let layout = page.layout(plx_ui::consts::SCR_W, measure);
        let focusable: Vec<usize> = (0..layout.items.len()).filter(|&i| layout.items[i].focusable).collect();
        let lefts: Vec<usize> = focusable.iter().copied().filter(|&i| matches!(layout.items[i].slot, Slot::Left(_))).collect();
        let mut stops: Vec<(u32, usize)> = [REQUESTS_ELEM, DISCOVER_ELEM].into_iter().zip(lefts).collect();
        if let Some(control) = focusable.iter().copied().find(|&i| matches!(layout.items[i].slot, Slot::Right(_))) {
            stops.push((AVAILABLE_ELEM, control));
        }
        Self { row: HeaderRow::new(page, layout, stops) }
    }

    /// The stops' elements, left to right.
    pub fn elems(&self) -> Vec<u32> {
        self.row.stops.iter().map(|(elem, _)| *elem).collect()
    }

    /// Stop `elem`'s plate (its focus rect and hit target), in document space.
    pub fn plate(&self, elem: u32) -> Option<Rect> {
        self.row.plate(elem)
    }

    /// The row as one focus group (`id`), for the page's declaration of its header section.
    pub fn focus(&self, entry: EntryId, id: GroupId) -> HeaderFocus<'_> {
        self.row.focus(entry, id, (0.0, 0.0), Rect::FULL)
    }

    /// The read-out the right cluster carries, if it has one.
    pub fn readout(&self) -> Option<&str> {
        self.row.readout()
    }

    /// One frame of the row's motion on the page's `capsules` and `pop` ([`HeaderRow::step`]).
    pub fn step(&self, entry: EntryId, current: Option<FocusKey<u32>>, capsules: &mut TabStrip, pop: &mut CtlPop<1>, dt: f32) {
        self.row.step(entry, current, capsules, pop, dt);
    }

    /// Paint the row `dy` px from the document origin (a scrolled page: minus the scroll).
    pub fn draw(&self, p: Painter, dy: f32, capsules: &TabStrip, pop: f32) {
        self.row.draw(p, -dy, capsules, pop, None);
    }
}
