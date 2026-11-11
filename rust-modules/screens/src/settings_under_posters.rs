//! **Settings > Libraries > Under posters**: what the focused poster shows beneath it (Settings v2,
//! composable details: Under posters v3).
//!
//! One setting, saved per profile (`plx_plex::plex::session::UnderPosters`). The page holds five
//! rows: a Title switch, two pickers (Movie details and TV show details, each a pushed page,
//! [`DetailsPage`]), a Sort value switch and Reset. A picker is one section, "Show under posters",
//! with a row per **part** a line under that kind of poster can carry (a movie: Year, Length, Genre,
//! Video quality, Age rating, Score; a TV show: Year, Seasons, Genre, Age rating, Score): a check
//! mark on the ticked ones and, on the right, a sample of the part. OK ticks or unticks the row and
//! STAYS on the page; Back returns to the Under posters page. The line under a poster is the ticked
//! parts' values joined with " · " in the parts' fixed order, **at most four**
//! ([`DetailParts::MAX`]); with four ticked the others are DISABLED, the shared control state
//! (`FormSection::disabled`), and say why in the sentence beside the table. The details rows' value
//! is the ticked names joined (`Year · Length`), "Nothing", or "3 details" (a count) once the joined
//! names are longer than [`NAMES_BUDGET_CHARS`] ([`names_value`]); the Settings root's Under posters row reads the Movie one ([`summary`]).
//!
//! **No panel and no sub-lines.** A row is its label and its value; the sentence that explained it
//! under its label is stated in the left column while the row has focus ([`sentence_for`],
//! [`picker_sentence`], the shared [`FollowCopy`](plx_ui::follow_copy::FollowCopy)), and a row with
//! none shows the page's. The picture under that sentence is the shared picture slot
//! ([`RootPicture`], also drawn on the Settings root and on both pickers): a movie poster on the left
//! and a TV show poster on the right, each under its KIND ("MOVIE", "TV SHOW") and over the lines the
//! stored setting selects. Each tile's art is the first poster of its kind from the widest cache the
//! Browse store holds ([`preview_posters`]; a kind it has none of is asked for as its library's shelves, [`PosterAsk`]); a kind with none anywhere is a finished art-less face
//! (`plx_ui::widgets::absent_card`), never a skeleton. The preview words each selected part with
//! the shared caption formatters and supplies the picker's example when the poster lacks the fact.
//! Details can wrap within their preview column. **The column of the kind that is not being chosen is dimmed** to the design's `dim`
//! ([`DIM`](plx_ui::follow_copy::DIM), 0.45; `Sv2s-P2-Details`): on the Under posters page the kind
//! whose details row is not focused (both are at full strength on Title, Sort value and Reset and on
//! the Settings root), in a picker the other kind. The caption, tile, title line and details line
//! are drawn as one group at that opacity, which moves on the shared appear spring
//! ([`ColumnDims`](plx_ui::follow_copy::ColumnDims)). It is NOT the disabled control state: a poster
//! tile is not a control.
//!
//! Except a movie or TV show library grid showing its sort value (one value line under every card),
//! the policy is the same under every POSTER in the app (Search results, a person's filmography,
//! Detail's Related and collection shelves and a collection's members included); a landscape still
//! or a headshot is not a poster and wears none of it. With Sort value On, the lines show each
//! poster's year (a library grid sorted by Year) while the Sort value row has focus, and never in
//! the pickers: Title and Details still govern every other poster surface.
//!
//! The write goes through the one door every live setting uses (`PreferenceCmd::Choose`, Confirm):
//! the row shows the pick at once, the stored value settles it when the receipt answers, and a
//! refused write (nobody seated) puts the row back.

use super::*;
use plx_data::pms::PmsMovie;
use plx_plex::plex::session::{DetailPart, DetailParts, UnderPosters};
use plx_ui::caption_policy::{part_value, CaptionFacts, CaptionPolicy};
use plx_ui::follow_copy::{ColumnDims, FollowCopy};
use plx_ui::widgets::Art;
use crate::registry::tile_facts;

/// The stored setting as the UI's caption policy. The mapping lives here, not in `plx_plex`: the
/// data layer must not name the UI layer (the parts themselves are the one `plx_base` type both read).
pub fn caption_policy_of(v: UnderPosters) -> CaptionPolicy {
    CaptionPolicy { title: v.title, details: v.details, show_details: v.show_details }
}

impl PosterKind {
    /// The stored parts for this kind.
    fn parts(self, v: UnderPosters) -> DetailParts {
        match self {
            PosterKind::Movie => v.details,
            PosterKind::Show => v.show_details,
        }
    }

    /// `v` with this kind's parts set to `parts` (held to what the kind offers and to the maximum).
    fn with_parts(self, v: UnderPosters, parts: DetailParts) -> UnderPosters {
        match self {
            PosterKind::Movie => UnderPosters { details: parts.for_movies(), ..v },
            PosterKind::Show => UnderPosters { show_details: parts.for_shows(), ..v },
        }
    }

