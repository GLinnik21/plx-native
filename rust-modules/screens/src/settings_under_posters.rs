//! **Settings > Libraries > Under posters**: what the focused poster shows beneath it (Settings v2).
//!
//! One setting, saved per profile (`plx_plex::plex::session::UnderPosters`). The page holds five
//! rows: a Title switch, two pickers (Movie details and TV show details, each a pushed page,
//! [`DetailsPage`], with one checked choice per details line and a sample of what it shows), a Sort
//! value switch and Reset. The details are two rows because a TV show has no single video quality:
//! the movie picker lists Video quality, the show picker does not (and the stored show value can
//! never be Video quality, [`UnderPostersDetails::for_shows`]).
//!
//! **No panel and no sub-lines.** A row is its label and its value; the sentence that explained it
//! under its label is stated in the left column while the row has focus ([`sentence_for`], the
//! shared [`FollowCopy`](plx_ui::follow_copy::FollowCopy)), and a row with none shows the page's.
//! The picture under that sentence is the shared picture slot ([`RootPicture`], also drawn on the
//! Settings root): a movie poster and a show poster, each with the lines the stored setting gives
//! it. Each tile's art is the first poster of its kind from the widest cache the Browse store holds
//! ([`preview_posters`]); a kind with none anywhere is a finished art-less face
//! (`plx_ui::widgets::absent_card`), never a skeleton. The lines are worded by the SAME policy the Library, Collections, the Watchlist, Home and
//! Recommended read ([`crate::registry::captions`], [`plx_ui::caption_policy`]); a face with no
//! artwork carries no word, and a tile whose kind the library has no poster of is worded from a
//! sample of its kind. A line the setting turns on is never drawn empty, for any kind of tile (a
//! film or show with not even a year says what it is: "Movie", "Show").
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
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
use plx_ui::caption_policy::{CaptionPolicy, DetailsKind};
use plx_data::pms::PmsMovie;
use plx_ui::caption_policy::CaptionFacts;
use plx_ui::follow_copy::FollowCopy;
use plx_ui::widgets::Art;
use crate::registry::tile_facts;

/// The stored setting as the UI's caption policy. The mapping lives here, not in `plx_plex`: the
/// data layer must not name the UI layer.
pub fn caption_policy_of(v: UnderPosters) -> CaptionPolicy {
    let kind = |d| match d {
        UnderPostersDetails::YearLength => DetailsKind::YearLength,
        UnderPostersDetails::Genre => DetailsKind::Genre,
        UnderPostersDetails::Video => DetailsKind::Video,
        UnderPostersDetails::AgeRatingScore => DetailsKind::AgeRatingScore,
        UnderPostersDetails::None => DetailsKind::None,
    };
    CaptionPolicy { title: v.title, details: kind(v.details), show_details: kind(v.show_details) }
}

impl PosterKind {
    /// The stored details line for this kind.
    fn details(self, v: UnderPosters) -> UnderPostersDetails {
        match self {
            PosterKind::Movie => v.details,
            PosterKind::Show => v.show_details,
        }
    }

    /// `v` with this kind's details line set to `d` (a show can never hold Video quality).
    fn with_details(self, v: UnderPosters, d: UnderPostersDetails) -> UnderPosters {
        match self {
            PosterKind::Movie => UnderPosters { details: d, ..v },
            PosterKind::Show => UnderPosters { show_details: d.for_shows(), ..v },
        }
    }

    /// The choices this kind's picker lists, Nothing first, then the ladder's order.
    fn picker_order(self) -> impl Iterator<Item = UnderPostersDetails> {
        let ladder: &'static [UnderPostersDetails] = match self {
            PosterKind::Movie => &UnderPostersDetails::LADDER,
            PosterKind::Show => &UnderPostersDetails::SHOW_LADDER,
        };
        std::iter::once(UnderPostersDetails::None).chain(ladder.iter().copied().filter(|d| *d != UnderPostersDetails::None))
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

