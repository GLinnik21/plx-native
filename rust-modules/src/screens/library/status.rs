//! Retained-view prose for the existing Library status surface.
use std::ffi::CString;
use super::*;

impl LibraryScreen {
    pub(super) fn status_text<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (CString, Option<CString>) {
        let directory = H::directory(cx);
        let listing = H::listing(cx);
        let (caption, reason) = match self.readout {
            Readout::Failed => {
                let source = directory.source().map(|(_, source)| source);
                let name = source.map(|source| source.name.as_str()).filter(|name| !name.is_empty()).unwrap_or("server");
                let owner = source.map(|source| source.handle.as_str()).filter(|owner| !owner.is_empty());
                (format!("Can't reach {name}"), owner.map(|owner| format!("Shared by {owner} · your own server is fine.")))
            }
            Readout::Empty => {
                let caption = if directory.sections().is_empty() { "No libraries on this server".into() }
                    else if listing.unwatched() || listing.genre().is_some() { "Nothing here matches".into() }
                    else if let Some(section) = directory.current().and_then(|i| directory.sections().get(i)) {
                        format!("No {} in {}", section.kind.noun(), section.row.title)
                    } else { "Nothing here matches".into() };
                (caption, None)
            }
            Readout::Loading => ("Loading…".into(), None),
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