    /// The parts this kind's picker lists, in the line's order.
    fn offered(self) -> &'static [DetailPart] {
        match self {
            PosterKind::Movie => &DetailPart::MOVIE,
            PosterKind::Show => &DetailPart::SHOW,
        }
    }

    /// The row on the Under posters page that opens this kind's picker, and that picker's title.
    fn row_label(self) -> &'static str {
        match self {
            PosterKind::Movie => plx_platform::i18n::msg::under_posters_movie_details_row(),
            PosterKind::Show => plx_platform::i18n::msg::under_posters_show_details_row(),
        }
    }

    /// The sentence beside that row on the page.
    fn row_sentence(self) -> &'static str {
        match self {
            PosterKind::Movie => plx_platform::i18n::msg::under_posters_movie_details_detail(),
            PosterKind::Show => plx_platform::i18n::msg::under_posters_show_details_detail(),
        }
    }

    /// The word over a tile of this kind: the library kind's own singular name, as the Library reads
    /// it (`browse.kind.movie`, `browse.kind.tv_show`). Upper-cased where it is drawn.
    fn caption(self) -> &'static str {
        match self {
            PosterKind::Movie => plx_platform::i18n::msg::browse_kind_movie(),
            PosterKind::Show => plx_platform::i18n::msg::browse_kind_tv_show(),
        }
    }

    /// Is `poster` a title of this kind?
    fn is_kind_of(self, poster: &PmsMovie) -> bool {
        match self {
            PosterKind::Movie => poster.kind == 0,
            PosterKind::Show => poster.kind == 1,
        }
    }
}

/// A part's name in a picker and in a details row's value.
fn part_label(part: DetailPart) -> &'static str {
    use plx_platform::i18n::msg;
    match part {
        DetailPart::Year => msg::under_posters_part_year(),
        DetailPart::Length => msg::under_posters_part_length(),
        DetailPart::Seasons => msg::under_posters_part_seasons(),
        DetailPart::Genre => msg::under_posters_part_genre(),
        DetailPart::VideoQuality => msg::under_posters_part_video(),
        DetailPart::AgeRating => msg::under_posters_part_age_rating(),
        DetailPart::Score => msg::under_posters_part_score(),
    }
}

/// How many characters of joined part names a details row's value holds before it reads a count
/// instead. The value column is what the row's label leaves (381 px for "Detalles de películas",
/// 421 px for the Belarusian "Звесткі серыялаў"), and the longest languages' names run about 14 px a
/// character: `every_under_posters_string_fits_in_every_language` walks every set of every kind in
/// every language and fails on a value the television would end in an ellipsis. Nothing is lost to
/// the count: the picker one level down names every ticked part.
const NAMES_BUDGET_CHARS: usize = 26;

/// **A details row's value**: the ticked parts' names joined with " · " in the line's order
/// ("Year · Length"), "Nothing" when none, and the count ("3 details") when the joined names run
/// past [`NAMES_BUDGET_CHARS`] and would end in an ellipsis in the row.
fn names_value(parts: DetailParts) -> String {
    use plx_platform::i18n::msg;
    if parts.is_empty() {
        return msg::under_posters_none().to_string();
    }
    let names = parts.iter().map(part_label).collect::<Vec<_>>().join(" \u{b7} ");
    if names.chars().count() > NAMES_BUDGET_CHARS { msg::under_posters_details_count(parts.len() as i64) } else { names }
}

