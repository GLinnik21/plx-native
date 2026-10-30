//! The cards of a row of Seerr titles (docs/seerr.md): Search's Request row and the Seerr tab's
//! Discover shelves draw the same card, so it is written once here, where a screen may name it.
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
