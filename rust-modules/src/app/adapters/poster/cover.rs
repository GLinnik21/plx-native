//! Plex's own COVER of a deck card: the 16:9 `coverArt` of a catalog title (its name baked into the
//! picture), which the PMS does not serve, so it is read from the plex.tv Discover provider and then
//! rendered by the user's own PMS photo transcoder at the card's box.
//!
//! The poster worker runs [`bake`] on a disk miss for a `/plx/cover/{hex}` key
//! ([`plx_plex::plex::artwork::cover_parts`]), through a seam ([`CoverIo`]) so the orchestration is
//! host-testable without a network, a server or a disk, exactly as the fan's ([`super::fan::FanIo`]).
//! It performs none itself.
//!
//! **Where the bytes live.** The transcoder's answer is persisted AS IS (no re-encode) under the
//! cover's own baked identity ([`plx_platform::imgcache::classify_baked`]; the namespace carries
//! the language, because a localized title has a different `coverArt`), and NOT also under a
//! URL-keyed transcode entry: it is written once. A warm disk hit decodes those bytes and reads
//! nothing, from plex.tv or anywhere.
//!
//! **A definitive "no cover"** (a 200 without `coverArt`, a `NotFound`, or the transcoder's own 4xx)
//! is persisted as a short fixed marker under a sibling identity, read before dialling, so a boot
//! does not re-read Discover for a title that has none. Everything else that fails persists
//! NOTHING and is answered [`Got::Final`]: the poster store records it in the miss set
//! (`super::miss`), because a parked retry would read as "pending" and leave the card on a skeleton
//! while the next rung waits. How long the miss counts follows its cause ([`Bake::lasting`]): one
//! that came from a read that did not answer (no identity, an unreachable Discover, a refused or
//! unauthorised read, a 5xx, no response: [`Bake::unanswered`]) says nothing about the title and
//! is forgotten once the Discover memo's window (45 s) has passed, so the next time the card is
//! shown it asks again and moves up to its cover; one that is a verdict on the bytes (a picture that
//! does not decode, or is smaller than the box) holds for the session.
//!
//! **Size.** The request is `minSize=1`, so the transcoder returns an image at least the box in
//! both axes (COVER, not fit). A cached or fetched cover SMALLER than the box in either axis is
//! unusable: a cached one is discarded and re-fetched, a fetched one is a miss. One larger than the
//! box is accepted and cover-cropped at draw like any still.

use super::baked::{Bake, Cached};
use super::fan::{Got, Rgba};
use plx_plex::plex::watchlist::{CatalogItem, WatchlistError};
use plx_plex::plex::ArtFetch;

/// The baked-image kind of a persisted cover, versioned: change it whenever what is stored under
/// it changes meaning.
pub(super) const COVER_KIND: &str = "cover.1";
/// The kind of the sibling "this title has no cover" marker.
pub(super) const COVER_NONE_KIND: &str = "cover-none.1";
/// The marker's body: fixed and NON-EMPTY, because the disk tier refuses an empty body
/// (`imgcache::write_locked`).
pub(super) const NO_COVER_MARKER: &[u8] = b"plx-no-cover-1";
/// The baked identity's `stamp` field: a cover has none (the catalog id is its whole identity).
pub(super) const COVER_STAMP: &str = "-";

/// The identity namespace of a cover and of its marker. No server and no profile: the cover is the
/// catalog's, one for every server that holds the title. `language` is what the Discover request
/// itself carries (`X-Plex-Language`), so the namespace names exactly what was asked.
pub(super) fn namespace(language: &str) -> String {
    format!("plextv|lang:{language}")
}

/// What a Discover answer means for the cover (the plan's failure table, one row each).
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The title has a cover: render this absolute URL through the PMS transcoder.
    Fetch(String),
    /// A definitive "no cover": a 200 with no `coverArt`, or `NotFound`. Persist the marker.
    NoCover,
    /// Anything that can change or is not about this title (refused by the dev gate, unauthorised,
    /// no network, a 5xx, a body that did not read, a malformed id): a miss for this session,
    /// nothing persisted.
    Skip,
}

pub(super) fn verdict(read: Result<CatalogItem, WatchlistError>) -> Verdict {
    match read {
        Ok(item) => item.cover_art.map_or(Verdict::NoCover, Verdict::Fetch),
        Err(WatchlistError::NotFound) => Verdict::NoCover,
        Err(_) => Verdict::Skip,
    }
}