/// **The Settings root's value for the Under posters row**: the Movie details row's value (the show
/// line is one level down), the way Language reads its language.
pub(super) fn summary(v: UnderPosters) -> String {
    // "Title" when the title is all a movie's poster wears (never "Nothing" beside a name).
    if v.title && v.details.is_empty() {
        return plx_platform::i18n::msg::under_posters_title_row().to_string();
    }
    names_value(v.details)
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpId {
    Title,
    MovieDetails,
    ShowDetails,
    SortValue,
    Reset,
}

impl FormId for UpId {
    fn key(&self) -> RowKey {
        RowKey(match self {
            UpId::Title => 1,
            UpId::MovieDetails => 2,
            UpId::SortValue => 3,
            UpId::ShowDetails => 4,
            UpId::Reset => 30,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum UpAction {
    Title,
    /// The details rows are pushes (`RowKind::Nav`): the form answers them with the page, never with this.
    Details,
    SortValue,
    Reset,
}

/// **The sentence the focused row states beside the table.** A pure function of the row's identity:
/// the page's own sentence when nothing is focused.
fn sentence_for(id: Option<&UpId>) -> &'static str {
    use plx_platform::i18n::msg;
    match id {
        Some(UpId::Title) => msg::under_posters_title_detail(),
        Some(UpId::MovieDetails) => PosterKind::Movie.row_sentence(),
        Some(UpId::ShowDetails) => PosterKind::Show.row_sentence(),
        Some(UpId::SortValue) => msg::under_posters_sort_value_detail(),
        Some(UpId::Reset) => msg::under_posters_reset_detail(),
        None => msg::under_posters_copy(),
    }
}

/// **Which column of the picture is dimmed for the focused row**: the kind whose details row is
/// NOT focused (`[movie, show]`); both stay at full strength on Title, Sort value and Reset.
fn dimmed_columns_for(id: Option<&UpId>) -> [bool; 2] {
    [matches!(id, Some(UpId::ShowDetails)), matches!(id, Some(UpId::MovieDetails))]
}

/// The page's rows from plain inputs (the text-fit tests call this directly).
fn under_posters_form(v: UnderPosters) -> Form<UpId, UpAction, SettingsPage> {
    use plx_platform::i18n::msg;
    let rows = FormSection::new("")
        .item(UpId::Title, RowKind::Toggle, UpAction::Title, Row::new(msg::under_posters_title_row()).toggle(v.title));
    let rows = [PosterKind::Movie, PosterKind::Show].into_iter().fold(rows, |rows, kind| {
        let id = match kind {
            PosterKind::Movie => UpId::MovieDetails,
            PosterKind::Show => UpId::ShowDetails,
        };
        rows.item(id, RowKind::Nav(SettingsPage::UnderPostersDetails(kind)), UpAction::Details,
            Row::new(kind.row_label()).value(names_value(kind.parts(v))).chevron(true))
    });
    let rows = rows.item(UpId::SortValue, RowKind::Toggle, UpAction::SortValue,
        Row::new(msg::under_posters_sort_value()).toggle(v.sort_value));
    let reset = FormSection::new("").item(
        UpId::Reset,
        RowKind::Button,
        UpAction::Reset,
        Row::new(msg::under_posters_reset()),
    ).disabled(v == UnderPosters::DEFAULT);
    Form::new().section(rows).section(reset)
}

pub(super) struct UnderPostersPage {
    entry: EntryId,
    form: FormTable<UpId, UpAction, SettingsPage>,
    state: UpState,
    /// The sentence beside the table, which fades to the focused row's.
    follow: FollowCopy,
    /// Which column of the picture is dimmed, moving as focus moves between the details rows.
    dims: ColumnDims<2>,
    /// A pick not yet answered: the value the rows show meanwhile, and the receipt of its write.
    pending: Option<(UnderPosters, std::sync::mpsc::Receiver<bool>)>,
    /// The libraries whose shelves the picture is owed ([`PosterAsk`]).
    ask: PosterAsk,
}

struct UpState {
    value: UnderPosters,
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
    busy: bool,
}

impl LogicalState for UpState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).u32(u32::from(self.value.pack())).bool(self.busy);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "under_posters sel={} title={} details={} show_details={} busy={} sort_value={}",
            self.sel.0, self.value.title, self.value.details.bits(), self.value.show_details.bits(), self.busy, self.value.sort_value
        ));
    }
}

impl UnderPostersPage {
    pub(super) fn new(entry: EntryId) -> Self {
        let value = plx_media::route::under_posters();
        let sel = UpId::Title.key();
        let mut page = Self {
            entry,
            form: FormTable::new(super::super::registry::BAND),
            state: UpState { value, sel, busy: false },
            follow: FollowCopy::new(""),
            dims: ColumnDims::at([false, false]),
            pending: None,
            ask: PosterAsk::default(),
        };
        page.rebuild();
        // A page opens on its first row's sentence at once, with nothing fading.
        page.follow = FollowCopy::new(sentence_for(page.form.selected_id()));
        page.dims = ColumnDims::at(dimmed_columns_for(page.form.selected_id()));
        page
    }

    /// Re-derive the rows from the shown value, keeping the cursor on its row by identity.
    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(UpId::Title);
        self.form.table.compact = false;
        self.form.table.board_metrics(true);
        self.form.set(under_posters_form(self.state.value), Some(&keep));
        self.form.table.list_focused = true;
        self.follow.follow(sentence_for(self.form.selected_id()));
    }

    fn view(&self) -> TableScreen<'_> {
        use plx_platform::i18n::msg;
        TableScreen::new(
            Header::new(RouteLayout::settings_page(), Some(msg::settings_title()), msg::under_posters_title(), msg::under_posters_copy())
                .with_follow(&self.follow),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form)
    }

    /// Send `next` through the one write door and show it meanwhile.
    fn send(&mut self, next: UnderPosters, fx: &mut Effects<'_, InnerHost>) {
        if next == self.state.value {
            return;
        }
        let (reply, receipt) = std::sync::mpsc::channel();
        fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::Choose(
            plx_media::route::Choice::UnderPosters(next), reply))));
        self.pending = Some((next, receipt));
        self.state.value = next;
        self.state.busy = true;
        self.rebuild();
    }

    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let Some(action) = form_activate(&self.form, key, fx) else { return };
        let v = self.state.value;
        let next = match action {
            UpAction::Title => UnderPosters { title: !v.title, ..v },
            UpAction::Details => return,
            UpAction::SortValue => UnderPosters { sort_value: !v.sort_value, ..v },
            UpAction::Reset => UnderPosters::DEFAULT,
        };
        self.send(next, fx);
    }

    /// Whether the picture words each poster's year (a library grid sorted by Year) instead of the
    /// caption: Sort value is On and its row has focus. Title and Details still govern Home, Search,
    /// the Watchlist and Collections, so the picture of their rows stays the caption they set.
    fn previews_sort_value(&self) -> bool {
        self.state.value.sort_value && self.state.sel == UpId::SortValue.key()
    }

    /// With no write of its own in flight, the rows read the stored value: a details picker
    /// above this page writes it too, and a page that kept what it last wrote would show the old
    /// pick on return and send it back with the next Title toggle.
    fn follow_stored(&mut self) -> bool {
        let stored = plx_media::route::under_posters();
        if stored == self.state.value {
            return false;
        }
        self.state.value = stored;
        self.rebuild();
        true
    }

    /// Settle an answered write: the row reads the stored value again, which holds the pick only
    /// if the write was durable.
    fn poll(&mut self) -> bool {
        let Some((_, receipt)) = &self.pending else { return self.follow_stored() };
        match receipt.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
        self.pending = None;
        self.state.busy = false;
        self.state.value = plx_media::route::under_posters();
        self.rebuild();
        true
    }
}

