//! `Tile` — the library's item abstraction for a shelf tile (restructure spec §10): the title,
//! the poster (a server id + path, resolved through `ui::tex`), the resume progress and the
//! watch marks — and [`TileFacts`], the plain value the application fills from its catalog row.
//! The widgets that draw a tile ask a `&dyn Tile` or read a [`TileFacts`] and never a Plex type,
//! which is the boundary the layer gate (§2.1, `docs/module-layers.md`) holds: `ui` names no
//! application type, so a caller above it (a screen) converts its row INTO these facts
//! (`screens::registry::tile_facts::of` is the one converter for a `pms::PmsMovie`) rather than
//! the library reaching into the row. [`TileFacts`] is itself a [`Tile`]. Phase 3a:
//! `widgets::poster_mark` reads through [`Tile`].

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

/// What an item IS, as far as a tile's caption and art care. The library draws a season, an
/// episode and a collection differently from everything else, so those three are named; a movie
/// and a show are named for completeness, and a kind the application sends that the library has no
/// rule for is [`TileKind::Other`] (it reads as none of the three).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TileKind {
    #[default]
    Movie,
    Show,
    Season,
    Episode,
    Collection,
    Other,
}

/// How far into an item the viewer is: the resume bar's fraction and the time still to play.
/// Present exactly when the application's own resume rule says the item is IN PROGRESS, so the
/// bar, the watched mark and the Continue Watching caption all read one answer.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Resume {
    /// Played fraction, 0..1 — the resume bar's width.
    pub frac: f32,
    /// Milliseconds left to play — the Continue Watching caption's "8 min left".
    pub left_ms: i64,
}

/// Everything a poster or a still tile reads of one catalog row, as plain values borrowed from it.
///
/// The application fills this (a screen, from its `PmsMovie`); the widgets take it where they used
/// to take the row, which is what keeps `ui` free of the Plex types. Every field is the row's own
/// fact copied across, never a decision: the resume rule, the composite-thumb test and the kind
/// mapping stay with the layer that owns them, and arrive here already answered.
#[derive(Clone, Copy, Debug, Default)]
pub struct TileFacts<'a> {
    /// The identity of the row these facts were read from (its address, in practice). Opaque to
    /// the library: two tiles drawn from one row share it, which is what `card_motion` keys a
    /// card's placement history on.
    pub owner: usize,
    /// The raw id of the server `thumb`, `still` and `art` are paths on (`ui::tex`'s `srv`). A
    /// path is only meaningful on the server that issued it, so it travels WITH the facts.
    pub src: u16,
    pub kind: TileKind,
    pub title: &'a str,
    /// An episode's show / a season's show; empty on everything else.
    pub show_title: &'a str,
    /// The portrait poster's path.
    pub thumb: &'a str,
    /// The item's OWN landscape art where `thumb` holds a substitute (an episode's 16:9 still);
    /// empty on everything else.
    pub still: &'a str,
    /// The backdrop's path — the last resort of a landscape tile's fallback chain.
    pub art: &'a str,
    pub year: i32,
    /// A season's number; an episode's season number.
    pub season_index: i32,
    /// An episode's number within its season.
    pub ep_index: i32,
    /// A collection's member count; 0 on every other kind.
    pub child_count: i64,
    /// `Some` while the item is in progress — see [`Resume`].
    pub resume: Option<Resume>,
    /// Finished — the corner tick's fact (see [`Tile::watched`]).
    pub watched: bool,
    /// Never started at all (see [`Tile::unwatched`]).
    pub unwatched: bool,
    /// `thumb` is the server's generated 2×2 composite of a collection, which the application
    /// bakes into a fan of its members; the collection's name is then set over it, live.
    pub composite_thumb: bool,
    /// The four-corner ambient colours the item's artwork lends the page behind it, when it has
    /// any.
    pub blur: Option<[[f32; 3]; 4]>,
}

impl Tile for TileFacts<'_> {
    fn title(&self) -> &str {
        self.title
    }
    fn poster(&self) -> Option<(u16, &str)> {
        (!self.thumb.is_empty()).then_some((self.src, self.thumb))
    }
    fn progress(&self) -> Option<f32> {
        self.resume.map(|r| r.frac)
    }
    fn watched(&self) -> bool {
        self.watched
    }
    fn unwatched(&self) -> bool {
        self.unwatched
    }
}
