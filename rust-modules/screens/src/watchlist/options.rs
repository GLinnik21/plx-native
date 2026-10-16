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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TypeFilter {
    #[default]
    All,
    Movies,
    Shows,
}

/// The Show section: `All` / `Released` / `On your servers`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
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

    /// The pill's `<summary>` half: "Default" when nothing is changed, else the changed choices in
    /// section order (Type, Sort, Show) joined by a comma, each by its menu label ("Movies, Title").
    /// The direction is not spelled: the open menu shows it as the active key's chevron, and the
    /// pill has to share the header line with the title and the count in the longest language.
    pub fn summary(&self) -> String {
        let mut parts: Vec<&'static str> = Vec::new();
        if self.type_ != TypeFilter::All {
            parts.push(OptionsRow::Type(self.type_).label());
        }
        let default = Self::default();
        if (self.sort, self.descending) != (default.sort, default.descending) {
            parts.push(OptionsRow::Sort(self.sort).label());
        }
        if self.show != ShowFilter::All {
            parts.push(OptionsRow::Show(self.show).label());
        }
        if parts.is_empty() {
            msg::watchlist_filter_sort_default().to_string()
        } else {
            parts.join(", ")
        }
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
/// A to Z, the others newest, highest and most recent first. Date Added descending is the
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
    /// Every row in draw order: Type, Sort by, Show (boards WatchlistOptions: 3 + 4 + 3 rows).
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
            OptionsRow::Sort(SortKey::DateAdded) => msg::watchlist_filter_sort_date_added(),
            OptionsRow::Sort(SortKey::Title) => msg::watchlist_filter_sort_title(),
            OptionsRow::Sort(SortKey::Release) => msg::watchlist_filter_sort_release(),
            OptionsRow::Sort(SortKey::Rating) => msg::watchlist_filter_sort_rating(),
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
    /// already chosen flips the direction, the Library sort menu's idiom (chevron on the active
    /// row). Returns whether `state` changed (a Type or Show press on the chosen row does not).
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

/// The menu's three sections for `state`: Type, Sort by (the active key carries the direction
/// chevron), Show. Pure, so the text-fit suite drives the real builder.
pub fn sections(state: &WatchlistOptionsState) -> Vec<Section> {
    use plx_ui::icons::Icon;
    let mut type_ = Section::new(msg::watchlist_filter_type_heading());
    let mut sort = Section::new(msg::watchlist_filter_sort_heading());
    let mut show = Section::new(msg::watchlist_filter_show_heading());
    for id in OptionsRow::ALL {
        let mut row = Row::new(id.label()).checked(id.is_chosen(state));
        if matches!(id, OptionsRow::Sort(_)) && id.is_chosen(state) {
            row = row.ticon(if state.descending { Icon::ChevronDown } else { Icon::ChevronUp });
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
/// The pill and the panel are ONE surface (`popover_morph`): [`draw`](Self::draw) lays the glass,
/// then the pill's own focused fill and label as the morph's opener face (`Popover::morph_face`),
/// then the rows. The surface begins and ends as the pill, at its rect and radius and in its
/// colours, so the face is the only pill while the menu is up: the host surface names the pill as
/// its `opener`, and the page leaves it out of its own pass.
pub struct OptionsMenu {
    popover: Popover,
    table: TableView,
    /// The pill the panel grows from and hangs under.
    pill: Rect,
    /// Whether the pill will REST focused once the menu is gone: the face's plate and label end on
    /// the pill as it will be (focus back on it: the accent plate; focus gone elsewhere: the idle
    /// fill), never on a colour it will not have. True while open, since the menu is opened from
    /// the focused pill.
    rest_focused: bool,
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
        Self { popover: Popover::new().morphing(), table, pill: Rect::default(), rest_focused: true }
    }

    /// Open on `state`, growing out of `pill` (the header pill's rect).
    pub fn open(&mut self, pill: Rect, state: &WatchlistOptionsState) {
        self.pill = pill;
        self.rest_focused = true;
        self.table.open_sections(sections(state));
        self.popover.open_from(Some(pill));
    }

    /// BACK / OK: run the morph in reverse; input returns to the page now, focus on the pill.
    pub fn dismiss(&mut self) {
        self.dismiss_resting(true);
    }

    /// Run the morph in reverse so that it lands on the pill as it will REST: `pill_focused` is
    /// whether focus returns to the pill (the accent plate) or goes elsewhere (the idle fill, so
    /// the glass never passes through the accent).
    pub fn dismiss_resting(&mut self, pill_focused: bool) {
        self.rest_focused = pill_focused;
        self.popover.dismiss();
    }

    /// The plate the face draws under the label: the pill's fill as it will rest
    /// ([`header::pill_fill`](super::header::pill_fill)).
    pub fn plate_fill(&self) -> [f32; 4] {
        super::header::pill_fill(self.rest_focused)
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

    /// The settled panel: hung under the pill, right edges aligned, as wide as its longest label
    /// ([`TableView::menu_panel_width`]) and as tall as its rows, held inside the safe frame.
    pub fn panel_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        let safe = plx_ui::consts::SAFE;
        let w = self.table.menu_panel_width(measure);
        let x = (self.pill.x + self.pill.w - w).max(safe.x);
        let h = self.table.measured_height().min(safe.h);
        // Hung under the pill; when the whole menu (ten rows, three headings) is taller than the
        // room below it, it slides up over the pill rather than cutting its last rows off, the
        // pill having dissolved into the panel by then (DS7). The page draws no pill under it
        // while the menu is up (`WatchlistMenuScreen::opener`): left there, the glass rim's lens
        // pulled its label into the panel's top edge.
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
    /// the morphing rect, the pill's face over it ([`draw_face`](Self::draw_face)), and the rows
    /// through `Popover::morph_rows` (clipped to the surface, scaled about the panel anchor, not
    /// before the glass has replaced the pill's fill).
    pub fn draw(
        &self,
        base: Painter,
        field: Option<&plx_ui::underlay::UnderlayField>,
        measure: &dyn plx_machine::machine::Measure,
        state: &WatchlistOptionsState,
    ) {
        let panel = self.panel_rect(measure);
        let p = self.popover.content_painter_on(base, 0.0);
        self.popover.panel(p, panel, PANEL_RAD, field, Ground::Menu);
        self.draw_face(base, measure, state);
        self.popover.morph_rows(p, panel, PANEL_RAD, |p| self.table.draw(p, panel, measure));
    }

    /// The pill as the morph's opener face: its focused fill as the plate, its label clipped to
    /// the surface. All a HELD surface shows (`Screen::draw_held`): at the start of the morph it is
    /// the page's own focused pill.
    pub fn draw_face(&self, base: Painter, measure: &dyn plx_machine::machine::Measure, state: &WatchlistOptionsState) {
        let p = self.popover.content_painter_on(base, 0.0);
        let summary = state.summary();
        let face = super::header::HeaderFace { title: "", count: None, summary: &summary, pill_focused: self.rest_focused };
        let pill = self.pill;
        self.popover.morph_face(p, self.panel_rect(measure), PANEL_RAD, self.plate_fill(), |p| {
            super::header::draw_pill_label(p, pill, &face)
        });
    }
}

#[cfg(test)]
#[path = "options_tests.rs"]
mod options_tests;
