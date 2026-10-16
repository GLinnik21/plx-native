//! The catalog store (plan §2.4 W6): owns `GET discover /library/metadata/<hex>` for the catalog
//! page, captures [`WatchlistAuth`](crate::watchlist::WatchlistAuth) when it opens, and reads
//! membership from `WatchlistStore` (never from its own fetch). CONTRACT-FREEZE STAGE: the
//! notice plumbing and the state holder only; the fetch worker lands with the store package.

use crate::catalog::Load;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub struct CatalogStore {
    /// The title the page is about, set by `open`.
    pub hex: Option<[u8; 12]>,
    pub load: Load,
    notice_gen: AtomicU32,
    notice_dirty: AtomicBool,
}

impl Default for CatalogStore {
    fn default() -> Self {
        Self { hex: None, load: Load::Loading, notice_gen: AtomicU32::new(0), notice_dirty: AtomicBool::new(false) }
    }
}

impl CatalogStore {
    pub fn gen(&self) -> u32 {
        self.notice_gen.load(Ordering::Relaxed)
    }

    pub fn take_notice(&self) -> Option<u32> {
        self.notice_dirty.swap(false, Ordering::Relaxed).then(|| self.gen())
    }

    /// Raise a notice (a landing changed `load`).
    pub fn bump(&self) {
        self.notice_dirty.store(true, Ordering::Relaxed);
        self.notice_gen.fetch_add(1, Ordering::Relaxed);
    }
}