    /// The sentence beside that kind's picker.
    fn picker_copy(self) -> &'static str {
        match self {
            PosterKind::Movie => plx_platform::i18n::msg::under_posters_movie_details_copy(),
            PosterKind::Show => plx_platform::i18n::msg::under_posters_show_details_copy(),
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

/// A details line's name in a picker and on the page's row. A show's year line says its seasons.
fn details_label(kind: PosterKind, d: UnderPostersDetails) -> &'static str {
    use plx_platform::i18n::msg;
    match d {
        UnderPostersDetails::YearLength if kind == PosterKind::Show => msg::under_posters_year_seasons(),
        UnderPostersDetails::YearLength => msg::under_posters_year_length(),
        UnderPostersDetails::Genre => msg::under_posters_genre(),
        UnderPostersDetails::Video => msg::under_posters_video(),
        UnderPostersDetails::AgeRatingScore => msg::under_posters_age_rating_score(),
        UnderPostersDetails::None => msg::under_posters_none(),
    }
}

/// **The Settings root's value for the Under posters row**: the movie details line the page's Movie
/// details row reads ("Year · Length", "Genre"), the way Language reads its language (the board's
/// root shows one value; the show line is one level down). The Title switch adds a bold line above
/// it and is the page's own row; it shows here only when it is all there is ("Title"), so the value
/// is never "Nothing" beside a poster that wears its name.
pub(super) fn summary(v: UnderPosters) -> String {
    use plx_platform::i18n::msg;
    match (v.details, v.title) {
        (UnderPostersDetails::None, true) => msg::under_posters_title_row().to_string(),
        (details, _) => details_label(PosterKind::Movie, details).to_string(),
    }
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
            Row::new(kind.row_label()).value(details_label(kind, kind.details(v))).chevron(true))
    });
    let rows = rows.item(UpId::SortValue, RowKind::Toggle, UpAction::SortValue,
        Row::new(msg::under_posters_sort_value()).toggle(v.sort_value));
    let reset = FormSection::new("").item(
        UpId::Reset,
        RowKind::Button,
        UpAction::Reset,
        Row::new(msg::under_posters_reset()).dim(v == UnderPosters::DEFAULT),
    );
    Form::new().section(rows).section(reset)
}

pub(super) struct UnderPostersPage {
    entry: EntryId,
    form: FormTable<UpId, UpAction, SettingsPage>,
    state: UpState,
    /// The sentence beside the table, which fades to the focused row's.
    follow: FollowCopy,
    /// A pick not yet answered: the value the rows show meanwhile, and the receipt of its write.
    pending: Option<(UnderPosters, std::sync::mpsc::Receiver<bool>)>,
}

struct UpState {
    value: UnderPosters,
    /// The focused row's key — an identity, not a position.
    sel: RowKey,
    busy: bool,
}

impl LogicalState for UpState {
    fn write(&self, w: &mut Canon) {
        w.u32(self.sel.0).u8(self.value.pack()).bool(self.busy);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "under_posters sel={} title={} details={} show_details={} busy={} sort_value={}",
            self.sel.0, self.value.title, self.value.details.index(), self.value.show_details.index(), self.busy, self.value.sort_value
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
            pending: None,
        };
        page.rebuild();
        // A page opens on its first row's sentence at once, with nothing fading.
        page.follow = FollowCopy::new(sentence_for(page.form.selected_id()));
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

/// The facts a picture tile of `kind` is worded from: its poster's own, else the sample a movie or
/// a show wears.
fn picture_facts(kind: PosterKind, poster: Option<&PmsMovie>) -> CaptionFacts {
    poster.map_or_else(
        || match kind {
            PosterKind::Movie => CaptionFacts { year: Some(2024), duration_min: Some(112), is_movie: true, ..sample_facts() },
            PosterKind::Show => CaptionFacts {
                title: Some(plx_platform::i18n::msg::browse_kind_show_in(plx_platform::i18n::current()).to_string()),
                year: Some(SAMPLE_YEAR), duration_min: None, season_count: Some(6), is_show: true,
                content_rating: Some("TV-14".to_string()), audience_rating: Some(8.4), ..sample_facts()
            },
        },
        tile_facts::caption_facts,
    )
}

/// The first of `posters` that is a title of `kind`.
fn poster_of<'a>(kind: PosterKind, posters: &[&'a PmsMovie]) -> Option<&'a PmsMovie> {
    posters.iter().copied().find(|p| kind.is_kind_of(p))
}