// ---------------------------------------------------------------------------------------------
// the picture
// ---------------------------------------------------------------------------------------------

/// The year of the sample a picture with no poster behind it shows.
const SAMPLE_YEAR: i32 = 2019;

/// The sample facts a tile with no poster behind it is captioned from.
fn sample_facts() -> CaptionFacts {
    use plx_platform::i18n::msg;
    CaptionFacts {
        title: Some(msg::under_posters_sample_title().to_string()),
        year: Some(SAMPLE_YEAR),
        duration_min: Some(134),
        genre: Some(msg::under_posters_sample_genre().to_string()),
        video_resolution: Some("4k".to_string()),
        hdr: true,
        content_rating: Some("PG-13".to_string()),
        audience_rating: Some(7.8),
        ..Default::default()
    }
}

/// The sample a movie or a show wears when the library has no poster of its kind.
fn kind_sample(kind: PosterKind) -> CaptionFacts {
    match kind {
        PosterKind::Movie => CaptionFacts { year: Some(2024), duration_min: Some(112), is_movie: true, ..sample_facts() },
        PosterKind::Show => CaptionFacts {
            title: Some(plx_platform::i18n::msg::browse_kind_show().to_string()),
            year: Some(SAMPLE_YEAR), duration_min: None, season_count: Some(6), is_show: true,
            content_rating: Some("TV-14".to_string()), audience_rating: Some(8.4), ..sample_facts()
        },
    }
}

/// The facts a picture tile of `kind` is worded from: its poster's own, else the sample a movie or
/// a show wears.
fn picture_facts(kind: PosterKind, poster: Option<&PmsMovie>) -> CaptionFacts {
    poster.map_or_else(|| kind_sample(kind), tile_facts::caption_facts)
}

/// The first of `posters` that is a title of `kind`.
fn poster_of<'a>(kind: PosterKind, posters: &[&'a PmsMovie]) -> Option<&'a PmsMovie> {
    posters.iter().copied().find(|p| kind.is_kind_of(p))
}

/// The facts shared by the picture and its picker.
fn kind_facts(kind: PosterKind, posters: &[&PmsMovie]) -> CaptionFacts {
    picture_facts(kind, poster_of(kind, posters))
}

/// The preview's `[title, details]` demonstrates every selected part with the same examples as
/// the picker, even for a partially watched item whose library caption shows its progress.
fn lines_of(v: UnderPosters, kind: PosterKind, facts: &CaptionFacts) -> [Option<String>; 2] {
    let title = facts.title.as_ref().filter(|s| v.title && !s.is_empty()).cloned();
    let details = plx_ui::caption_policy::join(kind.parts(v).iter()
        .map(|part| part_example(kind, part, facts))
        .filter(|example| !example.is_empty())
        .collect());
    [title, details]
}

/// The lines under the picture's tile of `kind`: the caption the setting gives it, or (the Sort
/// value row has focus, `sorted`) the year a library sorted by Year would print there.
fn picture_lines(v: UnderPosters, kind: PosterKind, poster: Option<&PmsMovie>, sorted: bool) -> [Option<String>; 2] {
    if sorted {
        let year = poster.map_or(SAMPLE_YEAR, |p| p.year);
        return [None, Some(crate::registry::sort_info::year_value(year).text)];
    }
    lines_of(v, kind, &picture_facts(kind, poster))
}

/// **The Under posters picture** (boards `Sv2-2`, `Sv2s-P1`, `Sv2s-P2`): a movie poster on the left and
/// a TV show poster on the right, each art-only, with its KIND ("MOVIE", "TV SHOW") centred over it
/// and the lines the stored setting gives it centred under it, painted INTO the picture slot. No
/// panel, ring or tick: it is display-only and never reads as a control. The Settings root, the
/// Under posters page and both details pickers draw this one picture, so they agree.
///
/// The column of the kind that is not being chosen is dimmed ([`Self::opacity`], the design's
/// [`DIM`](plx_ui::follow_copy::DIM)): caption, tile and lines are drawn together at that opacity.
pub(super) struct RootPicture<'a> {
    pub value: UnderPosters,
    pub posters: &'a [&'a PmsMovie],
    /// Word each tile with its poster's year rather than the caption.
    pub sorted: bool,
    /// The opacity of each kind's whole column, `[movie, show]` ([`ColumnDims::opacity`]).
    pub opacity: [f32; 2],
}

