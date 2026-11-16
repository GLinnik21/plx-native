//! OUR composite of a deck card: the server's `background` with its `clearLogo` baked in, rendered
//! ONCE into one bitmap and drawn as a plain image (the owner's rule: a logo is never a live widget
//! over art). The poster worker runs [`bake`] on a disk miss for a `/plx/card/…` key
//! ([`plx_plex::plex::artwork::card_parts`]), through a seam ([`CardIo`]) so the orchestration is
//! host-testable without a server or a disk, exactly as the fan's ([`super::fan::FanIo`]). The
//! compositor itself is pure and lives in `ui` ([`plx_ui::card_art::compose`]).
//!
//! **Sources.** Both come through the ordinary art path (`super::load_art`): the background at the
//! card box and the logo at the hero's own request box, so a source already on disk for a still or
//! a hero is a disk hit. Both are required: a background or a logo that settles as a miss makes the
//! whole card a miss ([`Got::Final`]) and the card draws its next rung.
//!
//! **Stale sources are refetched, not trusted.** `load_art` serves a stale disk entry as a hit (and
//! leaves the refresh to the refresh lane). A composite is built from two such sources and then
//! persisted for a day or more, so a bake that reads a stale source first fetches it from the
//! server through `super::refetch_art` (which also writes the fresh bytes back under the source's
//! own transcode key). If that refetch fails the composite is composed from the stale bytes and
//! delivered, but NOT persisted: the next demand bakes again rather than freezing a degraded card.
//!
//! **Persisted** as an opaque PNG under a baked identity that names the server and both source
//! paths, so a new logo or background is a new key. Deliberately no profile in the
//! namespace: a background and a clearLogo do not depend on whose restrictions apply, unlike the
//! members a collection lists. [`CARD_KIND`] is versioned: change it whenever the composite's
//! output changes (the pin test grades the constants that move pixels).

use super::baked::{Bake, Cached};
use super::fan::{Got, Rgba};
use plx_ui::card_art::{compose, CardSpec};

/// The baked-image kind of a persisted composite, versioned: change it whenever [`compose`]'s
/// output changes (its sizing tokens, the spec, the filtering), so every persisted card re-bakes
/// instead of showing the old look until its art path moves.
pub(super) const CARD_KIND: &str = "card.1";

/// Which of the composite's two sources.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Part {
    Background,
    Logo,
}

/// A decoded source, and whether the disk entry it came from is stale.
pub(super) struct Source {
    pub img: Rgba,
    pub stale: bool,
}

/// The bake's I/O, a seam so the orchestration is host-testable.
pub(super) trait CardIo {
    /// The persisted composite under its baked identity, if any.
    fn cached(&mut self) -> Option<Cached>;
    /// The persisted entry did not decode (or is not the spec's size); drop it.
    fn discard(&mut self);
    /// One source through the disk-first art path.
    fn load(&mut self, part: Part) -> Got<Source>;
    /// One source from the server, ignoring the disk; `None` when it could not be fetched and
    /// decoded. The worker writes the fresh bytes under the source's own key (still-current only).
    fn refetch(&mut self, part: Part) -> Option<Rgba>;
    /// Persist the composite's PNG (the worker files it only while the bake is still current).
    fn persist(&mut self, png: &[u8]);
}

fn encode(out: &Rgba) -> Option<Vec<u8>> {
    plx_gfx::img::img_encode_png(out.w, out.h, &out.px)
}

/// Disk first; otherwise load the background and the logo, refetch whichever the disk served
/// stale, compose, persist (only a bake no refetch failure degraded) and hand the pixels back.
/// `Ok` is one opaque `spec.w` x `spec.h` image, delivered as an ordinary decoded poster.
pub(super) fn bake(io: &mut dyn CardIo, spec: &CardSpec) -> Bake {
    if let Some(c) = io.cached() {
        match plx_gfx::img::img_decode_owned(&c.bytes) {
            Some((w, h, px)) if (w, h) == (spec.w, spec.h) => {
                return Bake { got: Got::Ok(Rgba { w, h, px }), stale: c.stale, lasting: true };
            }
            _ => io.discard(),
        }
    }
    let mut degraded = false;
    let mut part = |io: &mut dyn CardIo, part: Part| -> Got<Rgba> {
        match io.load(part) {
            Got::Ok(src) if !src.stale => Got::Ok(src.img),
            Got::Ok(src) => match io.refetch(part) {
                Some(fresh) => Got::Ok(fresh),
                None => {
                    degraded = true;
                    Got::Ok(src.img)
                }
            },
            Got::Final => Got::Final,
            Got::Transient => Got::Transient,
        }
    };
    let bg = part(io, Part::Background);
    let bg = match bg {
        Got::Ok(img) => img,
        Got::Final => return Bake::fresh(Got::Final),
        Got::Transient => return Bake::fresh(Got::Transient),
    };
    let logo = match part(io, Part::Logo) {
        Got::Ok(img) => img,
        Got::Final => return Bake::fresh(Got::Final),
        Got::Transient => return Bake::fresh(Got::Transient),
    };
    let out = compose(&bg, Some(&logo), spec);
    if !degraded {
        if let Some(png) = encode(&out) {
            io.persist(&png);
        }
    }
    Bake::fresh(Got::Ok(out))
}

/// The refresh lane's re-bake: refetch BOTH sources from the server and, only when both came back,
/// compose and persist. The cached logo is never read, so legacy unversioned paths are
/// revalidated as well. A server that cannot be reached writes
/// nothing, and the old composite stands.
pub(super) fn rebake(io: &mut dyn CardIo, spec: &CardSpec) {
    let Some(bg) = io.refetch(Part::Background) else { return };
    let Some(logo) = io.refetch(Part::Logo) else { return };
    if let Some(png) = encode(&compose(&bg, Some(&logo), spec)) {
        io.persist(&png);
    }
}

#[cfg(test)]
mod tests;
