//! The Sort & Filter value (plan §1.3, §1.10): what the pill summarises and the popover edits.
//! Filtering and ordering are client-side over the stored rows, so a change makes no request.
use super::page::WatchTile;
use plx_data::watchlist::model::{Kind, TileState};
pub use plx_data::watchlist::order::SortKey;
use plx_data::watchlist::order::{sorted_filtered, FilterSpec, Show, SortSpec};
use plx_platform::i18n::msg;
use plx_ui::popover::Popover;
use plx_ui::table::{Row, Section, TableView};
use plx_ui::widgets::Ground;
use plx_ui::{theme, Painter, Rect};

/// The Type section: `Movies & Shows` / `Movies` / `TV Shows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum TypeFilter {
    #[default]
    All,
    Movies,
    Shows,
}

/// The Show section: `All` / `Released` / `On your servers`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ShowFilter {
    #[default]
    All,
    Released,
    OnYourServers,
}

/// The whole Sort & Filter value. `Default` is the page's opening order: the service's own
/// (newest added first), every type, every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchlistOptionsState {
    pub type_: TypeFilter,
    pub sort: SortKey,
    pub descending: bool,
    pub show: ShowFilter,
}

/// A fingerprint of the value, for what is built from it (the header row's key).
impl std::hash::Hash for WatchlistOptionsState {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.type_.hash(state);
        std::mem::discriminant(&self.sort).hash(state);
        self.descending.hash(state);
        self.show.hash(state);
    }
}

impl Default for WatchlistOptionsState {
    fn default() -> Self {
        Self { type_: TypeFilter::All, sort: SortKey::DateAdded, descending: true, show: ShowFilter::All }
    }
}

impl WatchlistOptionsState {
    /// The data layer's spelling of this value.
    pub fn specs(&self) -> (SortSpec, FilterSpec) {
        let kind = match self.type_ {
            TypeFilter::All => None,
            TypeFilter::Movies => Some(Kind::Movie),
            TypeFilter::Shows => Some(Kind::Show),
        };
        let show = match self.show {
            ShowFilter::All => Show::All,
            ShowFilter::Released => Show::Released,
            ShowFilter::OnYourServers => Show::OnYourServers,
        };
        (SortSpec { key: self.sort, desc: self.descending }, FilterSpec { kind, show })
    }

    /// The canonical bytes of this value (the menu argument's and the menu screen's state hash).
    pub fn write_canon(&self, c: &mut plx_machine::machine::Canon) {
        c.u32(match self.type_ {
            TypeFilter::All => 0,
            TypeFilter::Movies => 1,
            TypeFilter::Shows => 2,
        })
        .u32(match self.sort {
            SortKey::Title => 0,
            SortKey::DateAdded => 1,
            SortKey::Release => 2,
            SortKey::Rating => 3,
        })
        .bool(self.descending)
        .u32(match self.show {
            ShowFilter::All => 0,
            ShowFilter::Released => 1,
            ShowFilter::OnYourServers => 2,
        });
    }

    /// The value the store's specs stand for: the inverse of [`specs`](Self::specs).
    pub fn from_specs(sort: SortSpec, filter: FilterSpec) -> Self {
        Self {
            type_: match filter.kind {
                None => TypeFilter::All,
                Some(Kind::Movie) => TypeFilter::Movies,
                Some(Kind::Show) => TypeFilter::Shows,
            },
            sort: sort.key,
            descending: sort.desc,
            show: match filter.show {
                Show::All => ShowFilter::All,
                Show::Released => ShowFilter::Released,
                Show::OnYourServers => ShowFilter::OnYourServers,
            },
        }
    }

    /// The sort control's criterion, the one text it draws: the current SORT, by its menu label
    /// ("Date added", "Title"), the same fact the Library's sort control names. A filter (Type,
    /// Show) never shows here; it shows as the header's "N of M" read-out. The direction is not
    /// spelled: the open menu shows it as the checked sort row's value word, and the control has
    /// to share the header line with the title and the count in the longest language.
    pub fn summary(&self) -> String {
        OptionsRow::Sort(self.sort).label().to_string()
    }

    /// `rows` filtered and ordered by this value, exactly as the data layer does it
    /// (`order::sorted_filtered`); "On your servers" reads each tile's committed state.
    pub fn apply(&self, rows: &[WatchTile]) -> Vec<WatchTile> {
        let (sort, filter) = self.specs();
        let data: Vec<_> = rows.iter().map(|t| t.row.clone()).collect();
        let playable = |i: usize| matches!(rows[i].state, TileState::Available | TileState::ComingSoonMatched);
        sorted_filtered(&data, &sort, &filter, &playable).into_iter().map(|i| rows[i].clone()).collect()
    }
}

/// A sort key's own first direction, the one choosing it from another key lands on: Title reads
/// A to Z, the others newest, highest and most recent first. Date added descending is the
/// service's own order and the page's default.
pub fn natural_desc(key: SortKey) -> bool {
    !matches!(key, SortKey::Title)
}