/// The board's picture tiles: 250 x 375 posters, their left edges 410 apart (a 660 px slot).
const PICTURE_TILE_W: f32 = 250.0;
const PICTURE_TILE_H: f32 = 375.0;
const PICTURE_TILE_STEP: f32 = 410.0;
/// The kind caption's CSS box (24 px on a 24 px line) and the tile's top under it: 52 px under the slot's.
const PICTURE_CAPTION_H: f32 = 24.0;
const PICTURE_TILE_TOP: f32 = 52.0;
/// The first line's CSS box under a tile: 24 px on a 24 px line, its top 28 px under the tile's
/// bottom; the second line (the details, under a title) hangs one line and a gap under it.
const PICTURE_LINE_GAP: f32 = 28.0;
const PICTURE_LINE_H: f32 = 24.0;
const PICTURE_LINE_STEP: f32 = 32.0;
/// The room a line or a caption is centred in: the board's 380 px, centred on the tile.
const PICTURE_LINE_W: f32 = 380.0;
/// The preview has room for two details lines below the optional title.
const PICTURE_DETAIL_LINES: usize = 2;

impl plx_ui::follow_copy::PictureSlot for RootPicture<'_> {
    fn draw(&self, p: Painter, slot: Rect, measure: &dyn plx_machine::machine::Measure) {
        use plx_ui::label::{HAlign, Label, VAlign};
        use plx_ui::text_view::TextView;
        let size = theme::size::CAPTION;
        // The art of a tile is the shared one, with no watch state (the fan's own rule). A kind with
        // no poster path anywhere is `None`: drawn as the art-less face, never as `Art::Poster(None)`,
        // which is a skeleton (a card that is still loading, and for an empty key one that never will).
        let kinds = [PosterKind::Movie, PosterKind::Show];
        let arts: Vec<Option<Art>> = kinds.iter()
            .map(|k| poster_of(*k, self.posters).filter(|m| !m.thumb.is_empty()).map(|m| Art::Poster(Some(tile_facts::of(m)))))
            .collect();
        let line_cap_top = plx_gfx::text::css_cap_drop(size, PICTURE_LINE_H, measure.cap_h(size));
        let caption_cap_top = plx_gfx::text::css_cap_drop(size, PICTURE_CAPTION_H, measure.cap_h(size));
        let centred = (PICTURE_LINE_W - PICTURE_TILE_W) * 0.5;
        for (i, kind) in kinds.iter().enumerate() {
            let x = slot.x + PICTURE_TILE_STEP * i as f32;
            // One group per kind: caption, tile and lines share the column's opacity.
            let p = p.alpha(self.opacity[i]);
            // The kind over its tile: the table's section-header caps, centred.
            plx_ui::table::draw_caps_label_aligned(
                p, Rect::new(x - centred, slot.y + caption_cap_top, PICTURE_LINE_W, plx_ui::table::caps_band_h()),
                kind.caption(), PICTURE_LINE_W,
                theme::TEXT_READING, plx_ui::table::BOARD_CAPS_TRACK_PX,
                HAlign::Center, measure,
            );
            let tile = Rect::new(x, slot.y + PICTURE_TILE_TOP, PICTURE_TILE_W, PICTURE_TILE_H);
            match arts[i] {
                Some(art) => plx_ui::widgets::card(p, tile, plx_ui::follow_copy::PosterFan::stateless(art), theme::CARD_RING_RAD, false, 1.0, 0.0),
                None => plx_ui::widgets::absent_card(p, tile, theme::CARD_RING_RAD),
            }
            let lines = picture_lines(self.value, *kind, poster_of(*kind, self.posters), self.sorted);
            // Present lines stack from the first: a title (bold) over its details.
            for (n, (line, is_title)) in lines.into_iter().zip([true, false]).filter_map(|(l, t)| l.map(|l| (l, t))).enumerate() {
                let frame = Rect::new(x - centred,
                    tile.y + PICTURE_TILE_H + PICTURE_LINE_GAP + PICTURE_LINE_STEP * n as f32 + line_cap_top,
                    PICTURE_LINE_W, PICTURE_LINE_H);
                if is_title {
                    let line = measure.fit_line(&line, PICTURE_LINE_W, size, true);
                    Label::new(line.as_ptr(), size, theme::TEXT_PRIMARY)
                        .h(HAlign::Center).v(VAlign::CapTop).bold().draw(p, frame);
                } else {
                    TextView::new(&line, size, theme::CAPTION_DETAIL_INK)
                        .with_measure(measure).h(HAlign::Center).leading(PICTURE_LINE_STEP)
                        .max_lines(PICTURE_DETAIL_LINES).draw(p, frame);
                }
            }
        }
    }
}

