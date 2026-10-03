//! `Tile` — the item abstraction a shelf tile is drawn from (restructure spec §10): the title, the
//! poster (a server id + path, resolved through `ui::tex`), the resume progress and the watch
//! marks. `pms::PmsMovie` implements it and the widgets that draw a tile (`ui::widgets::poster_mark`)
//! ask a `&dyn Tile`, never a Plex type.
//!
//! It is defined here, in `base`, and not in `ui`: the data layer may not name `ui` and `ui` may not
//! name the data layer, and an `impl Tile for PmsMovie` in any third crate would be an orphan. A
//! pure trait with no dependencies is what both sides can name; `ui::tile` re-exports it.
// `title` and `poster` have no reader yet (the widgets read the mark facts); under `ui/` the trait
// was covered by `ui/mod.rs`'s blanket `allow(dead_code)`, and moving it must not make it dead.
#![allow(dead_code)]

/// A shelf item as a tile sees it.
pub trait Tile {
    fn title(&self) -> &str;
    /// The poster's `(server raw id, path)`, if the item has art.
    fn poster(&self) -> Option<(u16, &str)>;
    /// How far in, 0..1 — `None` when never started or finished (the resume bar's fact).
    fn progress(&self) -> Option<f32>;
    /// Finished (the corner tick's fact).
    fn watched(&self) -> bool;
    /// Never started at all — `!unwatched && !watched` is a part-watched container.
    fn unwatched(&self) -> bool;
}
