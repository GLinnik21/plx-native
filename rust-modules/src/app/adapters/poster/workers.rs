//! The poster worker's REAL I/O for the two app-baked card kinds: [`WorkerCoverIo`] (the
//! [`super::cover::CoverIo`] seam: the disk tier, the plex.tv Discover read, the PMS photo
//! transcoder) and [`WorkerCardIo`] (the [`super::card::CardIo`] seam: the disk tier and the ordinary
//! art path), plus the three entry points the worker and the refresh lane call. Everything with a
//! decision in it is in `cover.rs` / `card.rs`, where it is tested with fake I/O; what is here is the
//! thin edge that touches the world, run on a poster worker (or the refresh lane's) thread, never the
//! main thread.

use super::baked::{Bake, Cached};
use super::card::{self, CardIo, Part, Source};
use super::cover::{self, CoverIo};
use super::fan::{Got, Rgba};
use plx_platform::imgcache::DiskKey;
use plx_plex::plex::account::AccountClient;
use plx_plex::plex::artwork::{card_parts, cover_parts};
use plx_plex::plex::watchlist::{CatalogItem, WatchlistError};
use plx_plex::plex::{ArtFetch, Client};
use plx_ui::card_art::CardSpec;

/// The card box in output pixels, as the cover's size rule and the composite's spec read it.
pub(super) fn box_px() -> (u32, u32) {
    let n = plx_base::surface::render_scale().max(1) as u32;
    (plx_ui::widgets::STILL_RES.0 as u32 * n, plx_ui::widgets::STILL_RES.1 as u32 * n)
}

fn decode(bytes: &[u8]) -> Option<Rgba> {
    plx_gfx::img::img_decode_owned(bytes).map(|(w, h, px)| Rgba { w, h, px })
}

/// Is the bake that started under `(cache_gen, token_gen, profile)` still the one in force? A bake
/// that straddles a sign-out, a retoken or a profile switch is still delivered to its (now
/// unreachable) slot, but never filed on disk.
fn current(client: &Client, cache_gen: u64, token_gen: u32, profile: Option<&str>) -> bool {
    cache_gen == plx_platform::imgcache::generation()
        && client.token_gen() == token_gen
        && profile.is_none_or(|p| plx_plex::plex::session::current_profile_key() == p)
}

fn disk_or_none(key: Option<DiskKey>) -> Option<DiskKey> {
    if crate::dev::scenarios::imagecache_bypass_armed() { None } else { key }
}

// ── the cover ───────────────────────────────────────────────────────────────────────────────────

pub(super) struct WorkerCoverIo<'a> {
    pub(super) client: &'a Client,
    pub(super) hex: &'a str,
    pub(super) cache_gen: u64,
    pub(super) token_gen: u32,
    /// The profile the bake started under: the Discover read is made as it, so a bake that straddles
    /// a switch is not filed.
    pub(super) profile: String,
    pub(super) disk: Option<DiskKey>,
    pub(super) none_disk: Option<DiskKey>,
    /// The identity the read is made as. The client identifier is required; the account token is
    /// sent when the seated profile has one and omitted when it has none (the endpoint answers the
    /// same either way, so a profile without one still gets covers).
    pub(super) client_id: String,
    pub(super) credential: Option<String>,
}

impl<'a> WorkerCoverIo<'a> {
    pub(super) fn new(client: &'a Client, hex: &'a str, cache_gen: u64, token_gen: u32) -> Self {
        let language = plx_plex::plex::identity::language().unwrap_or("");
        let ns = cover::namespace(language);
        let (w, h) = box_px();
        let identity = |kind, (w, h)| {
            disk_or_none(plx_platform::imgcache::classify_baked(&ns, kind, hex, cover::COVER_STAMP, w, h))
        };
        let snapshot = plx_plex::plex::session::current_snapshot();
        WorkerCoverIo {
            client,
            hex,
            cache_gen,
            token_gen,
            profile: plx_plex::plex::session::current_profile_key(),
            disk: identity(cover::COVER_KIND, (w, h)),
            none_disk: identity(cover::COVER_NONE_KIND, (1, 1)),
            client_id: plx_plex::plex::session::peek().client_id.clone(),
            credential: snapshot.user.as_ref().and_then(plx_plex::plex::session::plex_tv_credential),
        }
    }

    pub(super) fn still_current(&self) -> bool {
        current(self.client, self.cache_gen, self.token_gen, Some(&self.profile))
    }
}

impl CoverIo for WorkerCoverIo<'_> {
    fn cached(&mut self) -> Option<Cached> {
        let c = plx_platform::imgcache::read_at(self.cache_gen, self.disk.as_ref()?)?;
        Some(Cached { bytes: c.bytes, stale: c.stale })
    }
    fn discard(&mut self) {
        if let Some(k) = &self.disk {
            plx_platform::imgcache::remove_at(self.cache_gen, k);
        }
    }
    fn marker(&mut self) -> Option<bool> {
        let k = self.none_disk.as_ref()?;
        let c = plx_platform::imgcache::read_at(self.cache_gen, k)?;
        if c.bytes == cover::NO_COVER_MARKER {
            return Some(c.stale);
        }
        plx_platform::imgcache::remove_at(self.cache_gen, k);
        None
    }
    fn identified(&mut self) -> bool {
        !self.client_id.is_empty()
    }
    fn discover_unreachable(&mut self) -> bool {
        plx_plex::plex::account::discover_recently_unreachable()
    }
    fn read(&mut self) -> Result<CatalogItem, WatchlistError> {
        let account = AccountClient::new(&self.client_id, self.credential.as_deref());
        let mut transport = account.watchlist_transport();
        account.catalog_item(self.hex, &mut transport)
    }
    fn fetch(&mut self, url: &str) -> ArtFetch {
        let (w, h) = plx_ui::widgets::STILL_RES;
        let path = super::transcode_request(self.client, url, w, h, false);
        super::FETCHES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.client.fetch_built_outcome(&path)
    }
    fn persist(&mut self, bytes: &[u8]) {
        if let (true, Some(k)) = (self.still_current(), &self.disk) {
            plx_platform::imgcache::write_at(self.cache_gen, k, bytes);
        }
    }
    fn persist_marker(&mut self, body: &[u8]) {
        if let (true, Some(k)) = (self.still_current(), &self.none_disk) {
            plx_platform::imgcache::write_at(self.cache_gen, k, body);
        }
    }
}