/// The posters a page's picture draws: the first movie and the first show, with art, from the
/// widest cache the Browse store holds (`DirectoryView::kind_poster`: the current library, then every
/// other library it still has rows of), so a user whose last opened library lists only Movies still
/// sees a real show. A kind the cache has no poster of is absent from the list, and its tile is the
/// finished art-less face ([`RootPicture`]).
pub(super) fn preview_posters<'a>(cx: &Cx<'a, InnerHost>) -> Vec<&'a PmsMovie> {
    [plx_data::pms::KIND_MOVIE, plx_data::pms::KIND_SHOW].into_iter().filter_map(|kind| cx.views.directory.kind_poster(kind)).collect()
}

/// **A page that draws the picture asks the Browse store for the kind it has no poster of.** The
/// store caches only libraries the user has opened, so a show library never opened leaves the show
/// tile art-less although the library exists. Each kind missing from [`preview_posters`] whose
/// library the directory lists is asked for as that library's hubs (`LibraryWork::Hubs`: a read of
/// its shelves into the section's own cache that does not switch the library the user is on), and
/// the poster then arrives through `preview_posters` like any cached one. Three things make that
/// work from a page that is not a Library, and each is the store's own rule, not a shortcut: the
/// store reads one library's shelves at a time (the shared hubs claim), so the second kind's read
/// starts only on a later step; a landed read is STAGED and committed by the next `Hubs` command
/// (`may_publish`), never by the landing; and nothing lands until the store is pumped
/// (`StoreWork::Browse`, what the Library and Recommended pages push every tick). So the page sends
/// the same idempotent ask and the pump on each tick, per kind, until the poster is there or the
/// store has published that library's shelves ([`DirectoryView::kind_shelves_published`](plx_data::browse::DirectoryView::kind_shelves_published):
/// a library whose shelves hold no title of the kind is answered, and asked no more).
#[derive(Default)]
pub(super) struct PosterAsk;

impl PosterAsk {
    pub(super) fn step(&mut self, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) {
        use plx_data::browse::SecKind;
        use plx_data::stores::browse::{BrowseCmd, LibraryWork};
        use plx_data::stores::{StoreCmd, StoreId, StoreWork};
        let have = preview_posters(cx);
        let mut asked = false;
        for (kind, sec) in [(PosterKind::Movie, SecKind::Movie), (PosterKind::Show, SecKind::Show)] {
            if poster_of(kind, &have).is_some() || cx.views.directory.kind_shelves_published(sec) {
                continue;
            }
            let Some(target) = cx.views.directory.kind_address(sec) else { continue };
            asked = true;
            fx.push(Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
                target, work: LibraryWork::Hubs { may_publish: true } }))));
        }
        if asked {
            fx.push(Fx::App(AppFx::StoreWork(StoreWork::Browse)));
        }
    }
}

