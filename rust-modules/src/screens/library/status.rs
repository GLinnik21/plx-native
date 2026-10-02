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
        // centring in the content region, which dropped it well below them. Loading and the
        // empty answer keep the region.
        // Same untyped "can't reach" verdict as Home's — no typed cause here either, so the two
        // pages share the glyph.
        let glyph = self.clock_cause().map_or(crate::ui::icons::Icon::ServerBadgeMinus, |(_, glyph)| glyph);
        let mut overlay = StatusOverlay::new(self.status_frame(), caption, kind)
            .page(glyph).phase(cx.tick.ms)
            .focused(cx.focus.current == Some(self.key(RETRY)));
        // The tab strip (`draw_library_controls`) stays live above a failed section's read-out —
        // a section failing is not the app failing — so the glyph is told where that chrome's
        // bottom edge is, and would shrink to clear it if the strip ever reached the natural box
        // (`glyph_ceiling`'s own doc). At the current anchor it never does; the ceiling is a guard. `self.libraries` empty is exactly the condition `draw_library_controls` itself
        // uses to skip drawing the strip at all.
        if !self.libraries.is_empty() {
            overlay = overlay.glyph_ceiling(CONTENT_TOP + StatusOverlay::CTRL_H);
        }
        if let Some(reason) = reason { overlay = overlay.reason(reason); }
        if self.readout == Readout::Failed {
            overlay = overlay.action(super::super::plaintext_question::primary(self.plaintext.verdict()));
        }
        overlay
    }

    /// **The clock reason and glyph this read-out shows, if any** — the ONE rule, read by the text
    /// and the glyph alike: only a Failed read-out, and only when no plaintext offer holds the
    /// reason slot (that cause is the one the person can act on). From the held
    /// [`ClockWatch`](super::super::clock_readout::ClockWatch), so a frame's draw and hit rect agree.
    fn clock_cause(&self) -> Option<(&'static CStr, crate::ui::icons::Icon)> {
        if self.readout != Readout::Failed || self.plaintext.verdict().is_some() { return None; }
        self.clock.reason()
    }

    /// Follow the offer for the failed source's server (`plex::grant::offers`) and the clock fact
    /// about that same server (`net::keypin::blocked_for`), and take the question down once the
    /// read-out it was asked from no longer asks about that server. `true` when what the read-out
    /// shows changed, so the caller damages the frame (the reason line moves the action row).
    pub(super) fn watch_readout<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) -> bool {
        use super::super::plaintext_question::{asks, Near};
        let machine = (self.readout == Readout::Failed)
            .then(|| H::directory(cx).source().and_then(|(sid, _)| crate::plex::client_for(*sid)))
            .flatten()
            .map(|client| client.machine_id());
        let offer_moved = self.plaintext.refresh(machine, Near::Only);
        let clock_moved = self.clock.refresh(machine);
        if self.plaintext_alert.is_open()
            && !(self.readout == Readout::Failed
                && self.plaintext_alert.subject() == self.plaintext.verdict().map(|v| v.machine_id.as_str())
                && asks(self.plaintext.verdict()))
        {
            self.plaintext_alert.withdraw();
        }
        offer_moved || clock_moved
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
                // Your own server is "your Plex server", the words Home uses for the same fault;
                // a borrowed one is named, since "your" would be untrue of it.
                let caption = match owner {
                    None => crate::i18n::msg::browse_home_failed().to_string(),
                    Some(_) => crate::i18n::msg::browse_library_unreachable(name),
                };
                // A server discovery offers "Connect without encryption?" for says why instead,
                // and names what *Connect* / *Try again* does (`auth::plaintext_copy`).
                let reason = match self.plaintext.verdict() {
                    Some(verdict) => Some(crate::auth::plaintext_copy(Some(verdict),
                        crate::auth::ReadoutSurface::SignedIn).into_owned()),
                    // else a wrong clock, then who shares the server.
                    None => match self.clock_cause() {
                        Some((reason, _)) => Some(reason.to_string_lossy().into_owned()),
                        None => owner.map(|owner| crate::i18n::msg::browse_library_shared_unreachable(owner)),
                    },
                };
                (caption, reason)
            }
            Readout::Empty => {
                let caption = if self.wanted_kind.is_some() { crate::i18n::msg::browse_library_no_matches().into() }
                    else if directory.sections().is_empty() { crate::i18n::msg::browse_library_empty().into() }
                    else if listing.unwatched() || listing.genre().is_some() { crate::i18n::msg::browse_library_no_matches().into() }
                    else if let Some(section) = directory.current().and_then(|i| directory.sections().get(i)) {
                        listing.library_type().empty_readout(section.kind, &section.row.title)
                    } else { crate::i18n::msg::browse_library_no_matches().into() };
                (caption, None)
            }
            Readout::Loading => (crate::i18n::msg::browse_library_loading().into(), None),
            Readout::Grid => (String::new(), None),
        };
        (CString::new(caption).unwrap_or_default(), reason.map(|reason| CString::new(reason).unwrap_or_default()))
    }

    pub(super) fn status_frame(&self) -> Rect {
        // Under shelves, an empty answer stands in its own document band below the heading row
        // (`Layout::empty_band`) and scrolls with it; the fixed region would put it on a shelf.
        if let Some((top, h)) = self.layout.empty_band().filter(|_| self.layout.shelves > 0) {
            return Rect::new(MARGIN_X, CONTENT_TOP + top - self.scroll.pos, SCR_W - 2.0 * MARGIN_X, h);
        }
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
