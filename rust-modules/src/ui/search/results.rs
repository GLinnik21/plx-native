//! The typed result shelves.
//!
//! **STUB** — signatures final, bodies to come.
//!
//! ## The design
//!
//! Shelves in a **fixed** order — `crate::search::KINDS` — and an empty type draws nothing at all.
//! Ranking is honoured inside a shelf and never across them: the server reorders its hubs per
//! query (measured), and reordering rows per keystroke moves the row under the user's focus while
//! they are still typing.
//!
//! Tile geometry is per type: posters 250×375, episode stills 420×236, headshots a 250 circle.
//! Collections are posters. An item with no artwork mounts no slot — the tile's own skeleton face
//! is the resting state for art the server has not given us, which for a collection is always
//! (`Directory` entries carry no thumb).
//!
//! The heading is HEADLINE bold, the count BODY/tertiary beside it. Every search shelf merges
//! across sources, so the heading **cannot claim an owner** — the owner annotation follows FOCUS,
//! exactly as Continue Watching's does on Home, and rides on the focused tile's own caption.
//!
//! One caption on screen at a time, as every other shelf in this app does it: the label belongs to
//! the focused tile and nothing else carries type under its artwork. The band is reserved either
//! way ([`super::LABEL_BLOCK`]) so nothing reflows as focus travels.
//!
//! ## What to build it on
//!
//! `card_row::strip()` — the one-row loop — with `TileLabel::height()` reserving the label band,
//! the way `ui/person.rs`'s `draw_shelf` already stacks several shelves over one scroll flow.
//! Reach for it before writing another per-screen tile loop.
#![allow(dead_code)]

use crate::ui::search::View;
use crate::ui::Painter;

/// Tile size for a shelf of this kind: `(w, h, circular)`.
pub(crate) fn tile_size(kind: crate::search::Kind) -> (f32, f32, bool) {
    match kind {
        crate::search::Kind::Episode => (420.0, 236.0, false),
        crate::search::Kind::Person => (250.0, 250.0, true),
        _ => (crate::ui::consts::CARD_W, crate::ui::consts::CARD_H, false),
    }
}

pub(crate) fn draw(_p: Painter, _v: &View) {}