impl Machine<InnerHost> for UnderPostersPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.ask.step(cx, fx);
                // Seat the cursor explicitly, as the Language page does: the generic Enter runs
                // before queued mount effects.
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.ask.step(cx, fx);
                if self.poll() {
                    fx.invalidate(Provenance::Input);
                }
                self.form.table.update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
                self.follow.update(t.dt());
                self.dims.update(t.dt());
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                self.follow.follow(sentence_for(self.form.selected_id()));
                self.dims.set(dimmed_columns_for(self.form.selected_id()));
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                Handled::Yes
            }
            ScreenEvent::Input(plx_machine::machine::InputEvent {
                kind: plx_machine::machine::InputKind::Key { key: Key::Right, at_edge: true, .. }, ..
            }) => {
                if let Some(key) = cx.focus.current.and_then(|k| form_right_target(&self.form, k.elem)) {
                    self.activate(key, fx);
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(UnderPostersPage, InnerHost, view);

impl Screen<InnerHost> for UnderPostersPage {
    fn name(&self) -> &'static str { crate::registry::word::UNDER_POSTERS }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let posters = preview_posters(f.cx);
        let picture = RootPicture {
            value: self.state.value, posters: &posters, sorted: self.previews_sort_value(), opacity: self.dims.opacity(),
        };
        let mut v = self.view();
        v.header.picture = Some(&picture);
        plx_ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

// ---------------------------------------------------------------------------------------------
// the details pickers
// ---------------------------------------------------------------------------------------------

/// A part's row key: its place among the parts, clear of the first page's rows.
#[derive(Clone, PartialEq, Eq, Debug)]
struct PartId(DetailPart);

impl FormId for PartId {
    fn key(&self) -> RowKey {
        RowKey(10 + self.0 as u32)
    }
}

/// What a part shows under a poster, as the picture words it from `facts` ([`kind_facts`]): the
/// row's sample on the right and the example in its sentence. A part the poster lacks (a movie with
/// no genre, say) shows the kind's sample ([`kind_sample`]) instead, so a row always shows an
/// example beside the switch and under the preview poster when enabled.
fn part_example(kind: PosterKind, part: DetailPart, facts: &CaptionFacts) -> String {
    let cx = plx_platform::i18n::current();
    part_value(part, facts, cx).or_else(|| part_value(part, &kind_sample(kind), cx)).unwrap_or_default()
}

/// **The sentence the focused picker row states beside the table**: the part's own ("Shows the
/// release year, like 2024.") or, for a row that cannot be ticked because four already are, why
/// ("Up to four details fit under a poster. Remove one to add genre.").
fn picker_sentence(kind: PosterKind, parts: DetailParts, part: DetailPart, example: &str) -> String {
    use plx_platform::i18n::msg;
    if !parts.fits(part) {
        return match part {
            DetailPart::Year => msg::under_posters_blocked_year(),
            DetailPart::Length => msg::under_posters_blocked_length(),
            DetailPart::Seasons => msg::under_posters_blocked_seasons(),
            DetailPart::Genre => msg::under_posters_blocked_genre(),
            DetailPart::VideoQuality => msg::under_posters_blocked_video(),
            DetailPart::AgeRating => msg::under_posters_blocked_age_rating(),
            DetailPart::Score => msg::under_posters_blocked_score(),
        }
        .to_string();
    }
    match (part, kind) {
        (DetailPart::Year, PosterKind::Movie) => msg::under_posters_sentence_year_movie(example),
        (DetailPart::Year, PosterKind::Show) => msg::under_posters_sentence_year_show(example),
        (DetailPart::Length, _) => msg::under_posters_sentence_length(example),
        (DetailPart::Seasons, _) => msg::under_posters_sentence_seasons(example),
        (DetailPart::Genre, _) => msg::under_posters_sentence_genre(example),
        (DetailPart::VideoQuality, _) => msg::under_posters_sentence_video(example),
        (DetailPart::AgeRating, _) => msg::under_posters_sentence_age_rating(example),
        (DetailPart::Score, _) => msg::under_posters_sentence_score(example),
    }
}

/// The picker's rows: one row per part the kind offers, in the line's order, a check mark on the
/// ticked ones and a sample of each on the right (all from the picture's tile of that kind). With
/// the maximum ticked, the others are DISABLED (the shared control state, `Row::disabled`): focusable so the viewer can land
/// on one and read why, and inert to OK.
fn details_form(kind: PosterKind, parts: DetailParts, facts: &CaptionFacts) -> Form<PartId, DetailPart, SettingsPage> {
    let mut section = FormSection::new(plx_platform::i18n::msg::under_posters_picker_header());
    for part in kind.offered().iter().copied() {
        let row = Row::new(part_label(part)).value(part_example(kind, part, facts));
        section = section.choice(PartId(part), part, row, |id| parts.contains(id.0)).disabled(!parts.fits(part));
    }
    Form::new().section(section)
}

pub(super) struct DetailsPage {
    entry: EntryId,
    kind: PosterKind,
    form: FormTable<PartId, DetailPart, SettingsPage>,
    state: DetailsState,
    /// The sentence beside the table, which fades to the focused row's.
    follow: FollowCopy,
    /// What the rows' samples are worded from: the picture's tile of this kind ([`kind_facts`]),
    /// refreshed whenever the library's cached posters change.
    facts: CaptionFacts,
    /// A tick not yet answered, and the receipt of its write.
    pending: Option<(UnderPosters, std::sync::mpsc::Receiver<bool>)>,
}

struct DetailsState {
    kind: PosterKind,
    value: UnderPosters,
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
    busy: bool,
}

impl DetailsState {
    /// The part the focused row stands for.
    fn focused(&self) -> DetailPart {
        self.kind.offered().iter().copied().find(|p| PartId(*p).key() == self.sel).unwrap_or(self.kind.offered()[0])
    }
}

impl LogicalState for DetailsState {
    fn write(&self, w: &mut Canon) {
        w.u8(self.kind as u8).u32(self.sel.0).u32(u32::from(self.value.pack())).bool(self.busy);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "under_posters_details kind={} sel={} parts={} busy={}",
            self.kind as u8, self.sel.0, self.kind.parts(self.value).bits(), self.busy
        ));
    }
}

impl DetailsPage {
    pub(super) fn new(entry: EntryId, kind: PosterKind) -> Self {
        let value = plx_media::route::under_posters();
        let mut page = Self {
            entry,
            kind,
            form: FormTable::new(super::super::registry::BAND),
            state: DetailsState { kind, value, sel: PartId(kind.offered()[0]).key(), busy: false },
            follow: FollowCopy::new(""),
            facts: picture_facts(kind, None),
            pending: None,
        };
        page.rebuild();
        page.follow = FollowCopy::new(&page.sentence());
        page
    }

    /// The sentence for the focused row now.
    fn sentence(&self) -> String {
        let part = self.state.focused();
        picker_sentence(self.kind, self.kind.parts(self.state.value), part, &part_example(self.kind, part, &self.facts))
    }

    /// Word the rows' samples from the picture's tile as it is now; `true` when they changed.
    fn adopt(&mut self, cx: &Cx<'_, InnerHost>) -> bool {
        let facts = kind_facts(self.kind, &preview_posters(cx));
        if facts == self.facts {
            return false;
        }
        self.facts = facts;
        self.rebuild();
        true
    }

    fn rebuild(&mut self) {
        let keep = self.form.selected_id().cloned().unwrap_or(PartId(self.state.focused()));
        self.form.table.compact = false;
        self.form.table.board_metrics(true);
        self.form.set(details_form(self.kind, self.kind.parts(self.state.value), &self.facts), Some(&keep));
        self.form.table.list_focused = true;
        let sentence = self.sentence();
        self.follow.follow(&sentence);
    }

    fn view(&self) -> TableScreen<'_> {
        TableScreen::new(
            Header::new(RouteLayout::settings_page(), Some(plx_platform::i18n::msg::under_posters_title()), self.kind.row_label(), self.follow.text())
                .with_follow(&self.follow),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form)
    }

    /// OK on a row ticks it or unticks it and STAYS on the page: the new set goes through the one
    /// write door and the rows and the picture show it at once. A row that does not fit (four are
    /// ticked) is disabled in the form, so `form_activate` answers nothing for it.
    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        let Some(part) = form_activate(&self.form, key, fx) else { return };
        let parts = self.kind.parts(self.state.value);
        let next = self.kind.with_parts(self.state.value, parts.toggled(part));
        if next == self.state.value {
            return;
        }
        let (reply, receipt) = std::sync::mpsc::channel();
        fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::Choose(
            plx_media::route::Choice::UnderPosters(next), reply))));
        self.pending = Some((next, receipt));
        self.state.value = next;
        self.state.busy = true;
        self.rebuild();
        fx.invalidate(Provenance::Input);
    }

    /// Settle an answered write: the rows read the stored value again, which holds the tick only if
    /// the write was durable (a refused one puts the check mark back).
    fn poll(&mut self) -> bool {
        let Some((_, receipt)) = &self.pending else { return false };
        match receipt.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
        self.pending = None;
        self.state.busy = false;
        self.state.value = plx_media::route::under_posters();
        self.rebuild();
        true
    }
}

