//! `Tile` — the library's item abstraction for a shelf tile (restructure spec §10): the title,
//! the poster (a server id + path, resolved through `ui::tex`), the resume progress and the
//! watch marks. The application implements it for `pms::PmsMovie`; the widgets that draw a tile
//! ask a `&dyn Tile` and never a Plex type, which is the boundary the layer gate (§2.1) will hold
//! once the screens migrate. Phase 3a: `widgets::poster_mark` reads through it.

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