/// Bake (or reload) one catalog title's cover, and the baked identity it is filed under (the
/// refresh lane's dedup key).
pub(super) fn bake_cover(client: &Client, hex: &str, cache_gen: u64, token_gen: u32) -> (Bake, Option<DiskKey>) {
    let mut io = WorkerCoverIo::new(client, hex, cache_gen, token_gen);
    let bake = cover::bake(&mut io, box_px());
    (bake, io.disk)
}

// ── the composite ───────────────────────────────────────────────────────────────────────────────

pub(super) struct WorkerCardIo<'a> {
    pub(super) client: &'a Client,
    pub(super) srv: plx_plex::plex::ServerId,
    pub(super) logo: &'a str,
    pub(super) art: &'a str,
    pub(super) cache_gen: u64,
    pub(super) token_gen: u32,
    pub(super) disk: Option<DiskKey>,
}

impl<'a> WorkerCardIo<'a> {
    pub(super) fn new(client: &'a Client, logo: &'a str, art: &'a str, cache_gen: u64, token_gen: u32) -> Self {
        let (w, h) = box_px();
        let disk = disk_or_none(plx_platform::imgcache::classify_baked(
            &super::disk_namespace(client), card::CARD_KIND, logo, art, w, h,
        ));
        WorkerCardIo { client, srv: client.id(), logo, art, cache_gen, token_gen, disk }
    }

    /// The built request of one source: the background at the card box (the key a still of this art
    /// would use) or the clearLogo at the hero's request box (the key a hero logo would use), so a
    /// source already on disk for either is a hit.
    fn request(&self, part: Part) -> String {
        match part {
            Part::Background => {
                let (w, h) = plx_ui::widgets::STILL_RES;
                super::transcode_request(self.client, self.art, w, h, false)
            }
            Part::Logo => super::transcode_request(
                self.client,
                &plx_plex::plex::artwork::logo_path(self.logo).expect("validated by card_parts"),
                super::LOGO_REQ_W,
                super::LOGO_REQ_H,
                true,
            ),
        }
    }

    pub(super) fn still_current(&self) -> bool {
        current(self.client, self.cache_gen, self.token_gen, None)
    }
}

impl CardIo for WorkerCardIo<'_> {
    fn cached(&mut self) -> Option<Cached> {
        let c = plx_platform::imgcache::read_at(self.cache_gen, self.disk.as_ref()?)?;
        Some(Cached { bytes: c.bytes, stale: c.stale })
    }
    fn discard(&mut self) {
        if let Some(k) = &self.disk {
            plx_platform::imgcache::remove_at(self.cache_gen, k);
        }
    }
    fn load(&mut self, part: Part) -> Got<Source> {
        let path = self.request(part);
        let loaded = super::load_art(self.client, self.srv, &path, self.cache_gen, decode);
        match loaded.art {
            Some(img) => Got::Ok(Source { img, stale: loaded.stale.is_some() }),
            None if loaded.transient => Got::Transient,
            None => Got::Final,
        }
    }
    fn refetch(&mut self, part: Part) -> Option<Rgba> {
        let path = self.request(part);
        super::refetch_art(self.client, self.srv, &path, self.cache_gen, || self.still_current(), decode)
    }
    fn persist(&mut self, png: &[u8]) {
        if let (true, Some(k)) = (self.still_current(), &self.disk) {
            plx_platform::imgcache::write_at(self.cache_gen, k, png);
        }
    }
}

/// Bake (or reload) one composite, and the baked identity it is filed under.
pub(super) fn bake_card(client: &Client, logo: &str, art: &str, cache_gen: u64, token_gen: u32) -> (Bake, Option<DiskKey>) {
    let mut io = WorkerCardIo::new(client, logo, art, cache_gen, token_gen);
    let bake = card::bake(&mut io, &CardSpec::deck(plx_base::surface::render_scale()));
    (bake, io.disk)
}

// ── the refresh lane ────────────────────────────────────────────────────────────────────────────

/// The refresh lane's re-bake of a baked key. `false` when `key` is not one of the two baked kinds
/// (the caller then refreshes it as an ordinary transcode); `true` when it was handled, whatever
/// the outcome (a failed re-bake leaves the old entry standing).
pub(super) fn rebake(client: &Client, key: &str, cache_gen: u64, token_gen: u32) -> bool {
    if let Some(hex) = cover_parts(key) {
        let mut io = WorkerCoverIo::new(client, hex, cache_gen, token_gen);
        cover::rebake(&mut io, box_px());
    } else if let Some((logo, art)) = card_parts(key) {
        let mut io = WorkerCardIo::new(client, logo, art, cache_gen, token_gen);
        card::rebake(&mut io, &CardSpec::deck(plx_base::surface::render_scale()));
    } else {
        return false;
    }
    true
}
