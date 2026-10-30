//! What one request row says (docs/seerr.md), composed from the request and its title's detail:
//! pure, so every wording is tested without a page.
//!
//! - the title, once its detail has landed (the request list names no title);
//! - line 1: the kind, the year and, for a show, the seasons asked for;
//! - line 2: who asked, and when;
//! - line 3: the download while one moves ("68% · 1.2 GB of 4.0 GB · 12m left"), else who answered
//!   it ("Approved by …", "Declined by …");
//! - the chip: what has become of it, the request's own state folded with the title's
//!   ([`chip`]);
//! - the band: the download's progress.
use plx_data::seerr::{progress, Availability, DownloadState, MediaKind, MediaRequest, RequestStatus, TitleDetail};
use plx_platform::i18n::msg;
use plx_ui::fmt;

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

/// The chip: a request that waits, was declined or failed says so; one that went through says
/// where its title is (downloading, on its way, partly or wholly on the server).
pub fn chip(request: &MediaRequest) -> &'static str {
    match request.status {
        RequestStatus::PendingApproval => msg::seerr_chip_pending(),
        RequestStatus::Declined => msg::seerr_chip_declined(),
        RequestStatus::Failed => msg::seerr_chip_failed(),
        RequestStatus::Approved | RequestStatus::Completed | RequestStatus::Other => match request.availability {
            Availability::Available => msg::seerr_chip_available(),
            Availability::PartlyAvailable => msg::seerr_chip_partly(),
            _ if request.downloads.iter().any(|d| d.state == DownloadState::Downloading) => msg::seerr_chip_downloading(),
            Availability::Processing => msg::seerr_chip_processing(),
            _ if request.status == RequestStatus::Completed => msg::seerr_chip_available(),
            _ => msg::seerr_chip_approved(),
        },
    }
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

/// An ISO 8601 stamp as the UI language's short date, or "" when it is not one.
fn date(stamp: Option<&str>) -> String {
    stamp.and_then(|s| s.get(..10)).map_or_else(String::new, |day| fmt::pretty_date(day, 0))
}

/// Parts joined by the app's middle dot, empty ones left out.
fn dotted(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

/// The download line: done, of what, and how long, while anything is queued or moving.
pub fn download_line(request: &MediaRequest) -> Option<String> {
    let p = progress(&request.downloads)?;
    let done = fmt::bytes(p.size - p.size_left, (1, 0, 0));
    let of = msg::seerr_row_of(&done, &fmt::bytes(p.size, (1, 0, 0)));
    let pct = fmt::percent((f64::from(p.fraction) * 100.0).floor() as i64);
    let left = p.time_left_ms.filter(|_| p.downloading).map(fmt::time_left).unwrap_or_default();
    Some(dotted(&[&pct, &of, &left]))
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
    let asked = match request.requested_by.as_deref() {
        Some(name) => msg::seerr_row_requested_by(name),
        None => String::new(),
    };
    let answered = match (request.status, request.modified_by.as_deref()) {
        (RequestStatus::Approved, Some(name)) => msg::seerr_row_approved_by(name),
        (RequestStatus::Declined, Some(name)) => msg::seerr_row_declined_by(name),
        _ => String::new(),
    };
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
            dotted(&[&asked, &date(request.created.as_deref())]),
            download_line(request).unwrap_or(answered),
        ],
        chip: chip(request),
        progress,
        poster: title.and_then(|t| t.poster.as_deref()).and_then(|p| plx_plex::plex::seerr::tmdb_image(p, "w342")),
        pending,
        moving,
    }
}

#[cfg(test)]
#[path = "rows_tests.rs"]
mod tests;
