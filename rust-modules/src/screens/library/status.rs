//! Retained-view prose for the existing Library status surface.
use std::ffi::{CStr, CString};
use super::*;
use crate::ui::widgets::{StatusKind, StatusOverlay};

impl LibraryScreen {
    pub(super) fn status_overlay<'a, H: LibraryLike>(&self, cx: &Cx<'_, H>, caption: &'a CStr, reason: Option<&'a CStr>) -> StatusOverlay<'a> {
        let kind = match self.readout {
            Readout::Failed => StatusKind::Failed, Readout::Loading => StatusKind::Working,
            Readout::Empty | Readout::Grid => StatusKind::Empty,
        };
        let mut overlay = StatusOverlay::new(self.status_frame(), caption, kind).phase(cx.tick.ms)
            .focused(cx.focus.current == Some(self.key(RETRY)));
        if let Some(reason) = reason { overlay = overlay.reason(reason); }
        if self.readout == Readout::Failed { overlay = overlay.action(crate::i18n::msg::browse_action_retry_c()); }
        overlay
    }

    pub(super) fn status_rect<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<Rect> {
        if self.readout != Readout::Failed { return None; }
        let (caption, reason) = self.status_text(cx);
        self.status_overlay(cx, &caption, reason.as_deref()).action_frame_measured(cx.measure)
    }

    pub(super) fn status_text<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (CString, Option<CString>) {
        let directory = H::directory(cx);
        let listing = H::listing(cx);
        let (caption, reason) = match self.readout {
            Readout::Failed => {
                let source = directory.source().map(|(_, source)| source);
                let name = source.map(|source| source.name.as_str()).filter(|name| !name.is_empty()).unwrap_or(crate::i18n::msg::browse_library_server());
                let owner = source.map(|source| source.handle.as_str()).filter(|owner| !owner.is_empty());
                (crate::i18n::msg::browse_library_unreachable(name), owner.map(|owner| crate::i18n::msg::browse_library_shared_unreachable(owner)))
            }
            Readout::Empty => {
                let caption = if self.wanted_kind.is_some() { crate::i18n::msg::browse_library_no_matches().into() }
                    else if directory.sections().is_empty() { crate::i18n::msg::browse_library_empty().into() }
                    else if listing.unwatched() || listing.genre().is_some() { crate::i18n::msg::browse_library_no_matches().into() }
                    else if let Some(section) = directory.current().and_then(|i| directory.sections().get(i)) {
                        match section.kind {
                            SecKind::Movie => crate::i18n::msg::browse_library_no_movies(&section.row.title),
                            SecKind::Show => crate::i18n::msg::browse_library_no_shows(&section.row.title),
                        }
                    } else { crate::i18n::msg::browse_library_no_matches().into() };
                (caption, None)
            }
            Readout::Loading => (crate::i18n::msg::browse_library_loading().into(), None),
            Readout::Grid => (String::new(), None),
        };
        (CString::new(caption).unwrap_or_default(), reason.map(|reason| CString::new(reason).unwrap_or_default()))
    }

    pub(super) fn status_frame(&self) -> Rect {
        // The legacy readout occupies the fixed content region, inside the overscan frame.
        const STATUS_TOP: f32 = 232.0;
        Rect::new(MARGIN_X, STATUS_TOP, SCR_W - 2.0 * MARGIN_X,
            SCR_H - STATUS_TOP - crate::ui::consts::MARGIN_Y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("status_contract_tests.rs");
}
