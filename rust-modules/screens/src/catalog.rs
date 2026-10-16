//! The catalog page (plan §2.4 W6): the page for a title no server holds, opened by
//! `ContentArg::Catalog { hex }`. CONTRACT-FREEZE STAGE: only the page's view-model lives here;
//! `CatalogScreen` lands in the catalog-screen package. The view-model is the screen's OWN type
//! (the store-side `plx_data::catalog::{Load, Membership}` maps onto it in the app's mount
//! glue), so the screen is drawn and tested without a store.

use plx_data::watchlist::model::Kind;

/// Is the title on the Watchlist? `Unknown` wears the Add face (the PUT is idempotent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Membership {
    On,
    Off,
    #[default]
    Unknown,
}

/// The status line's meaning, which picks its icon (clock / server-slash / none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Status {
    #[default]
    None,
    ComingSoon,
    NotOnAnyServer,
}

/// Where the page's record stands.
#[derive(Clone, Debug, PartialEq)]
pub enum Load {
    Loading,
    Loaded {
        title: String,
        year: Option<i32>,
        kind: Kind,
        summary: String,
        genres: Vec<String>,
        /// Absolute discover art URL; empty = a neutral ground.
        art_url: String,
        /// `YYYY-MM-DD`.
        release_date: Option<String>,
        status: Status,
    },
    NotFound,
    Unreachable,
}

/// Everything the catalog page draws and decides from, in one value.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogViewModel {
    pub load: Load,
    pub membership: Membership,
}

impl Default for CatalogViewModel {
    fn default() -> Self {
        Self { load: Load::Loading, membership: Membership::Unknown }
    }
}

impl From<plx_data::catalog::Membership> for Membership {
    fn from(m: plx_data::catalog::Membership) -> Self {
        match m {
            plx_data::catalog::Membership::On => Self::On,
            plx_data::catalog::Membership::Off => Self::Off,
            plx_data::catalog::Membership::Unknown => Self::Unknown,
        }
    }
}

impl From<plx_data::catalog::CatalogStatus> for Status {
    fn from(s: plx_data::catalog::CatalogStatus) -> Self {
        match s {
            plx_data::catalog::CatalogStatus::None => Self::None,
            plx_data::catalog::CatalogStatus::ComingSoon => Self::ComingSoon,
            plx_data::catalog::CatalogStatus::NotOnAnyServer => Self::NotOnAnyServer,
        }
    }
}

impl From<&plx_data::catalog::Load> for Load {
    fn from(l: &plx_data::catalog::Load) -> Self {
        use plx_data::catalog::Load as S;
        match l {
            S::Loading => Self::Loading,
            S::NotFound => Self::NotFound,
            S::Unreachable => Self::Unreachable,
            S::Loaded(r) => Self::Loaded {
                title: r.title.clone(),
                year: r.year,
                kind: r.kind,
                summary: r.summary.clone(),
                genres: r.genres.clone(),
                art_url: r.art_url.clone(),
                release_date: r.release_date.clone(),
                status: r.status.into(),
            },
        }
    }
}
