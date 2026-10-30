//! The cards of a row of Seerr titles (docs/seerr.md): Search's Request row and the Seerr tab's
//! Discover shelves draw the same card, so it is written once here, where a screen may name it.
//!
//! So are the words a request is described by, which the Seerr tab's request rows and a Seerr
//! title page both say: its state ([`request_chip`]), who asked and when ([`asked_line`]), who
//! answered it ([`answered_line`]), its download ([`download_line`]) and a season's state
//! ([`season_state`]).
use plx_ui::cards::{CardSource, TileLabel};
use plx_ui::widgets::Art;

/// One card of the Request row: a title the household's Seerr server found for the query and the
/// household's servers do not hold yet (docs/seerr.md). Owned by the page, built from the store's
/// answer by the page that shows it (Search's Request row, the Seerr tab's Discover shelves).
#[derive(Clone, Debug, PartialEq)]
pub struct RequestCard {
    pub kind: plx_data::seerr::MediaKind,
    pub tmdb: i64,
    pub title: String,
    pub status: plx_data::seerr::Availability,
    /// The TMDB poster's URL (`plex::seerr::tmdb_image`), drawn through a household server's proxy.
    pub poster: Option<String>,
}

/// What a Request card says under its title: what has become of a request for it.
pub fn request_caption(status: plx_data::seerr::Availability) -> &'static str {
    use plx_data::seerr::Availability;
    use plx_platform::i18n::msg;
    match status {
        Availability::Pending => msg::seerr_status_pending(),
        Availability::Processing => msg::seerr_status_processing(),
        Availability::PartlyAvailable => msg::seerr_status_partly(),
        Availability::Available => msg::seerr_status_available(),
        Availability::Blocked => msg::seerr_status_blocked(),
        Availability::Unknown => "",
    }
}

/// The Request row's cards.
pub struct RequestCards<'a> {
    pub elems: &'a [u32],
    pub cards: &'a [RequestCard],
    /// A household server whose photo proxy fetches the TMDB posters; `None` draws blank faces.
    pub art_server: Option<u16>,
    /// The server has further pages of this row: walking near its end asks for the next one
    /// (`CardEvent::Want`).
    pub more: bool,
    /// How many of the row's pages have landed, so the shelf knows an ask was answered even when
    /// the page brought no card it shows (`CardSource::page_epoch`).
    pub epoch: u32,
}

impl<H: plx_machine::machine::Host<Elem = u32>> CardSource<H> for RequestCards<'_> {
    fn len(&self) -> usize { self.elems.len().min(self.cards.len()) }
    fn elem(&self, i: usize) -> u32 { self.elems[i] }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.elems.iter().position(|elem| elem == e).filter(|&i| i < self.cards.len())
    }
    fn art(&self, i: usize) -> Art<'_> {
        match (self.cards.get(i).and_then(|card| card.poster.as_deref()), self.art_server) {
            (Some(url), Some(sid)) => Art::Thumb { sid, key: url, res: (250, 375) },
            _ => Art::Poster(None),
        }
    }
    fn more(&self) -> bool { self.more }
    fn page_epoch(&self) -> u32 { self.epoch }
    fn label(&self, i: usize) -> TileLabel {
        self.cards.get(i).map_or_else(TileLabel::default, |card| TileLabel::titled(&card.title, request_caption(card.status)))
    }
}

impl RequestCard {
    /// The card of `title`, or `None` for a row the server sent without a kind.
    pub fn of(title: &plx_data::seerr::Title) -> Option<Self> {
        Some(Self {
            kind: title.kind?,
            tmdb: title.tmdb,
            title: title.title.clone(),
            status: title.status,
            poster: title.poster.as_deref().and_then(|path| plx_plex::plex::seerr::tmdb_image(path, "w342")),
        })
    }
}

use plx_data::seerr::{progress, Availability, Download, DownloadState, MediaRequest, RequestStatus};
use plx_platform::i18n::msg;
use plx_ui::fmt;

/// A request's state in one word: one that waits, was declined or failed says so; one that went
/// through says where its title is (downloading, on its way, partly or wholly on the server).
pub fn request_chip(request: &MediaRequest) -> &'static str {
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

/// One season's state in one word, a season nobody asked for being "Missing".
pub fn season_state(status: Availability) -> &'static str {
    match status {
        Availability::Unknown => msg::seerr_chip_missing(),
        Availability::Pending => msg::seerr_chip_pending(),
        Availability::Processing => msg::seerr_chip_processing(),
        Availability::PartlyAvailable => msg::seerr_chip_partly(),
        Availability::Available => msg::seerr_chip_available(),
        Availability::Blocked => msg::seerr_chip_blocked(),
    }
}

/// Parts joined by the app's middle dot, empty ones left out.
pub fn dotted(parts: &[&str]) -> String {
    parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" · ")
}

/// An ISO 8601 stamp as the UI language's short date, or "" when it is not one.
pub fn date(stamp: Option<&str>) -> String {
    stamp.and_then(|s| s.get(..10)).map_or_else(String::new, |day| fmt::pretty_date(day, 0))
}

/// "Requested by Alex · 10/28/2026": who asked and when, each left out when the server did not say.
pub fn asked_line(request: &MediaRequest) -> String {
    let asked = request.requested_by.as_deref().map(msg::seerr_row_requested_by).unwrap_or_default();
    dotted(&[&asked, &date(request.created.as_deref())])
}

/// "Approved by …" / "Declined by …", or "" while nobody has answered (or the server did not say who).
pub fn answered_line(request: &MediaRequest) -> String {
    match (request.status, request.modified_by.as_deref()) {
        (RequestStatus::Approved, Some(name)) => msg::seerr_row_approved_by(name),
        (RequestStatus::Declined, Some(name)) => msg::seerr_row_declined_by(name),
        _ => String::new(),
    }
}

/// The download line, "68% · 1.2 GB of 4.0 GB · 12m left", while anything is queued or moving; the
/// time left only while something moves.
pub fn download_line(downloads: &[Download]) -> Option<String> {
    let p = progress(downloads)?;
    let of = msg::seerr_row_of(&fmt::bytes(p.size - p.size_left, (1, 0, 0)), &fmt::bytes(p.size, (1, 0, 0)));
    let pct = fmt::percent((f64::from(p.fraction) * 100.0).floor() as i64);
    let left = p.time_left_ms.filter(|_| p.downloading).map(fmt::time_left).unwrap_or_default();
    Some(dotted(&[&pct, &of, &left]))
}