/// **The ONE source of every sample on the Under posters pages**: the facts of the tile a picker
/// words, so a row never promises "2019 · 2h 14m" over a picture that says "2022 · 6m".
fn kind_facts(kind: PosterKind, posters: &[&PmsMovie]) -> CaptionFacts {
    picture_facts(kind, poster_of(kind, posters))
}

/// The lines under a tile: `[title, details]` as `v` words them from `facts`, through the one
/// builder every poster surface uses ([`crate::registry::captions::label`]), so the picture cannot
/// word a caption differently from the screens it previews.
fn lines_of(v: UnderPosters, facts: &CaptionFacts) -> [Option<String>; 2] {
    let label = crate::registry::captions::label(&caption_policy_of(v), facts, None, false);
    let text = |line: &Option<std::ffi::CString>| line.as_ref().map(|line| line.to_string_lossy().into_owned());
    [text(&label.title), text(&label.caption)]
}

/// The lines under the picture's tile of `kind`: the caption the setting gives it, or (the Sort
/// value row has focus, `sorted`) the year a library sorted by Year would print there.
fn picture_lines(v: UnderPosters, kind: PosterKind, poster: Option<&PmsMovie>, sorted: bool) -> [Option<String>; 2] {
    if sorted {
        let year = poster.map_or(SAMPLE_YEAR, |p| p.year);
        return [None, Some(crate::registry::sort_info::year_value(year).text)];
    }
    lines_of(v, &picture_facts(kind, poster))
}

/// The Under posters picture (board `Sv2-2`): one art-only poster tile per kind, each with the
/// lines the stored setting gives it centred under it, painted INTO the picture slot. No panel,
/// ring, tick or label: it is display-only and never reads as a control. The Settings root draws
/// the same picture, so root and child agree; a details picker draws the one tile of its kind.
pub(super) struct RootPicture<'a> {
    pub value: UnderPosters,
    pub posters: &'a [&'a PmsMovie],
    /// The kinds drawn, left to right.
    pub kinds: &'a [PosterKind],
    /// Word each tile with its poster's year rather than the caption.
    pub sorted: bool,
}

/// The board's picture tiles: 200 x 300 posters, their left edges 240 apart.
const PICTURE_TILE_W: f32 = 200.0;
const PICTURE_TILE_H: f32 = 300.0;
const PICTURE_TILE_STEP: f32 = 240.0;
/// The first line's CSS box under a tile: 24 px on a 24 px line, its top 312 px under the tile's;
/// the second line (the details, under a title) hangs one line and a gap under it.
const PICTURE_LINE_TOP: f32 = 312.0;
const PICTURE_LINE_H: f32 = 24.0;
const PICTURE_LINE_STEP: f32 = 32.0;
/// The room a line may take: a tile's width and the air up to its neighbour's.
const PICTURE_LINE_W: f32 = PICTURE_TILE_STEP - 16.0;