/// Is this transcoder status a definitive "the picture is not there" (persist the marker, which
/// `REFRESH_AFTER` revalidates), as opposed to one that can change (a refused grant, a timeout, a
/// rate limit, any 5xx)?
pub(super) fn marks_no_cover(status: i32) -> bool {
    (400..500).contains(&status) && !matches!(status, 401 | 403 | 408 | 429)
}

/// The bake's I/O, a seam so the orchestration is host-testable.
pub(super) trait CoverIo {
    /// The persisted cover under its baked identity, if any.
    fn cached(&mut self) -> Option<Cached>;
    /// The persisted entry did not do (it did not decode, or it is below the box); drop it.
    fn discard(&mut self);
    /// The "no cover" marker, when one is on disk: `Some(stale)`.
    fn marker(&mut self) -> Option<bool>;
    /// Is there an identity to read Discover as (a client identifier)? Without one nothing is sent.
    fn identified(&mut self) -> bool;
    /// Did Discover fail to answer moments ago (`plex::account::discover_recently_unreachable`)?
    fn discover_unreachable(&mut self) -> bool;
    /// `GET {discover}/library/metadata/{hex}`: the existing `catalog_item` read, with its dev gate,
    /// transport, reachability memo and error mapping.
    fn read(&mut self) -> Result<CatalogItem, WatchlistError>;
    /// Render `url` through the item's server's photo transcoder at the card box.
    fn fetch(&mut self, url: &str) -> ArtFetch;
    /// Persist the transcoder's bytes (the worker files them only while the bake is still current).
    fn persist(&mut self, bytes: &[u8]);
    /// Persist the "no cover" marker with `body` (same currency rule).
    fn persist_marker(&mut self, body: &[u8]);
}

/// A decoded image at least `min` in both axes.
fn usable(bytes: &[u8], min: (u32, u32)) -> Option<Rgba> {
    plx_gfx::img::img_decode_owned(bytes)
        .filter(|(w, h, _)| *w >= min.0 && *h >= min.1 && *w > 0 && *h > 0)
        .map(|(w, h, px)| Rgba { w, h, px })
}

/// Disk first (a cover that decodes at least the box, else the marker); otherwise read Discover,
/// render the cover through the transcoder, persist it and hand the pixels back. `min` is the card
/// box in output pixels. `Ok` is the decoded cover; `Final` is a miss this session (the table in
/// the module doc); this never answers `Transient`.
pub(super) fn bake(io: &mut dyn CoverIo, min: (u32, u32)) -> Bake {
    run(io, min, false)
}

/// The refresh lane's re-bake: read Discover and the image again whatever is on disk, and write
/// what comes back. A server that cannot be reached writes nothing, so the cached cover stands.
pub(super) fn rebake(io: &mut dyn CoverIo, min: (u32, u32)) {
    let _ = run(io, min, true);
}

fn run(io: &mut dyn CoverIo, min: (u32, u32), force: bool) -> Bake {
    if !force {
        if let Some(c) = io.cached() {
            match usable(&c.bytes, min) {
                Some(img) => return Bake { got: Got::Ok(img), stale: c.stale, lasting: true },
                None => io.discard(),
            }
        }
        if let Some(stale) = io.marker() {
            return Bake { got: Got::Final, stale, lasting: true };
        }
    }
    if !io.identified() || io.discover_unreachable() {
        return Bake::unanswered();
    }
    let url = match verdict(io.read()) {
        Verdict::Fetch(url) => url,
        Verdict::NoCover => {
            // A cover the title once had is gone: the cached one must not outlive the marker.
            if force {
                io.discard();
            }
            io.persist_marker(NO_COVER_MARKER);
            return Bake::fresh(Got::Final);
        }
        Verdict::Skip => return Bake::unanswered(),
    };
    match io.fetch(&url) {
        ArtFetch::Bytes(bytes) if !bytes.is_empty() => match usable(&bytes, min) {
            Some(img) => {
                io.persist(&bytes);
                Bake::fresh(Got::Ok(img))
            }
            None => Bake::fresh(Got::Final),
        },
        ArtFetch::Status(s) if marks_no_cover(s) => {
            io.persist_marker(NO_COVER_MARKER);
            Bake::fresh(Got::Final)
        }
        ArtFetch::Bytes(_) | ArtFetch::Status(_) | ArtFetch::NoResponse => Bake::unanswered(),
    }
}

#[cfg(test)]
mod tests;
