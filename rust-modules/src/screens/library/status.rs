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
        // A failed source fills the page under the live chrome, so it stands on the shared page
        // lines (`StatusOverlay::page`) — level with Home's and the sign-in failure's — rather than
        // centring in the content region, which dropped it ~250px below them. Loading and the
        // empty answer keep the region.
        let mut overlay = StatusOverlay::new(self.status_frame(), caption, kind).page().phase(cx.tick.ms)
            .focused(cx.focus.current == Some(self.key(RETRY)));
        if let Some(reason) = reason { overlay = overlay.reason(reason); }
        if self.readout == Readout::Failed { overlay = overlay.action(c"Try again"); }
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
                let name = source.map(|source| source.name.as_str()).filter(|name| !name.is_empty()).unwrap_or("server");
                let owner = source.map(|source| source.handle.as_str()).filter(|owner| !owner.is_empty());
                // Your own server is "your Plex server", the words Home uses for the same fault;
                // a borrowed one is named, since "your" would be untrue of it.
                let caption = match owner {
                    None => crate::i18n::t("Can\u{2019}t reach your Plex server").to_string(),
                    Some(_) => crate::i18n::t("Can\u{2019}t reach {name}").replacen("{name}", name, 1),
                };
                (caption, owner.map(|owner| crate::i18n::t("Shared by {o} · your own server is fine.").replacen("{o}", owner, 1)))
            }
            Readout::Empty => {
                let caption = if self.wanted_kind.is_some() { "Nothing here matches".into() }
                    else if directory.sections().is_empty() { "No libraries on this server".into() }
                    else if listing.unwatched() || listing.genre().is_some() { "Nothing here matches".into() }
                    else if let Some(section) = directory.current().and_then(|i| directory.sections().get(i)) {
                        format!("No {} in {}", section.kind.noun(), section.row.title)
                    } else { "Nothing here matches".into() };
                (caption, None)
            }
            Readout::Loading => (crate::i18n::t("Loading…").into(), None),
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