impl plx_ui::follow_copy::PictureSlot for RootPicture<'_> {
    fn draw(&self, p: Painter, slot: Rect, measure: &dyn plx_machine::machine::Measure) {
        use plx_ui::label::{HAlign, Label, VAlign};
        let size = theme::size::CAPTION;
        // The art of a tile is the shared one, with no watch state (the fan's own rule). A kind with
        // no poster path anywhere is `None`: drawn as the art-less face, never as `Art::Poster(None)`,
        // which is a skeleton (a card that is still loading, and for an empty key one that never will).
        let arts: Vec<Option<Art>> = self.kinds.iter()
            .map(|k| poster_of(*k, self.posters).filter(|m| !m.thumb.is_empty()).map(|m| Art::Poster(Some(tile_facts::of(m)))))
            .collect();
        let line_cap_top = plx_gfx::text::css_cap_drop(size, PICTURE_LINE_H, measure.cap_h(size));
        for (i, kind) in self.kinds.iter().enumerate() {
            let x = slot.x + PICTURE_TILE_STEP * i as f32;
            let tile = Rect::new(x, slot.y, PICTURE_TILE_W, PICTURE_TILE_H);
            match arts[i] {
                Some(art) => plx_ui::widgets::card(p, tile, plx_ui::follow_copy::PosterFan::stateless(art), theme::CARD_RING_RAD, false, 1.0, 0.0),
                None => plx_ui::widgets::absent_card(p, tile, theme::CARD_RING_RAD),
            }
            let lines = picture_lines(self.value, *kind, poster_of(*kind, self.posters), self.sorted);
            // Present lines stack from the first: a title (bold) over its details.
            for (n, (line, is_title)) in lines.into_iter().zip([true, false]).filter_map(|(l, t)| l.map(|l| (l, t))).enumerate() {
                let line = plx_gfx::text::elide_by(&line, PICTURE_LINE_W, false, |t| measure.width_str(t, size, is_title));
                let Ok(line) = std::ffi::CString::new(line) else { continue };
                let mut label = Label::new(line.as_ptr(), size, if is_title { theme::TEXT_PRIMARY } else { theme::CAPTION_DETAIL_INK }).h(HAlign::Center).v(VAlign::CapTop);
                if is_title {
                    label = label.bold();
                }
                label.draw(
                    p,
                    Rect::new(x + (PICTURE_TILE_W - PICTURE_LINE_W) * 0.5,
                        slot.y + PICTURE_LINE_TOP + PICTURE_LINE_STEP * n as f32 + line_cap_top, PICTURE_LINE_W, PICTURE_LINE_H),
                );
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

impl Machine<InnerHost> for UnderPostersPage {
    type Ev = ScreenEvent<InnerHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, InnerHost>, fx: &mut Effects<'_, InnerHost>) -> Handled {
        match ev {
            ScreenEvent::Mount => {
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
                if self.poll() {
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
                self.follow.follow(sentence_for(self.form.selected_id()));
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
    fn name(&self) -> &'static str { "under-posters" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::settings_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        let posters = preview_posters(f.cx);
        let picture = RootPicture {
            value: self.state.value, posters: &posters, kinds: &[PosterKind::Movie, PosterKind::Show], sorted: self.previews_sort_value(),
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

/// A details choice's row key: its place on the ladder, clear of the first page's rows.
#[derive(Clone, PartialEq, Eq, Debug)]
struct DetailsId(UnderPostersDetails);

impl FormId for DetailsId {
    fn key(&self) -> RowKey {
        RowKey(10 + self.0.index() as u32)
    }
}

/// What a choice shows under a poster, as the picture words it from `facts` ([`kind_facts`]): the
/// row's sample on the right. `None` for Nothing; a kind the poster has no fact for samples the identity
/// line the tile says instead (its year), as the picture draws it.
fn details_sample(d: UnderPostersDetails, facts: &CaptionFacts) -> Option<String> {
    lines_of(UnderPosters { title: false, details: d, show_details: d, sort_value: false }, facts)[1].clone()
}

/// The picker's rows: one checked choice per details line, the ladder's order, a sample of each
/// on the right, all from the picture's tile of that kind.
fn details_form(kind: PosterKind, current: UnderPostersDetails, facts: &CaptionFacts) -> Form<DetailsId, UnderPostersDetails, SettingsPage> {
    let mut section = FormSection::new("");
    for d in kind.picker_order() {
        let mut row = Row::new(details_label(kind, d));
        if let Some(sample) = details_sample(d, facts) {
            row = row.value(sample);
        }
        section = section.choice(DetailsId(d), d, row, |id| id.0 == current);
    }
    Form::new().section(section)
}

pub(super) struct DetailsPage {
    entry: EntryId,
    kind: PosterKind,
    form: FormTable<DetailsId, UnderPostersDetails, SettingsPage>,
    state: DetailsState,
    /// What the rows' samples are worded from: the picture's tile of this kind ([`kind_facts`]),
    /// refreshed whenever the library's cached posters change.
    facts: CaptionFacts,
    /// A pick not yet answered, and the receipt of its write: the page pops when it is durable.
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
    /// The line the focused row stands for: what the picture shows.
    fn focused(&self) -> UnderPostersDetails {
        UnderPostersDetails::LADDER.into_iter().find(|d| DetailsId(*d).key() == self.sel).unwrap_or(self.kind.details(self.value))
    }
}

impl LogicalState for DetailsState {
    fn write(&self, w: &mut Canon) {
        w.u8(self.kind as u8).u32(self.sel.0).u8(self.value.pack()).bool(self.busy);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "under_posters_details kind={} sel={} details={} busy={}",
            self.kind as u8, self.sel.0, self.kind.details(self.value).index(), self.busy
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
            state: DetailsState { kind, value, sel: DetailsId(kind.details(value)).key(), busy: false },
            facts: picture_facts(kind, None),
            pending: None,
        };
        page.rebuild();
        page
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
        let current = self.kind.details(self.state.value);
        let keep = self.form.selected_id().cloned().unwrap_or(DetailsId(current));
        self.form.table.compact = false;
        self.form.table.board_metrics(true);
        self.form.set(details_form(self.kind, current, &self.facts), Some(&keep));
        self.form.table.list_focused = true;
    }

    fn view(&self) -> TableScreen<'_> {
        use plx_platform::i18n::msg;
        TableScreen::new(
            Header::new(RouteLayout::settings_page(), Some(msg::under_posters_title()), self.kind.row_label(), self.kind.picker_copy()),
            &self.form.table, GroupId(0), self.entry,
        ).keyed(&self.form)
    }

    fn pop(fx: &mut Effects<'_, InnerHost>) {
        fx.push(Fx::Nav(plx_machine::machine::NavOp::Pop));
        fx.invalidate(Provenance::Input);
    }

    /// Choosing the checked line pops without a write; any other goes through the one write door
    /// and the page pops when the receipt answers durable.
    fn activate(&mut self, key: u32, fx: &mut Effects<'_, InnerHost>) {
        if self.state.busy {
            return;
        }
        let Some(details) = form_activate(&self.form, key, fx) else { return };
        if details == self.kind.details(self.state.value) {
            Self::pop(fx);
            return;
        }
        let next = self.kind.with_details(self.state.value, details);
        let (reply, receipt) = std::sync::mpsc::channel();
        fx.push(Fx::App(AppFx::Preferences(super::super::registry::PreferenceCmd::Choose(
            plx_media::route::Choice::UnderPosters(next), reply))));
        self.pending = Some((next, receipt));
        self.state.value = next;
        self.state.busy = true;
        self.rebuild();
    }

    /// Settle an answered write: a durable one pops, a refused one puts the checkmark back.
    fn poll(&mut self, fx: &mut Effects<'_, InnerHost>) -> bool {
        let Some((_, receipt)) = &self.pending else { return false };
        match receipt.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
        }
        let wrote = self.pending.take().map(|(next, _)| next);
        self.state.busy = false;
        self.state.value = plx_media::route::under_posters();
        self.rebuild();
        if wrote == Some(self.state.value) {
            Self::pop(fx);
        }
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
                let (settled, moved) = (self.poll(fx), self.adopt(cx));
                if settled || moved {
                    fx.invalidate(Provenance::Input);
                }
                self.form.table.update(t.dt(), RouteLayout::settings_page().sectioned_table().h);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { to, .. } => {
                form_focus(&mut self.form, to.elem);
                if let Some(key) = self.form.key_at(self.form.table.sel.max(0) as usize) {
                    self.state.sel = key;
                }
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

plx_ui::focusable_via_view!(DetailsPage, InnerHost, view);

impl Screen<InnerHost> for DetailsPage {
    fn name(&self) -> &'static str { "under-posters-details" }
    fn state(&self) -> &dyn LogicalState { &self.state }
    fn crumb(&self, _cx: &Cx<'_, InnerHost>) -> Option<Cow<'_, str>> { Some(Cow::Borrowed(plx_platform::i18n::msg::under_posters_title())) }
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, InnerHost>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, InnerHost>) {
        // The picker chooses one kind's details line, so its picture is the one tile of that kind
        // wearing the focused choice: the sort caption would hide every choice behind the same words.
        let previewed = UnderPosters { sort_value: false, ..self.kind.with_details(self.state.value, self.state.focused()) };
        let posters = preview_posters(f.cx);
        let picture = RootPicture { value: previewed, posters: &posters, kinds: &[self.kind], sorted: false };
        let mut v = self.view();
        v.header.picture = Some(&picture);
        plx_ui::screen::Part::<InnerHost>::draw(&mut v, f, Rect::FULL);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
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

/// The focus key of a details line's row in the picker.
#[cfg(test)]
pub(super) fn details_elem_for_test(d: UnderPostersDetails) -> u32 {
    DetailsId(d).key().0
}

#[cfg(test)]
#[path = "settings_under_posters_tests.rs"]
mod tests;