/// One selectable row of the Sort & Filter menu: its identity is its meaning, never its position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionsRow {
    Type(TypeFilter),
    Sort(SortKey),
    Show(ShowFilter),
}

impl OptionsRow {
    /// Every row in draw order: Show, Sort, Filter (boards WatchlistOptions: 3 + 4 + 3 rows).
    pub const ALL: [OptionsRow; 10] = [
        OptionsRow::Type(TypeFilter::All),
        OptionsRow::Type(TypeFilter::Movies),
        OptionsRow::Type(TypeFilter::Shows),
        OptionsRow::Sort(SortKey::DateAdded),
        OptionsRow::Sort(SortKey::Title),
        OptionsRow::Sort(SortKey::Release),
        OptionsRow::Sort(SortKey::Rating),
        OptionsRow::Show(ShowFilter::All),
        OptionsRow::Show(ShowFilter::Released),
        OptionsRow::Show(ShowFilter::OnYourServers),
    ];

    pub fn label(self) -> &'static str {
        match self {
            OptionsRow::Type(TypeFilter::All) => msg::watchlist_filter_type_all(),
            OptionsRow::Type(TypeFilter::Movies) => msg::watchlist_filter_type_movies(),
            OptionsRow::Type(TypeFilter::Shows) => msg::watchlist_filter_type_shows(),
            // The shared table's name for the fact, the one the Library's Sort section wears.
            OptionsRow::Sort(key) => key.fact().label(),
            OptionsRow::Show(ShowFilter::All) => msg::watchlist_filter_show_all(),
            OptionsRow::Show(ShowFilter::Released) => msg::watchlist_filter_show_released(),
            OptionsRow::Show(ShowFilter::OnYourServers) => msg::watchlist_filter_show_on_servers(),
        }
    }

    /// Whether `state` has this choice (the leading check).
    pub fn is_chosen(self, state: &WatchlistOptionsState) -> bool {
        match self {
            OptionsRow::Type(t) => state.type_ == t,
            OptionsRow::Sort(k) => state.sort == k,
            OptionsRow::Show(s) => state.show == s,
        }
    }

    /// Press this row. A different sort key lands on its [`natural_desc`]; pressing the key that is
    /// already chosen flips the direction, the Library sort menu's idiom (the direction word on the
    /// active row). Returns whether `state` changed (a Type or Show press on the chosen row does not).
    pub fn apply(self, state: &mut WatchlistOptionsState) -> bool {
        let before = *state;
        match self {
            OptionsRow::Type(t) => state.type_ = t,
            OptionsRow::Show(s) => state.show = s,
            OptionsRow::Sort(k) if state.sort == k => state.descending = !state.descending,
            OptionsRow::Sort(k) => {
                state.sort = k;
                state.descending = natural_desc(k);
            }
        }
        *state != before
    }
}

/// The menu's three sections for `state`, named as the Library's Sort & Filter panel names its own:
/// Show (the kind of title), Sort (the checked key carries the direction as a value word, "Newest
/// first", from the same table), Filter (availability). Pure, so the text-fit suite drives the
/// real builder.
pub fn sections(state: &WatchlistOptionsState) -> Vec<Section> {
    let mut type_ = Section::new(msg::library_header_show());
    let mut sort = Section::new(msg::browse_library_sort());
    let mut show = Section::new(msg::browse_library_filter());
    for id in OptionsRow::ALL {
        let mut row = Row::new(id.label()).checked(id.is_chosen(state));
        if let OptionsRow::Sort(key) = id {
            if let Some(axis) = key.fact().axis().filter(|_| id.is_chosen(state)) { row = row.value(axis.direction_word(state.descending)); }
        }
        match id {
            OptionsRow::Type(_) => type_ = type_.row(row),
            OptionsRow::Sort(_) => sort = sort.row(row),
            OptionsRow::Show(_) => show = show.row(row),
        }
    }
    vec![type_, sort, show]
}

/// The panel's corner radius: `--radius-panel`, as the shipped account menu pins it. The board
/// draws 26; shipped menus win and the morph ends on this one (`Popover::morph_frame`).
pub const PANEL_RAD: f32 = 24.0;

/// The Sort & Filter menu: a glass `Ground::Menu` panel that MORPHS out of the header pill (DS6,
/// DS7), its rows a [`TableView`]. A component the mounting surface owns, like the profile menu's
/// table: it holds the open/morph state, the row cursor and the geometry, and answers `draw`.
///
/// The panel grows out of the pill (`popover_morph`): [`draw`](Self::draw) lays the glass at the
/// morphing rect, then the rows. It draws NO pill: the pill is the control's own, kept lit above the
/// panel by the host's opener lift (the host surface names it as its `opener`, the page leaves it
/// out of its pass, `WatchlistScreen::redraw_focused` paints it) like every header opener, so the
/// panel opens under the control it is about (design review S12).
pub struct OptionsMenu {
    popover: Popover,
    table: TableView,
    /// The pill the panel grows from and hangs under.
    pill: Rect,
}

