//! `Tile` — the library's item abstraction for a shelf tile (restructure spec §10): the title,
//! the poster (a server id + path, resolved through `ui::tex`), the resume progress and the
//! watch marks. The application implements it for `pms::PmsMovie`; the widgets that draw a tile
//! ask a `&dyn Tile` and never a Plex type, which is the boundary the layer gate (§2.1) will hold
//! once the screens migrate. Phase 3a: `widgets::poster_mark` reads through it.
//!
//! The trait itself is `crate::tile::Tile`, defined in `base` so that the data layer can implement
//! it without naming `ui` (and `ui` without naming the data layer); this is its library spelling.

pub(crate) use crate::tile::Tile;
