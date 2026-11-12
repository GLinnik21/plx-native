//! Catalog worker transport; the store never calls these methods while stepping.
use crate::catalog::CatalogIo;
use crate::watchlist::WatchlistAuth;
use plx_plex::plex::account::AccountClient;
use plx_plex::plex::watchlist::{CatalogItem, WatchlistError};
pub struct LiveCatalogIo;
impl CatalogIo for LiveCatalogIo {
    fn fetch(&self, auth: &WatchlistAuth, hex: &str) -> Result<CatalogItem, WatchlistError> {
        let client = AccountClient::new(&auth.client_id, Some(&auth.credential));
        let mut transport = client.watchlist_transport();
        client.catalog_item(hex, &mut transport)
    }
    fn today(&self) -> Option<String> {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some(crate::watchlist::ymd_of_unix_days((secs / 86_400) as i64))
    }
}
