//! What one request row says (docs/seerr.md), composed from the request and its title's detail:
//! pure, so every wording is tested without a page.
//!
//! - the title, once its detail has landed (the request list names no title);
//! - line 1: the kind, the year and, for a show, the seasons asked for;
//! - line 2: who asked, and when;
//! - line 3: the download while one moves ("68% · 1.2 GB of 4.0 GB · 12m left"), else who answered
//!   it ("Approved by …", "Declined by …");
//! - the chip: what has become of it, the request's own state folded with the title's
//!   (`registry::seerr_cards::request_chip`, which a Seerr title page says too);
//! - the band: the download's progress.
use plx_data::seerr::{progress, Availability, MediaKind, MediaRequest, RequestStatus, TitleDetail};
use plx_platform::i18n::msg;
use plx_ui::fmt;

use crate::registry::seerr_cards::{answered_line, asked_line, dotted, download_line, request_chip};

/// One row's words, as the page draws it ([`plx_ui::media_row::MediaRow`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowView {
    pub id: i64,
    pub kind: Option<MediaKind>,
    pub tmdb: i64,
    pub title: String,
    pub lines: [String; plx_ui::media_row::MAX_LINES],
    pub chip: &'static str,
    pub progress: Option<f32>,
    /// The TMDB poster's URL (`plex::seerr::tmdb_image`).
    pub poster: Option<String>,
    /// Pending approval: a manager's press opens the approve / decline menu.
    pub pending: bool,
    /// Something about it is still moving (approved and not yet on the server): the page reads the
    /// list again while any row is.
    pub moving: bool,
}

/// "Seasons 1, 2" (or "Season 3"), `None` for a movie or a show with none named.
fn seasons(request: &MediaRequest) -> Option<String> {
    match request.seasons.as_slice() {
        [] => None,
        [one] => Some(fmt::season_identity(i64::from(*one), "")),
        many => {
            let list: Vec<String> = many.iter().map(|n| plx_platform::i18n::current().number(i64::from(*n))).collect();
            Some(msg::seerr_row_seasons(&list.join(", ")))
        }
    }
}

/// The row of `request`, with its title's `detail` once that has landed.
pub fn row_view(request: &MediaRequest, detail: Option<&TitleDetail>) -> RowView {
    let title = detail.map(|d| &d.title);
    let kind = match request.kind {
        MediaKind::Movie => msg::browse_kind_movie(),
        MediaKind::Tv => msg::browse_kind_tv_show(),
    };
    let year = title.and_then(|t| t.year).filter(|y| *y > 0).map(|y| y.to_string()).unwrap_or_default();
    let seasons = seasons(request).unwrap_or_default();
    let progress = progress(&request.downloads).map(|p| p.fraction);
    let pending = request.status == RequestStatus::PendingApproval;
    let moving = matches!(request.status, RequestStatus::Approved | RequestStatus::Other)
        && !matches!(request.availability, Availability::Available);
    RowView {
        id: request.id,
        kind: Some(request.kind),
        tmdb: request.tmdb,
        title: title.map(|t| t.title.clone()).unwrap_or_default(),
        lines: [
            dotted(&[kind, &year, &seasons]),
            asked_line(request),
            download_line(&request.downloads).unwrap_or_else(|| answered_line(request)),
        ],
        chip: request_chip(request),
        progress,
        poster: title.and_then(|t| t.poster.as_deref()).and_then(|p| plx_plex::plex::seerr::tmdb_image(p, "w342")),
        pending,
        moving,
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