impl Default for OptionsMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl OptionsMenu {
    pub fn new() -> Self {
        let mut table = TableView::new();
        // A menu of one-word choices: BODY regular labels (the Library's sort menu does the same).
        table.compact = true;
        Self { popover: Popover::new().morphing(), table, pill: Rect::default() }
    }

    /// The open table as it reads: each section's heading and its rows' `(label, value)`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_panel(&self) -> Vec<(String, Vec<(String, Option<String>)>)> {
        self.table.sections.iter()
            .map(|section| (section.header.clone(), section.rows.iter().map(|row| (row.label.clone(), row.value.clone())).collect()))
            .collect()
    }

    /// Open on `state`, growing out of `pill` (the header pill's rect).
    pub fn open(&mut self, pill: Rect, state: &WatchlistOptionsState) {
        self.pill = pill;
        self.table.open_sections(sections(state));
        self.popover.open_from(Some(pill));
    }

    /// BACK / OK: run the morph in reverse; input returns to the page now, focus on the pill.
    pub fn dismiss(&mut self) {
        self.popover.dismiss();
    }

    pub fn is_open(&self) -> bool {
        self.popover.is_open()
    }

    /// Still on screen (the close morph is running).
    pub fn visible(&self) -> bool {
        self.popover.visible()
    }

    pub fn popover(&self) -> &Popover {
        &self.popover
    }

    /// The highlighted row's identity.
    pub fn selected(&self) -> Option<OptionsRow> {
        OptionsRow::ALL.get(usize::try_from(self.table.sel).ok()?).copied()
    }

    /// Move the highlight `delta` rows (the table skips nothing here: every row is selectable).
    pub fn move_sel(&mut self, delta: i32) {
        self.table.move_sel(delta);
    }

    /// Press the highlighted row against `state`; the rows are rebuilt in place (the highlight
    /// stays where it was) when the value changed. Returns whether it changed.
    pub fn press(&mut self, state: &mut WatchlistOptionsState) -> bool {
        let Some(row) = self.selected() else { return false };
        let changed = row.apply(state);
        if changed {
            self.table.set_sections(sections(state), self.table.sel, true);
        }
        changed
    }

    /// The settled panel: hung under the pill, right edges aligned, as wide as its longest label and
    /// never narrower than the pill ([`TableView::menu_panel_width_from`], the Library's rule too)
    /// and as tall as its rows, held inside the safe frame.
    pub fn panel_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let safe = plx_ui::consts::SAFE;
        let w = self.table.menu_panel_width_from(measure, self.pill);
        let x = plx_ui::popover::hang_x(self.pill, w, safe);
        let h = self.table.measured_height().min(safe.h);
        // Hung under the pill; a menu taller than the room below it slides up rather than cutting
        // its last rows off (the ten-row menu fits under the header on a 1080 frame in every
        // shipped language, so it does not today). The pill is drawn above the panel by the opener
        // lift, never under its glass.
        let y = (self.pill.y + self.pill.h + theme::space::SM).min(safe.y + safe.h - h);
        Rect::new(x, y, w, h)
    }

    /// The `row`-th row's drawn frame inside the settled panel (`OptionsRow::ALL` order), for the
    /// focus engine's placement and the hit stops.
    pub fn row_frame(&self, row: usize, measure: &dyn plx_machine::machine::Measure) -> Option<Rect> {
        self.table.row_frame(self.panel_rect(measure), i32::try_from(row).ok()?)
    }

    /// Put the highlight on `row` (the engine's focus moved there).
    pub fn set_selected(&mut self, row: usize) {
        self.table.sel = i32::try_from(row).unwrap_or(-1);
    }

    /// The table's motion state, for the screen's canonical hash.
    pub fn write_motion(&self, c: &mut plx_machine::machine::Canon) {
        self.table.write_motion(c);
    }

    /// Step the morph and the table.
    pub fn update(&mut self, dt: f32, measure: &dyn plx_machine::machine::Measure) {
        self.popover.update(dt);
        self.table.update(dt, self.panel_rect(measure).h);
    }

    /// Draw the menu on `base` (the surface's frame painter): the glass panel (`Ground::Menu`) at
    /// the morphing rect and the rows through `Popover::morph_rows` (clipped to the surface, scaled
    /// about the panel anchor, not before the glass has taken over).
    pub fn draw(
        &self,
        base: Painter,
        field: Option<&plx_ui::underlay::UnderlayField>,
        measure: &dyn plx_machine::machine::Measure,
    ) {
        let panel = self.panel_rect(measure);
        let p = self.popover.content_painter_on(base, 0.0);
        self.popover.panel(p, panel, PANEL_RAD, field, Ground::Menu);
        self.popover.morph_rows(p, panel, PANEL_RAD, |p| self.table.draw(p, panel, measure));
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod options_tests;