impl Machine<InnerHost> for DetailsPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
                self.adopt(cx);
                fx.push(Fx::Deliver(
                    MachineId::Instance(InstanceId(0)),
                    Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                        focus: FocusTarget::Elem(FocusKey { entry: self.entry, elem: self.state.sel.0 }),
                    })),
                ));
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                let (settled, moved) = (self.poll(), self.adopt(cx));
                if settled || moved {
                    fx.invalidate(Provenance::Input);
                }
                self.form.table.update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
                self.follow.update(t.dt());
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
                let sentence = self.sentence();
                self.follow.follow(&sentence);
                Handled::Yes
            }
            ScreenEvent::Activate(key) => {
                self.activate(*key, fx);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

plx_ui::focusable_via_view!(DetailsPage, InnerHost, view);

impl Screen<InnerHost> for DetailsPage {
    /// One word per kind, so a scene that boots the Show picker is not satisfied by the Movie one.
    fn name(&self) -> &'static str {
        match self.kind {
            PosterKind::Movie => crate::registry::word::UNDER_POSTERS_MOVIE,
            PosterKind::Show => crate::registry::word::UNDER_POSTERS_SHOW,
        }
    }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::under_posters_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        // The picker chooses one kind's parts, so the picture is the same two tiles the page draws
        // with the other kind dimmed, the lines following each tick; the sort caption would hide
        // every choice behind the same words.
        let previewed = UnderPosters { sort_value: false, ..self.state.value };
        let posters = preview_posters(f.cx);
        let picture = RootPicture { value: previewed, posters: &posters, sorted: false, opacity: self.picture_opacity() };
        let mut v = self.view();
        v.header.picture = Some(&picture);
        plx_ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
}

impl DetailsPage {
    /// The picker's picture: the kind being chosen is at full strength, the other dimmed. The page below
    /// already drew exactly this for the focused details row, so the push moves nothing.
    fn picture_opacity(&self) -> [f32; 2] {
        match self.kind {
            PosterKind::Movie => [1.0, plx_ui::follow_copy::DIM],
            PosterKind::Show => [plx_ui::follow_copy::DIM, 1.0],
        }
    }
}

/// The `(focus key, destination)` of every `Nav` row the page lists: the structural navigation
/// test's expectation, built by the page's own form builder.
#[cfg(test)]
pub(super) fn nav_items_for_test() -> Vec<(u32, SettingsPage)> {
    let mut form: FormTable<UpId, UpAction, SettingsPage> = FormTable::new(super::super::registry::BAND);
    form.set(under_posters_form(UnderPosters::DEFAULT), None);
    (0..form.table.n_rows() as usize)
        .filter_map(|i| match form.binding_at(i)?.kind.clone() {
            RowKind::Nav(dest) => Some((form.key_at(i)?.0, dest)),
            _ => None,
        })
        .collect()
}

/// The focus key of a part's row in the picker.
#[cfg(test)]
pub(super) fn details_elem_for_test(part: DetailPart) -> u32 {
    PartId(part).key().0
}

#[cfg(test)]
#[path = "settings_under_posters_tests.rs"]
mod tests;
